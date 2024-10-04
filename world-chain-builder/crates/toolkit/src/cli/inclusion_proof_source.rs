use clap::Args;
use semaphore::poseidon_tree::Proof;

use super::utils::parse_from_json;

#[derive(Debug, Args)]
pub struct InclusionProofSource {
    #[clap(
        short = 'P',
        long,
        value_parser = parse_from_json
    )]
    pub inclusion_proof: Proof,
}
