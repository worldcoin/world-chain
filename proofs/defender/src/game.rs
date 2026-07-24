use crate::{error::DefenderError, traits::DefenderClient, types::GameMetadata};
use alloy_primitives::BlockNumber;
use tracing::{error, info, warn};
use world_chain_proofs::{ConsensusProvider, InvalidationReason, RootState, proof_count};

/// On-chain state relevant to the defender for a single game.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GameObservation {
    Proposed,
    Challenged {
        proof_bitmap: u8,
        has_required_support: bool,
    },
    Finalized,
    Invalidated(InvalidationReason),
    Unset,
}

/// Result of scanning a newly discovered allowlisted game.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GameScanOutcome {
    /// Retain the game for monitoring.
    Track,
    /// The game does not need defense monitoring.
    Skip,
}

/// Result of watching a single game for one tick.
#[derive(Debug, Clone, Copy)]
pub(crate) enum WatchOutcome {
    /// Keep watching the game.
    Keep,
    /// The game was challenged and its root is valid: start a defense.
    Defend,
    /// The game no longer needs watching.
    Drop,
}

pub(crate) struct GameEvaluator<'a, E, C> {
    execution_client: &'a E,
    consensus_provider: &'a C,
}

impl<'a, E, C> Clone for GameEvaluator<'a, E, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E, C> Copy for GameEvaluator<'_, E, C> {}

impl<'a, E, C> GameEvaluator<'a, E, C>
where
    E: DefenderClient,
    C: ConsensusProvider,
{
    pub(crate) const fn new(execution_client: &'a E, consensus_provider: &'a C) -> Self {
        Self {
            execution_client,
            consensus_provider,
        }
    }

    pub(crate) async fn observe(
        &self,
        game: &GameMetadata,
    ) -> Result<GameObservation, DefenderError> {
        let status = self
            .execution_client
            .resolution_status(game.address)
            .await?;
        Ok(match status.root_state {
            RootState::Proposed => GameObservation::Proposed,
            RootState::Challenged => {
                let proof_bitmap = self.execution_client.proof_bitmap(game.address).await?;
                GameObservation::Challenged {
                    proof_bitmap,
                    has_required_support: has_required_proof_support(game, proof_bitmap),
                }
            }
            RootState::Finalized => GameObservation::Finalized,
            RootState::Invalidated => GameObservation::Invalidated(status.invalidation_reason),
            RootState::None => GameObservation::Unset,
        })
    }

    pub(crate) async fn scan(
        &self,
        game: &GameMetadata,
        now: u64,
    ) -> Result<GameScanOutcome, DefenderError> {
        match self.observe(game).await? {
            GameObservation::Proposed => {
                if now < game.challenge_deadline {
                    Ok(GameScanOutcome::Track)
                } else {
                    Ok(GameScanOutcome::Skip)
                }
            }
            GameObservation::Challenged {
                proof_bitmap,
                has_required_support,
            } => {
                if has_required_support {
                    info!(
                        game = %game.address,
                        proof_bitmap,
                        proof_threshold = game.proof_threshold,
                        "challenged game already has sufficient proof support"
                    );
                    return Ok(GameScanOutcome::Skip);
                }
                if now < game.proof_deadline {
                    return Ok(GameScanOutcome::Track);
                }

                error!(
                    game = %game.address,
                    proof_deadline = game.proof_deadline,
                    "challenged game proof deadline elapsed before defense completed"
                );
                Ok(GameScanOutcome::Skip)
            }
            GameObservation::Finalized => {
                info!(
                    game = %game.address,
                    "game already has a positive resolution outcome"
                );
                Ok(GameScanOutcome::Skip)
            }
            GameObservation::Invalidated(reason) => {
                if reason == InvalidationReason::ProofTimeout {
                    error!(
                        game = %game.address,
                        proof_deadline = game.proof_deadline,
                        "challenged game proof deadline elapsed before defense completed"
                    );
                } else {
                    warn!(
                        game = %game.address,
                        reason = ?reason,
                        "game is invalidatable without defense"
                    );
                }
                Ok(GameScanOutcome::Skip)
            }
            GameObservation::Unset => {
                error!(game = %game.address, "factory game has unset root state");
                Ok(GameScanOutcome::Skip)
            }
        }
    }

    pub(crate) async fn watch(
        &self,
        game: &GameMetadata,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Result<WatchOutcome, DefenderError> {
        let address = game.address;
        match self.observe(game).await? {
            GameObservation::Proposed => {
                if now >= game.challenge_deadline {
                    // the game can no longer be challenged
                    return Ok(WatchOutcome::Drop);
                }
                Ok(WatchOutcome::Keep)
            }
            GameObservation::Challenged {
                proof_bitmap,
                has_required_support,
            } => {
                if has_required_support {
                    info!(
                        game = %address,
                        proof_bitmap,
                        proof_threshold = game.proof_threshold,
                        "challenged game already has sufficient proof support"
                    );
                    return Ok(WatchOutcome::Drop);
                }
                if now >= game.proof_deadline {
                    error!(
                        game = %address,
                        proof_deadline = game.proof_deadline,
                        "challenged game proof deadline elapsed before defense completed"
                    );
                    return Ok(WatchOutcome::Drop);
                }

                // only judge the root against finalized L2 state
                if game.l2_block_number > latest_finalized_l2_block {
                    return Ok(WatchOutcome::Keep);
                }
                let root = self
                    .consensus_provider
                    .output_root_at_block(game.l2_block_number)
                    .await?;
                if root == game.root_claim {
                    Ok(WatchOutcome::Defend)
                } else {
                    error!(
                        game = %address,
                        claimed_root = %game.root_claim,
                        canonical_root = %root,
                        "allowlisted proposer published a non-canonical root; refusing to defend"
                    );
                    Ok(WatchOutcome::Drop)
                }
            }
            GameObservation::Finalized => {
                info!(game = %address, "game has a positive resolution outcome");
                Ok(WatchOutcome::Drop)
            }
            GameObservation::Invalidated(reason) => {
                if reason == InvalidationReason::ProofTimeout {
                    error!(
                        game = %address,
                        proof_deadline = game.proof_deadline,
                        "challenged game proof deadline elapsed before defense completed"
                    );
                } else {
                    warn!(
                        game = %address,
                        reason = ?reason,
                        "game is invalidatable without defense"
                    );
                }
                Ok(WatchOutcome::Drop)
            }
            GameObservation::Unset => {
                error!(game = %address, "factory game has unset root state");
                Ok(WatchOutcome::Drop)
            }
        }
    }
}

fn has_required_proof_support(game: &GameMetadata, proof_bitmap: u8) -> bool {
    proof_count(proof_bitmap) >= game.proof_threshold
}
