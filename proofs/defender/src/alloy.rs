use crate::{error::DefenderError, traits::DefenderClient, types::DefenderSubmission};
use alloy_primitives::{Address, BlockNumber, Bytes, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use world_chain_proofs::{GameCreated, IDisputeGameFactory, IWorldChainProofSystemGame, RootState};

/// Alloy-backed implementation of [`DefenderClient`].
#[derive(Debug, Clone)]
pub struct AlloyDefenderClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    game_type: u32,
    provider: P,
}

impl<P> AlloyDefenderClient<P>
where
    P: Provider + Clone,
{
    /// Creates a new Alloy-backed contract client.
    pub fn new(provider: P, factory_address: Address, game_type: u32) -> Self {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );

        Self {
            factory,
            game_type,
            provider,
        }
    }

    fn game_instance(
        &self,
        game: Address,
    ) -> IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance<P> {
        IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        )
    }

    /// Reads the proposal context of a factory-created game.
    ///
    /// Discovery filters the trusted factory's `DisputeGameCreated` event, so every game read
    /// here is a genuine `WorldChainProofSystemGame`; the rich per-proposal fields live on the
    /// game itself rather than in the factory event.
    async fn read_game_created(
        &self,
        game_address: Address,
        root_claim: alloy_primitives::B256,
    ) -> Result<GameCreated, DefenderError> {
        let game = self.game_instance(game_address);
        macro_rules! call {
            ($method:ident) => {
                game.$method()
                    .call()
                    .await
                    .map_err(|err| DefenderError::Contract(err.to_string()))?
            };
        }

        Ok(GameCreated {
            root_id: call!(rootId),
            game: game_address,
            proposer: call!(gameCreator),
            root_claim,
            l2_block_number: u256_to_u64(call!(l2BlockNumber), "l2BlockNumber")?,
            parent_ref: call!(parentRef),
            l1_origin_hash: call!(l1OriginHash),
            l1_origin_number: u256_to_u64(call!(l1OriginNumber), "l1OriginNumber")?,
            attempt: call!(attempt),
        })
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
            .topic2(U256::from(self.game_type))
            .from_block(from)
            .to_block(to)
            .query()
            .await
            .map_err(|err| DefenderError::Rpc(err.to_string()))?;

        let mut games = Vec::with_capacity(logs.len());
        for (event, _log) in logs {
            games.push(
                self.read_game_created(event.disputeProxy, event.rootClaim)
                    .await?,
            );
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
