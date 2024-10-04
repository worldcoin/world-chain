use std::path::PathBuf;

use clap::Args;
use semaphore::poseidon_tree::Proof;

use super::utils::parse_from_json;

#[derive(Debug, Clone, Args)]
pub struct InclusionProofSource {
    /// Inclusion proof in JSON format
    #[clap(
        short = 'P',
        long,
        value_parser = parse_from_json::<Proof>,
        conflicts_with = "inclusion_proof_file",
        required_unless_present = "inclusion_proof_file"
    )]
    pub inclusion_proof: Option<Proof>,

    #[clap(
        long,
        conflicts_with = "inclusion_proof",
        required_unless_present = "inclusion_proof"
    )]
    pub inclusion_proof_file: Option<PathBuf>,

    // TODO: Add fetching from signup-sequencer/world-tree
    // TODO: Add fetching from smart contract via RPC
}

impl InclusionProofSource {
    pub fn load(&self) -> Proof {
        if let Some(inclusion_proof) = self.inclusion_proof.clone() {
            return inclusion_proof;
        }

        if let Some(inclusion_proof_file) = &self.inclusion_proof_file {
            let inclusion_proof = std::fs::read(inclusion_proof_file).unwrap();
            return serde_json::from_slice(&inclusion_proof).unwrap();
        }

        unreachable!()
    }
}
