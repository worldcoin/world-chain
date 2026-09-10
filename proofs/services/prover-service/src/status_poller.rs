use std::{sync::Arc, time::Duration};

use alloy_primitives::Address;
use alloy_provider::Provider;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};
use world_chain_proof_protocol::{GameStatus, IMultiProofGame, ProposalStatus};

use crate::ProverService;

/// Cancels obsolete proof jobs and fails requests that exhausted their worker attempts.
pub async fn run_status_poller<P: Provider + Clone>(
    service: Arc<ProverService>,
    provider: P,
    poll_interval: Duration,
) {
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        if let Err(error) = cancel_obsolete_proofs(&service, &provider).await {
            warn!(%error, "obsolete proof scan failed");
        }
        match service.mark_exhausted_proof_requests_failed().await {
            Ok(0) => {}
            Ok(failed) => {
                warn!(
                    failed,
                    "status poller marked exhausted proof requests failed"
                );
            }
            Err(error) => {
                warn!(%error, "status poller scan failed");
            }
        }
    }
}

async fn proof_is_obsolete<P: Provider + Clone>(
    provider: &P,
    address: Address,
) -> anyhow::Result<bool> {
    let game = IMultiProofGame::new(address, provider);
    // One snapshot prevents a concurrent challenge from mixing old support with new state.
    let (resolution, claim) = provider
        .multicall()
        .add(game.resolutionStatus())
        .add(game.claimData())
        .aggregate()
        .await?;
    let outcome = GameStatus::try_from(resolution.outcome)?;
    let status = ProposalStatus::try_from(claim.status)?;
    Ok(outcome != GameStatus::InProgress
        || status.has_sufficient_proof_support()
        || status == ProposalStatus::Resolved)
}

pub(crate) async fn cancel_obsolete_proofs<P: Provider + Clone>(
    service: &ProverService,
    provider: &P,
) -> anyhow::Result<()> {
    for game in service.active_games().await? {
        match proof_is_obsolete(provider, game).await {
            Ok(true) => match service.cancel_game_proofs(game).await {
                Ok(cancelled) if cancelled > 0 => {
                    info!(game_address = %game, cancelled, "cancelled obsolete proof requests");
                }
                Ok(_) => {}
                Err(error) => warn!(game_address = %game, %error, "proof cancellation failed"),
            },
            Ok(false) => {}
            Err(error) => {
                warn!(game_address = %game, %error, "could not check proof support; retaining jobs")
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloy_primitives::{Bytes, U256};
    use alloy_provider::ProviderBuilder;
    use alloy_sol_types::{SolCall, SolValue};
    use alloy_transport::mock::Asserter;

    pub(crate) fn push_game_state(asserter: &Asserter, outcome: u8, status: u8, bitmap: u8) {
        let resolution = Bytes::from(IMultiProofGame::resolutionStatusCall::abi_encode_returns(
            &IMultiProofGame::resolutionStatusReturn {
                resolvable: false,
                outcome,
                reason: 0,
            },
        ));
        let claim = Bytes::from(IMultiProofGame::claimDataCall::abi_encode_returns(
            &IMultiProofGame::claimDataReturn {
                status,
                challenger: Address::ZERO,
                deadline: 0,
                proofBitmap: bitmap,
                invalidationReason: 0,
            },
        ));
        asserter.push_success(&Bytes::from(
            (U256::from(100), vec![resolution, claim]).abi_encode_params(),
        ));
    }

    #[tokio::test]
    async fn follows_contract_support_and_resolution_states() {
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());
        for (outcome, status, bitmap, obsolete) in [
            (0, 0, 0, false), // Unchallenged, no support.
            (0, 1, 4, false), // Challenged, council alone is below threshold.
            (0, 2, 4, true),  // Council satisfies an unchallenged game.
            (0, 3, 6, true),  // TEE plus council satisfies the threshold without SP1.
            (1, 1, 0, true),  // Invalidated or timed out, including with an open parent.
            (2, 4, 5, true),  // Resolved in the defender's favor.
        ] {
            push_game_state(&asserter, outcome, status, bitmap);
            assert_eq!(
                proof_is_obsolete(&provider, Address::ZERO).await.unwrap(),
                obsolete,
                "outcome={outcome}, status={status}, bitmap={bitmap}",
            );
        }
        for (outcome, status) in [(255, 0), (0, 255)] {
            push_game_state(&asserter, outcome, status, 0);
            assert!(proof_is_obsolete(&provider, Address::ZERO).await.is_err());
        }
        asserter.push_failure_msg("L1 unavailable");
        assert!(proof_is_obsolete(&provider, Address::ZERO).await.is_err());
    }
}
