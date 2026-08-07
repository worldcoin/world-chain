use crate::{error::DefenderError, traits::DefenderClient, types::GameMetadata};
use world_chain_proofs::{GameStatus, InvalidationReason, ProposalStatus, proof_count};

/// On-chain state relevant to proof support for one selected game.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GameObservation {
    Proposed {
        proof_bitmap: u8,
        has_initial_support: bool,
    },
    Challenged {
        proof_bitmap: u8,
        has_required_support: bool,
    },
    Finalized,
    Invalidated {
        reason: InvalidationReason,
    },
    Unset,
}

pub(crate) struct GameEvaluator<'a, E> {
    execution_client: &'a E,
}

impl<'a, E> GameEvaluator<'a, E>
where
    E: DefenderClient,
{
    pub(crate) const fn new(execution_client: &'a E) -> Self {
        Self { execution_client }
    }

    pub(crate) async fn observe(
        &self,
        game: &GameMetadata,
    ) -> Result<GameObservation, DefenderError> {
        let status = self
            .execution_client
            .lineage_resolution_status(game.address)
            .await?;
        // A resolvable game reports the outcome a resolve call would produce, so a proof that
        // already landed shows up here before anyone resolves it.
        match status.outcome {
            GameStatus::DefenderWins => return Ok(GameObservation::Finalized),
            GameStatus::ChallengerWins => {
                return Ok(GameObservation::Invalidated {
                    reason: status.invalidation_reason,
                });
            }
            GameStatus::InProgress => {}
        }

        // `GameStatus` cannot distinguish a challenged proposal from an unchallenged one; only
        // the proposal state machine can, and it carries the lane bitmap in the same slot.
        let claim = self.execution_client.claim_data(game.address).await?;
        let proof_bitmap = claim.proof_bitmap;
        Ok(match claim.status {
            ProposalStatus::Unchallenged | ProposalStatus::UnchallengedAndValidProofProvided => {
                GameObservation::Proposed {
                    proof_bitmap,
                    has_initial_support: proof_bitmap != 0,
                }
            }
            ProposalStatus::Challenged | ProposalStatus::ChallengedAndValidProofProvided => {
                GameObservation::Challenged {
                    proof_bitmap,
                    has_required_support: proof_count(proof_bitmap) >= game.proof_threshold,
                }
            }
            // The game sets `Resolved` and its `GameStatus` in the same call, so this is only
            // reachable if the two ever diverge.
            ProposalStatus::Resolved => GameObservation::Unset,
        })
    }

    pub(crate) async fn needs_defense(
        &self,
        game: &GameMetadata,
        now: u64,
    ) -> Result<bool, DefenderError> {
        Ok(match self.observe(game).await? {
            GameObservation::Proposed {
                has_initial_support,
                ..
            } => !has_initial_support && now < game.challenge_deadline,
            GameObservation::Challenged {
                has_required_support,
                ..
            } => !has_required_support && now < game.proof_deadline,
            GameObservation::Finalized
            | GameObservation::Invalidated { .. }
            | GameObservation::Unset => false,
        })
    }
}
