use crate::{error::DefenderError, traits::DefenderClient, types::DefenderSubmission};
use alloy_primitives::{Address, BlockNumber, Bytes, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use world_chain_proofs::{
    GameCreated, IDisputeGameFactory, IWorldChainProofSystemGame, ProposalCommitment, RootState,
    WIP_1006_GAME_TYPE,
};

/// Alloy-backed implementation of [`DefenderClient`].
#[derive(Debug, Clone)]
pub struct AlloyDefenderClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    provider: P,
}

impl<P> AlloyDefenderClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address) -> Self {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );

        Self { factory, provider }
    }
}

#[async_trait]
impl<P> DefenderClient for AlloyDefenderClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn root_state(&self, game: Address) -> Result<RootState, DefenderError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let root_state_raw = game
            .state()
            .call()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        let root_state: RootState = root_state_raw.try_into()?;
        Ok(root_state)
    }

    async fn finalized_l1_block_num(&self) -> Result<BlockNumber, DefenderError> {
        self.provider
            .get_block(BlockId::finalized())
            .await
            .map_err(|err| DefenderError::Rpc(err.to_string()))?
            .map(|block| block.number())
            .ok_or(DefenderError::L1FinalizedBlockNotFound)
    }

    async fn games_created(
        &self,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<Vec<GameCreated>, DefenderError> {
        let logs = self
            .factory
            .DisputeGameCreated_filter()
            .from_block(from)
            .to_block(to)
            .query()
            .await
            .map_err(|err| DefenderError::Rpc(err.to_string()))?;

        let mut games = Vec::new();
        for (event, _log) in logs {
            if event.gameType != WIP_1006_GAME_TYPE {
                continue;
            }
            let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
                event.disputeProxy,
                self.provider.clone(),
            );
            let root_id_call = game.rootId();
            let game_creator_call = game.gameCreator();
            let parent_ref_call = game.parentRef();
            let l2_block_number_call = game.l2SequenceNumber();
            let l1_origin_hash_call = game.l1Head();
            let l1_origin_number_call = game.l1OriginNumber();
            let domain_hash_call = game.domainHash();
            let (
                root_id,
                game_creator,
                parent_ref,
                l2_block_number,
                l1_origin_hash,
                l1_origin_number,
                domain_hash,
            ) = tokio::try_join!(
                root_id_call.call(),
                game_creator_call.call(),
                parent_ref_call.call(),
                l2_block_number_call.call(),
                l1_origin_hash_call.call(),
                l1_origin_number_call.call(),
                domain_hash_call.call(),
            )
            .map_err(|error| DefenderError::Contract(error.to_string()))?;
            let l2_block_number = u256_to_u64(l2_block_number, "l2SequenceNumber")?;
            games.push(GameCreated {
                transition_key: ProposalCommitment {
                    parent_ref,
                    root_claim: event.rootClaim,
                    l2_block_number,
                }
                .transition_key(domain_hash),
                root_id,
                game: event.disputeProxy,
                game_creator,
                root_claim: event.rootClaim,
                l2_block_number,
                parent_ref,
                l1_origin_hash,
                l1_origin_number: u256_to_u64(l1_origin_number, "l1OriginNumber")?,
            });
        }
        Ok(games)
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, DefenderError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let challenge_deadline = game
            .challengeDeadline()
            .call()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        Ok(challenge_deadline)
    }

    async fn proof_bitmap(&self, game: Address) -> Result<u8, DefenderError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let proof_bitmap = game
            .proofBitmap()
            .call()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        Ok(proof_bitmap)
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let pending = game
            .submitProofLane(lane, proof)
            .send()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|err| DefenderError::Contract(err.to_string()))?;
        if !receipt.status() {
            return Err(DefenderError::Revert(tx_hash));
        }
        Ok(DefenderSubmission { tx_hash })
    }
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, DefenderError> {
    value
        .try_into()
        .map_err(|_| DefenderError::Contract(format!("{field} overflows u64")))
}
