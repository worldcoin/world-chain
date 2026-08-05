use crate::{error::DefenderError, traits::DefenderClient, types::GameMetadata};
use world_chain_proofs::{ClaimData, InvalidationReason, ProposalStatus, proof_count};

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
        let ClaimData {
            status,
            proof_bitmap,
            invalidation_reason,
        } = self.execution_client.claim_data(game.address).await?;
        Ok(match status {
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
            ProposalStatus::Resolved => {
                if invalidation_reason == InvalidationReason::None {
                    GameObservation::Finalized
                } else {
                    GameObservation::Invalidated {
                        reason: invalidation_reason,
                    }
                }
            }
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
            GameObservation::Finalized | GameObservation::Invalidated { .. } => false,
        })
    }
}
