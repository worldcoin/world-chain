use alloy_primitives::{Address, B256, BlockNumber, U256};
use alloy_provider::Provider;
use async_trait::async_trait;
use world_chain_proofs::{IWorldChainProofSystemFactory, IWorldChainProofSystemGame};

use crate::{
    error::ChallengerError,
    traits::ChallengerClient,
    types::{ChallengeSubmission, GameCreated, RootState},
};

/// Alloy-backed implementation of [`ChallengerClient`].
#[derive(Debug, Clone)]
pub struct AlloyChallengerClient<P> {
    factory: IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance<P>,
    provider: P,
}

impl<P> AlloyChallengerClient<P>
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
impl<P> ChallengerClient for AlloyChallengerClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn root_state(&self, game: Address) -> Result<RootState, ChallengerError> {
        let game = IWorldChainProofSystemGame::IWorldChainProofSystemGameInstance::new(
            game,
            self.provider.clone(),
        );
        let root_state_raw = game
            .state()
            .call()
            .await
            .map_err(|err| ChallengerError::Contract(err.to_string()))?;
        let root_state: RootState = root_state_raw.try_into()?;
        Ok(root_state)
    }

    async fn finalized_l1_block_num(&self) -> Result<BlockNumber, ChallengerError> {}

    async fn games_created(
        &self,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<Vec<GameCreated>, ChallengerError> {
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError> {}

    async fn submit_challenge(
        &self,
        game: Address,
        challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError> {
    }
}

fn u256_to_u64(value: U256, field: &'static str) -> Result<u64, ChallengerError> {
    value
        .try_into()
        .map_err(|_| ChallengerError::Contract(format!("{field} overflows u64")))
}
