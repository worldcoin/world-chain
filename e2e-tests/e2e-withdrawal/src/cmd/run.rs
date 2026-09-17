use crate::{
    args::StepArgs,
    clients::Clients,
    cmd::step,
    retry::RetryBudget,
    storage,
    types::{GameBlockedReason, StepError, StepOutcome, StepStage},
};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(120);

/// Run complete withdrawal cycles until a terminal failure occurs.
pub async fn run(args: &StepArgs) -> Result<StepOutcome, StepError> {
    args.validate()?;
    let clients = Clients::from_step_args(args).await?;
    let mut cleanup_retry = RetryBudget::default();
    loop {
        let result = step::run_with(args, &clients).await;
        match &result {
            Ok(outcome) => tracing::info!(?outcome, "withdrawal workflow iteration completed"),
            Err(_) => tracing::info!("withdrawal workflow iteration failed, handling error"),
        }
        match result {
            Ok(_) => cleanup_retry = RetryBudget::default(),
            Err(StepError::PersistenceAfterTransaction {
                transaction_hash,
                stage,
                handoff,
                source,
            }) => {
                let location = match stage {
                    StepStage::Init => &args.initiated,
                    StepStage::Prove => &args.proven,
                    _ => {
                        return Err(StepError::PersistenceAfterTransaction {
                            transaction_hash,
                            stage,
                            handoff,
                            source,
                        });
                    }
                };
                // Include the initial write in the budget and retain the payload.
                // Recovery needs no further RPC or storage reads.
                let mut retry = RetryBudget::default();
                let mut source = source;
                loop {
                    let Some(delay) = retry.next_delay(storage::is_permanent_error(&source)) else {
                        return Err(StepError::PersistenceAfterTransaction {
                            transaction_hash,
                            stage,
                            handoff,
                            source,
                        });
                    };
                    tracing::warn!(error = ?source, %transaction_hash, %stage,
                        attempt = retry.failures, retry_in_secs = delay.as_secs_f64(),
                        "handoff persistence failed, retrying");
                    tokio::time::sleep(delay).await;
                    match storage::write_json(location, &handoff).await {
                        Ok(()) => break,
                        Err(error) => source = error,
                    }
                }
            }
            Err(error @ StepError::CleanupAfterFinalized { .. }) => {
                let StepError::CleanupAfterFinalized { source, .. } = &error else {
                    // Safe, we've just checked it's a `CleanupAfterFinalized` error
                    unreachable!()
                };
                let Some(delay) = cleanup_retry.next_delay(storage::is_permanent_error(source))
                else {
                    return Err(error);
                };
                tracing::warn!(error = ?error, attempt = cleanup_retry.failures,
                    retry_in_secs = delay.as_secs_f64(), "handoff cleanup failed, retrying");
                tokio::time::sleep(delay).await;
                continue;
            }
            Err(StepError::GameBlocked {
                withdrawal_hash,
                game_address,
                reason,
            }) if reason != GameBlockedReason::SystemPaused => {
                tracing::warn!(
                    %withdrawal_hash,
                    %game_address,
                    %reason,
                    "supporting game is invalid. Deleting proven handoff to re-prove"
                );
                storage::delete(&args.proven)
                    .await
                    .map_err(StepError::Generic)?;
            }
            Err(error) if error.is_retryable() => {
                tracing::warn!(error = ?error, "retryable withdrawal condition, waiting");
            }
            Err(error) => return Err(error),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
