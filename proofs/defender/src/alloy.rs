use crate::{error::DefenderError, traits::DefenderClient, types::DefenderSubmission};
use alloy_primitives::{Address, BlockNumber, Bytes, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use world_chain_proofs::{
    GameCreated, IWorldChainProofSystemFactory, IWorldChainProofSystemGame, RootState,
};

/// Alloy-backed implementation of [`DefenderClient`].
#[derive(Debug, Clone)]
pub struct AlloyDefenderClient<P> {
    factory: IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance<P>,
    provider: P,
}

impl<P> AlloyDefenderClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address) -> Self {
        let factory = IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance::new(
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
            .GameCreated_filter()
            .from_block(from)
            .to_block(to)
            .query()
            .await
            .map_err(|err| DefenderError::Rpc(err.to_string()))?;

        logs.into_iter()
            .map(|(event, _log)| {
                Ok(GameCreated {
                    proposal_key: event.proposalKey,
                    root_id: event.rootId,
                    game: event.game,
                    proposer: event.proposer,
                    root_claim: event.rootClaim,
                    l2_block_number: u256_to_u64(event.l2BlockNumber, "l2BlockNumber")?,
                    parent_ref: event.parentRef,
                    l1_origin_hash: event.l1OriginHash,
                    l1_origin_number: u256_to_u64(event.l1OriginNumber, "l1OriginNumber")?,
                })
            })
            .collect()
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
