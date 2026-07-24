use crate::{
    error::DefenderError,
    traits::DefenderClient,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, Bytes, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::BlockId;
use async_trait::async_trait;
use world_chain_proofs::{
    IWorldChainProofSystemFactory, IWorldChainProofSystemGame, ResolutionStatus,
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

    fn game(
        &self,
        address: Address,
    ) -> IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance<P> {
        IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            address,
            self.provider.clone(),
        )
    }
}

#[async_trait]
impl<P> DefenderClient for AlloyDefenderClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn game_count(&self) -> Result<u64, DefenderError> {
        let count = self
            .factory
            .gameCount()
            .block(BlockId::finalized())
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))?;
        u256_to_u64(count, "gameCount")
    }

    async fn game_address_at(&self, index: u64) -> Result<Address, DefenderError> {
        self.factory
            .gameAt(U256::from(index))
            .block(BlockId::finalized())
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))
    }

    async fn game_proposer(&self, address: Address) -> Result<Address, DefenderError> {
        self.game(address)
            .proposer()
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))
    }

    async fn game_metadata(&self, address: Address) -> Result<GameMetadata, DefenderError> {
        let game = self.game(address);
        let (root_claim, l2_block_number, l1_origin_hash, challenge_deadline, proof_deadline) = tokio::try_join!(
            async {
                game.rootClaim()
                    .call()
                    .await
                    .map_err(|error| DefenderError::Contract(error.to_string()))
            },
            async {
                game.l2BlockNumber()
                    .call()
                    .await
                    .map_err(|error| DefenderError::Contract(error.to_string()))
            },
            async {
                game.l1OriginHash()
                    .call()
                    .await
                    .map_err(|error| DefenderError::Contract(error.to_string()))
            },
            async {
                game.challengeDeadline()
                    .call()
                    .await
                    .map_err(|error| DefenderError::Contract(error.to_string()))
            },
            async {
                game.proofDeadline()
                    .call()
                    .await
                    .map_err(|error| DefenderError::Contract(error.to_string()))
            },
        )?;

        Ok(GameMetadata {
            address,
            root_claim,
            l2_block_number: u256_to_u64(l2_block_number, "l2BlockNumber")?,
            l1_origin_hash,
            challenge_deadline,
            proof_deadline,
        })
    }

    async fn proof_deadline(&self, address: Address) -> Result<u64, DefenderError> {
        self.game(address)
            .proofDeadline()
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))
    }

    async fn resolution_status(&self, address: Address) -> Result<ResolutionStatus, DefenderError> {
        let result = self
            .game(address)
            .resolutionStatus()
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))?;
        let root_state = result.outcome.try_into()?;
        let invalidation_reason = result.reason.try_into()?;

        Ok(ResolutionStatus {
            resolvable: result.resolvable,
            root_state,
            invalidation_reason,
        })
    }

    async fn proof_bitmap(&self, address: Address) -> Result<u8, DefenderError> {
        self.game(address)
            .proofBitmap()
            .call()
            .await
            .map_err(|error| DefenderError::Contract(error.to_string()))
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
        let pending = self
            .game(game)
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
