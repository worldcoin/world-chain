use crate::{error::DefenderError, traits::DefenderClient, types::GameMetadata};
use world_chain_proofs::{InvalidationReason, RootState, proof_count};

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
        Ok(match status.root_state {
            RootState::Proposed => {
                let proof_bitmap = self.execution_client.proof_bitmap(game.address).await?;
                GameObservation::Proposed {
                    proof_bitmap,
                    has_initial_support: proof_bitmap != 0,
                }
            }
            RootState::Challenged => {
                let proof_bitmap = self.execution_client.proof_bitmap(game.address).await?;
                GameObservation::Challenged {
                    proof_bitmap,
                    has_required_support: proof_count(proof_bitmap) >= game.proof_threshold,
                }
            }
            RootState::Finalized => GameObservation::Finalized,
            RootState::Invalidated => GameObservation::Invalidated {
                reason: status.invalidation_reason,
            },
            RootState::None => GameObservation::Unset,
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
