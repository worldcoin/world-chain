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

#[derive(Debug, Clone, Copy)]
enum GameAssessment {
    Proposed,
    Challenged,
    Finalized,
    Closed,
}

/// Result of evaluating a game for one tick.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GameOutcome {
    /// Retain the game for monitoring.
    Track,
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

    async fn assess(&self, game: &GameMetadata, now: u64) -> Result<GameAssessment, DefenderError> {
        match self.observe(game).await? {
            GameObservation::Proposed => {
                if now < game.challenge_deadline {
                    Ok(GameAssessment::Proposed)
                } else {
                    Ok(GameAssessment::Closed)
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
                    return Ok(GameAssessment::Closed);
                }
                if now >= game.proof_deadline {
                    error!(
                        game = %game.address,
                        proof_deadline = game.proof_deadline,
                        "challenged game proof deadline elapsed before defense completed"
                    );
                    return Ok(GameAssessment::Closed);
                }
                Ok(GameAssessment::Challenged)
            }
            GameObservation::Finalized => Ok(GameAssessment::Finalized),
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
                Ok(GameAssessment::Closed)
            }
            GameObservation::Unset => {
                error!(game = %game.address, "factory game has unset root state");
                Ok(GameAssessment::Closed)
            }
        }
    }

    pub(crate) async fn scan(
        &self,
        game: &GameMetadata,
        now: u64,
    ) -> Result<GameOutcome, DefenderError> {
        Ok(match self.assess(game, now).await? {
            GameAssessment::Proposed | GameAssessment::Challenged => GameOutcome::Track,
            GameAssessment::Finalized => {
                info!(
                    game = %game.address,
                    "game already has a positive resolution outcome"
                );
                GameOutcome::Drop
            }
            GameAssessment::Closed => GameOutcome::Drop,
        })
    }

    pub(crate) async fn watch(
        &self,
        game: &GameMetadata,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Result<GameOutcome, DefenderError> {
        let address = game.address;
        match self.assess(game, now).await? {
            GameAssessment::Proposed => Ok(GameOutcome::Track),
            GameAssessment::Challenged => {
                // only judge the root against finalized L2 state
                if game.l2_block_number > latest_finalized_l2_block {
                    return Ok(GameOutcome::Track);
                }
                let root = self
                    .consensus_provider
                    .output_root_at_block(game.l2_block_number)
                    .await?;
                if root == game.root_claim {
                    Ok(GameOutcome::Defend)
                } else {
                    error!(
                        game = %address,
                        claimed_root = %game.root_claim,
                        canonical_root = %root,
                        "allowlisted proposer published a non-canonical root; refusing to defend"
                    );
                    Ok(GameOutcome::Drop)
                }
            }
            GameAssessment::Finalized => {
                info!(
                    game = %game.address,
                    "game has a positive resolution outcome"
                );
                Ok(GameOutcome::Drop)
            }
            GameAssessment::Closed => Ok(GameOutcome::Drop),
        }
    }
}

fn has_required_proof_support(game: &GameMetadata, proof_bitmap: u8) -> bool {
    proof_count(proof_bitmap) >= game.proof_threshold
}
