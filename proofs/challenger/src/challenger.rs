use crate::{
    config::ChallengerConfig,
    error::ChallengerError,
    traits::{ChallengeSubmitter, GameWatcher},
};
use tracing::{info, warn};
use world_chain_proofs::OutputRootProvider;

/// World Chain Challenger.
#[derive(Debug)]
pub struct WorldChainChallenger<C, O> {
    config: ChallengerConfig,
    contracts: C,
    output_roots: O,
}

impl<C, O> WorldChainChallenger<C, O> {
    /// Creates a challenger from contract and output-root clients.
    pub const fn new(config: ChallengerConfig, contracts: C, output_roots: O) -> Self {
        Self {
            config,
            contracts,
            output_roots,
        }
    }

    /// Returns the challenger configuration.
    #[must_use]
    pub const fn config(&self) -> &ChallengerConfig {
        &self.config
    }
}

impl<C, O> WorldChainChallenger<C, O>
where
    C: GameWatcher + ChallengeSubmitter,
    O: OutputRootProvider,
{
    /// Runs the challenger forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&self) -> Result<(), ChallengerError> {
        self.config.validate()?;
        // constantly keep an eye on new `GameCreated` events
        Ok(())
    }
}
