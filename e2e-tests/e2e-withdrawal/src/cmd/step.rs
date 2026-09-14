use crate::{
    args::StepArgs,
    cmd::{finalize, init, prove},
    storage,
    types::{FinalizedOutcome, StepError, StepOutcome},
};
use eyre::eyre::WrapErr;

/// Run the `step` command.
///
/// Dispatches to the next withdrawal stage based on durable handoff presence:
/// - no initiated -> `init`
/// - initiated, no proven -> `prove`
/// - proven -> `finalize` (idempotent if already on-chain), then delete handoffs
pub async fn run(args: &StepArgs) -> Result<StepOutcome, StepError> {
    let initiated_exists = storage::exists(&args.initiated)
        .await
        .map_err(StepError::Generic)?;
    let proven_exists = storage::exists(&args.proven)
        .await
        .map_err(StepError::Generic)?;

    match (initiated_exists, proven_exists) {
        (false, false) => init::run(&args.to_init()).await,
        (true, false) => prove::run(&args.to_prove()).await,
        // Both present, or only proven left after a partial cleanup: finalize is
        // idempotent, then retry handoff deletion.
        (true, true) | (false, true) => {
            let outcome = finalize::run(&args.to_finalize()).await?;
            match outcome {
                StepOutcome::Finalized(finalized) => {
                    delete_handoffs(args, &finalized).await?;
                    Ok(StepOutcome::Finalized(finalized))
                }
                // Waiting (maturity / game not valid yet): keep handoffs for the next tick.
                other => Ok(other),
            }
        }
    }
}

/// Delete handoffs in order: initiated first, then proven.
///
/// If initiated delete fails, keep proven so the next tick still sees both
/// handoffs and retries finalize (idempotent) + cleanup instead of re-proving.
async fn delete_handoffs(args: &StepArgs, finalized: &FinalizedOutcome) -> Result<(), StepError> {
    let withdrawal_hash = finalized.withdrawal_hash();
    storage::delete(&args.initiated)
        .await
        .wrap_err_with(|| format!("failed to delete initiated handoff at `{}`", args.initiated))
        .map_err(|source| StepError::CleanupAfterFinalized {
            withdrawal_hash,
            source,
        })?;
    storage::delete(&args.proven)
        .await
        .wrap_err_with(|| format!("failed to delete proven handoff at `{}`", args.proven))
        .map_err(|source| StepError::CleanupAfterFinalized {
            withdrawal_hash,
            source,
        })?;
    Ok(())
}
