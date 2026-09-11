use crate::{
    args::StepArgs,
    cmd::{finalize, init, prove},
    storage,
};
use eyre::eyre::bail;

/// Run the `step` command.
///
/// Dispatches to the next withdrawal stage based on durable handoff presence:
/// - no initiated -> `init`
/// - initiated, no proven -> `prove`
/// - proven -> `finalize`, then delete both handoffs
pub async fn run(args: &StepArgs) -> eyre::Result<()> {
    let initiated_exists = storage::exists(&args.initiated).await?;
    let proven_exists = storage::exists(&args.proven).await?;

    match (initiated_exists, proven_exists) {
        (false, false) => init::run(&args.to_init()).await,
        (true, false) => prove::run(&args.to_prove()).await,
        (true, true) => {
            finalize::run(&args.to_finalize()).await?;
            storage::delete(&args.initiated).await?;
            storage::delete(&args.proven).await?;
            Ok(())
        }
        (false, true) => {
            bail!(
                "inconsistent withdrawal state: proven handoff exists at `{}` \
                 but initiated handoff is missing at `{}`",
                args.proven,
                args.initiated
            )
        }
    }
}
