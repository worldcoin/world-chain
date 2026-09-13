use crate::{
    args::StepArgs,
    cmd::{finalize, init, prove},
    storage,
};
use eyre::eyre::WrapErr;

/// Run the `step` command.
///
/// Dispatches to the next withdrawal stage based on durable handoff presence:
/// - no initiated -> `init`
/// - initiated, no proven -> `prove`
/// - proven -> `finalize` (idempotent if already on-chain), then delete handoffs
pub async fn run(args: &StepArgs) -> eyre::Result<()> {
    let initiated_exists = storage::exists(&args.initiated).await?;
    let proven_exists = storage::exists(&args.proven).await?;

    match (initiated_exists, proven_exists) {
        (false, false) => init::run(&args.to_init()).await,
        (true, false) => prove::run(&args.to_prove()).await,
        // Both present, or only proven left after a partial cleanup: finalize is
        // idempotent, then retry handoff deletion.
        (true, true) | (false, true) => {
            finalize::run(&args.to_finalize()).await?;
            delete_handoffs(args).await
        }
    }
}

/// Delete handoffs in order: initiated first, then proven.
///
/// If initiated delete fails, keep proven so the next tick still sees both
/// handoffs and retries finalize (idempotent) + cleanup instead of re-proving.
async fn delete_handoffs(args: &StepArgs) -> eyre::Result<()> {
    storage::delete(&args.initiated)
        .await
        .wrap_err_with(|| format!("failed to delete initiated handoff at `{}`", args.initiated))?;
    storage::delete(&args.proven)
        .await
        .wrap_err_with(|| format!("failed to delete proven handoff at `{}`", args.proven))?;
    Ok(())
}
