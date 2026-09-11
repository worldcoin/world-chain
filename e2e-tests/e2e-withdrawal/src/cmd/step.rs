use crate::{args::StepArgs, cmd::prove};

/// Run the `step` command.
pub async fn run(args: &StepArgs) -> eyre::Result<()> {
    let prove_args = args.to_prove();
    prove::run(&prove_args).await
}
