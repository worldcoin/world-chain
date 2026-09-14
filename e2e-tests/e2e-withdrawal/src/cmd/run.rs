use crate::{
    args::StepArgs,
    cmd::{init, prove, step},
    storage,
    types::{StepError, StepOutcome, StepStage},
};
use alloy_primitives::B256;
use alloy_provider::ProviderBuilder;
use backoff::{Error as BackoffError, ExponentialBackoff, future::retry_notify};
use serde::Serialize;
use std::time::Duration;

/// Run the `run` command.
pub async fn run(args: &StepArgs) -> Result<StepOutcome, StepError> {
    loop {
        let step_result = step::run(&args).await;
        if let Ok(step_outcome) = step_result {
            match step_outcome {
                StepOutcome::Initiated(initiated_outcome) => {
                    tracing::info!(?initiated_outcome);
                }
                StepOutcome::Proven(proven_outcome) => {
                    tracing::info!(?proven_outcome);
                }
                StepOutcome::Finalized(finalized_outcome) => {
                    tracing::info!(?finalized_outcome);
                }
                StepOutcome::Waiting(waiting_outcome) => {
                    tracing::info!(?waiting_outcome);
                }
            }
        } else {
            // check if it's a retryable error:
            // - if it is, then retry `step`

            // safe unwrap because we've just checked above
            let step_error = step_result.unwrap_err();
            tracing::error!(?step_error);
            if step_error.is_retryable() {
                // sleep and continue
                tokio::time::sleep(Duration::from_secs(12)).await;
                continue;
            }

            // if error is `PersistenceAfterTransaction` we need to gather the tx receipt,
            // recreate the needed data, save it and then continue the loop
            if let StepError::PersistenceAfterTransaction {
                transaction_hash,
                stage,
                source: _,
            } = &step_error
            {
                match stage {
                    StepStage::Init => {
                        let l2_provider = ProviderBuilder::new()
                            .connect(&args.l2_rpc_endpoint)
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?;
                        let initiated_withdrawal =
                            init::recover_initiated_withdrawal(l2_provider, *transaction_hash)
                                .await?;
                        write_json_with_backoff(
                            &args.initiated,
                            &initiated_withdrawal,
                            *transaction_hash,
                            StepStage::Init,
                        )
                        .await?;
                        tokio::time::sleep(Duration::from_secs(12)).await;
                        continue;
                    }
                    StepStage::Prove => {
                        let l1_provider = ProviderBuilder::new()
                            .connect(&args.l1_args.l1_rpc_endpoint)
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?;
                        let initiated_withdrawal = storage::read_json(&args.initiated)
                            .await
                            .map_err(|err| StepError::Generic(err.into()))?;
                        let prove_withdrawal = prove::recover_prove_withdrawal(
                            &l1_provider,
                            args.dispute_game_factory,
                            *transaction_hash,
                            initiated_withdrawal,
                        )
                        .await?;
                        write_json_with_backoff(
                            &args.proven,
                            &prove_withdrawal,
                            *transaction_hash,
                            StepStage::Prove,
                        )
                        .await?;
                        tokio::time::sleep(Duration::from_secs(12)).await;
                        continue;
                    }
                    _ => {
                        // other stages don't have the `PersistenceAfterTransaction` error variant
                    }
                }
            }

            // otherwise just exit the loop and return the error
            return Err(step_error);
        }
        tokio::time::sleep(Duration::from_secs(12)).await;
    }
}

/// Maximum write attempts (initial try + retries) when reconciling a handoff.
const MAX_PERSISTENCE_ATTEMPTS: u32 = 10;

/// Persist a handoff with bounded exponential backoff.
async fn write_json_with_backoff<T: Serialize>(
    location: &str,
    value: &T,
    transaction_hash: B256,
    stage: StepStage,
) -> Result<(), StepError> {
    let backoff = ExponentialBackoff::default();
    let mut attempts = 0u32;
    retry_notify(
        backoff,
        || {
            attempts += 1;
            let attempt = attempts;
            async move {
                match storage::write_json(location, value).await {
                    Ok(()) => Ok(()),
                    Err(err) if attempt >= MAX_PERSISTENCE_ATTEMPTS => {
                        Err(BackoffError::permanent(err))
                    }
                    Err(err) => Err(BackoffError::transient(err)),
                }
            }
        },
        |err, delay: Duration| {
            tracing::warn!(
                ?err,
                retry_in_secs = delay.as_secs(),
                %transaction_hash,
                %stage,
                "persistence after transaction failed; retrying"
            );
        },
    )
    .await
    .map_err(|err| StepError::PersistenceAfterTransaction {
        transaction_hash,
        stage,
        source: err,
    })
}
