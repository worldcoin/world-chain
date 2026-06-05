use crate::{
    config::ChallengerConfig, error::ChallengerError, traits::ChallengerClient, types::RootState,
};
use alloy_primitives::BlockNumber;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;
use world_chain_proofs::ConsensusProvider;

/// The number of L1 blocks published in 24h.
const ONE_DAY_OF_L1_BLOCKS: u64 = 7_200;
/// The safety margin of blocks.
const MARGIN: u64 = 500;

/// World Chain Challenger.
#[derive(Debug)]
pub struct WorldChainChallenger<E, C> {
    config: ChallengerConfig,
    execution_provider: E,
    consensus_provider: C,
    cursor: BlockNumber,
}

impl<E, C> WorldChainChallenger<E, C> {
    /// Creates a challenger from contract and output-root clients.
    pub const fn new(
        config: ChallengerConfig,
        execution_provider: E,
        consensus_provider: C,
    ) -> Self {
        Self {
            config,
            execution_provider,
            consensus_provider,
            cursor: 0,
        }
    }

    /// Returns the challenger configuration.
    #[must_use]
    pub const fn config(&self) -> &ChallengerConfig {
        &self.config
    }
}

impl<E, C> WorldChainChallenger<E, C>
where
    E: ChallengerClient,
    C: ConsensusProvider,
{
    pub async fn scan_once(&mut self) -> Result<(), ChallengerError> {
        let target = self.execution_provider.finalized_l1_block_num().await?;
        let from = if self.cursor == 0 {
            target.saturating_sub(ONE_DAY_OF_L1_BLOCKS + MARGIN)
        } else {
            self.cursor
        };
        // short circuit if from > target: it means that there are no
        // new L1 finalized blocks compared to last scan
        if from > target {
            return Ok(());
        }
        let game_created = self.execution_provider.games_created(from, target).await?;
        for game_created in game_created {
            let game = game_created.game;
            let root_state = self.execution_provider.root_state(game).await?;
            if root_state != RootState::Proposed {
                // root state is not `Proposed` anymore, skip immediately
                continue;
            }
            let challenge_deadline = self.execution_provider.challenge_deadline(game).await?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_secs();

            if now >= challenge_deadline {
                // challenge deadline has expired, skip immediately
                continue;
            }
            // ensure that the l2 block is finalized
            let latest_finalized_l2_block =
                self.consensus_provider.latest_l2_finalized_block().await?;
            if game_created.l2_block_number > latest_finalized_l2_block {
                return Err(ChallengerError::L2BlockNotFinalized {
                    game,
                    latest_finalized: latest_finalized_l2_block,
                    given_block: game_created.l2_block_number,
                });
            }

            match self
                .consensus_provider
                .output_root_at_block(game_created.l2_block_number)
                .await
            {
                Ok(root) if root != game_created.root_claim => {
                    self.execution_provider
                        .submit_challenge(game, self.config.challenger_bond)
                        .await?;
                }
                Ok(_root) => {
                    // valid root, leave it
                }
                Err(err) => return Err(ChallengerError::OutputRoot(err)),
            }
        }
        // if the scan goes well, update the cursor
        self.cursor = target + 1;
        Ok(())
    }

    /// Runs the challenger forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&mut self) -> Result<(), ChallengerError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(e) = self.scan_once().await {
                warn!(%e, "scan attempt failed");
            }
        }
    }
}
