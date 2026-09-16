use crate::{
    args::StepArgs,
    clients::Clients,
    cmd::{finalize, init, prove},
    storage,
    types::{FinalizedOutcome, StepError, StepOutcome, StepStage},
};
use eyre::eyre::WrapErr;

/// Run the `step` command.
///
/// Dispatches to the next withdrawal stage based on durable handoff presence:
/// - no initiated -> `init`
/// - initiated, no proven -> `prove`
/// - proven -> `finalize` (idempotent if already on-chain), then delete handoffs
pub async fn run(args: &StepArgs) -> Result<StepOutcome, StepError> {
    args.validate()?;
    let clients = Clients::from_step_args(args).await?;
    run_with(args, &clients).await
}

/// Run the next workflow stage using pre-built providers.
pub async fn run_with(args: &StepArgs, clients: &Clients) -> Result<StepOutcome, StepError> {
    let initiated_exists = storage::exists(&args.initiated).await?;
    let proven_exists = storage::exists(&args.proven).await?;

    match (initiated_exists, proven_exists) {
        (false, false) => {
            init::run_with(&args.to_init(), &clients.l2, clients.l2_address).await
        }
        (true, false) => prove::run_with(&args.to_prove(), &clients.l1, &clients.l2).await,
        // Both present, or only proven left after a partial cleanup: finalize is
        // idempotent, then retry handoff deletion.
        (true, true) | (false, true) => {
            let outcome = finalize::run_with(&args.to_finalize(), &clients.l1).await?;
            match outcome {
                StepOutcome::Finalized(finalized) => {
                    delete_handoffs(args, &finalized).await?;
                    Ok(StepOutcome::Finalized(finalized))
                }
                StepOutcome::Waiting(waiting) => Ok(StepOutcome::Waiting(waiting)),
                StepOutcome::Initiated(_) | StepOutcome::Proven(_) => {
                    Err(StepError::InvalidState {
                        stage: StepStage::Finalize,
                        message: "finalize returned an outcome belonging to another stage",
                    })
                }
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
