use std::path::PathBuf;

use clap::Args;
use semaphore::poseidon_tree::Proof;

use super::utils::parse_from_json;
use crate::InclusionProof;

#[derive(Debug, Clone, Args)]
pub struct InclusionProofSource {
    /// Inclusion proof in JSON format
    #[clap(
        short = 'P',
        long,
        value_parser = parse_from_json::<Proof>,
        conflicts_with = "inclusion_proof_file",
        conflicts_with = "inclusion_proof_url",
        required_unless_present = "inclusion_proof_file",
        required_unless_present = "inclusion_proof_url"
    )]
    pub inclusion_proof: Option<InclusionProof>,

    #[clap(
        long,
        conflicts_with = "inclusion_proof",
        conflicts_with = "inclusion_proof_url",
        required_unless_present = "inclusion_proof",
        required_unless_present = "inclusion_proof_url"
    )]
    pub inclusion_proof_file: Option<PathBuf>,

    /// Endpoint to fetch the inclusion proof from
    ///
    /// i.e. https://world-tree.crypto.worldcoin.dev/inclusionProof
    #[clap(
        long,
        env,
        conflicts_with = "inclusion_proof",
        conflicts_with = "inclusion_proof_file",
        required_unless_present = "inclusion_proof",
        required_unless_present = "inclusion_proof_file"
    )]
    pub inclusion_proof_url: Option<String>,
}

impl InclusionProofSource {
    pub fn into_variant(self) -> InclusionProofSourceVariant {
        if let Some(inclusion_proof) = self.inclusion_proof {
            return InclusionProofSourceVariant::Proof(inclusion_proof);
        }

        if let Some(inclusion_proof_file) = self.inclusion_proof_file {
            return InclusionProofSourceVariant::File(inclusion_proof_file);
        }

        if let Some(inclusion_proof_url) = self.inclusion_proof_url {
            return InclusionProofSourceVariant::Url(inclusion_proof_url);
        }

        unreachable!()
    }
}

pub enum InclusionProofSourceVariant {
    Proof(InclusionProof),
    File(PathBuf),
    Url(String),
}
