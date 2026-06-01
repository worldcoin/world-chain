use std::{env, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use world_chain_proof_core::types::AggregationInputs;
use world_chain_proof_succinct_client_utils::WorldRangeWitness;

pub use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};

// ---------------------------------------------------------------------------
// Build metadata
// ---------------------------------------------------------------------------

/// Root of the World OP Succinct Lite proof tree.
pub const PROOF_TREE_ROOT: &str = "crates/proof/succinct";
/// SP1 guest program tree excluded from the normal workspace.
pub const PROGRAMS_ROOT: &str = "crates/proof/succinct/programs";
/// Range proof guest package manifest.
pub const RANGE_ETHEREUM_MANIFEST: &str =
    "crates/proof/succinct/programs/range-ethereum/Cargo.toml";
/// Aggregation proof guest package manifest.
pub const AGGREGATION_MANIFEST: &str = "crates/proof/succinct/programs/aggregation/Cargo.toml";
/// ELF manifest used by release/build automation.
pub const ELF_MANIFEST: &str = "crates/proof/succinct/elf/manifest.toml";

/// World SP1 guest programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorldSuccinctProgram {
    /// L2 range execution/derivation proof.
    RangeEthereum,
    /// Proof aggregation program.
    Aggregation,
}

impl WorldSuccinctProgram {
    /// Cargo package name.
    pub const fn package(self) -> &'static str {
        match self {
            Self::RangeEthereum => "world-chain-proof-succinct-range-ethereum",
            Self::Aggregation => "world-chain-proof-succinct-aggregation",
        }
    }

    /// Package manifest path.
    pub const fn manifest_path(self) -> &'static str {
        match self {
            Self::RangeEthereum => RANGE_ETHEREUM_MANIFEST,
            Self::Aggregation => AGGREGATION_MANIFEST,
        }
    }

    /// Canonical ELF name.
    pub const fn elf_name(self) -> &'static str {
        match self {
            Self::RangeEthereum => "world-chain-range-ethereum",
            Self::Aggregation => "world-chain-aggregation",
        }
    }

    /// Environment variable used by the host to load the compiled ELF.
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::RangeEthereum => "WORLD_CHAIN_RANGE_ELF",
            Self::Aggregation => "WORLD_CHAIN_AGGREGATION_ELF",
        }
    }
}

/// All World SP1 programs in release/build order.
pub const PROGRAMS: [WorldSuccinctProgram; 2] = [
    WorldSuccinctProgram::RangeEthereum,
    WorldSuccinctProgram::Aggregation,
];

// ---------------------------------------------------------------------------
// ELF loading
// ---------------------------------------------------------------------------

/// Embedded World aggregation guest ELF.
#[cfg(feature = "embedded-elfs")]
pub const AGGREGATION_ELF: &[u8] = include_bytes!("../../../elf/world-chain-aggregation");

/// Embedded World range guest ELF.
#[cfg(feature = "embedded-elfs")]
pub const RANGE_ELF_EMBEDDED: &[u8] = include_bytes!("../../../elf/world-chain-range-ethereum");

/// Returns the embedded World range ELF.
#[cfg(feature = "embedded-elfs")]
pub fn get_range_elf_embedded() -> &'static [u8] {
    RANGE_ELF_EMBEDDED
}

/// Returns the embedded World aggregation ELF.
#[cfg(feature = "embedded-elfs")]
pub fn get_aggregation_elf_embedded() -> &'static [u8] {
    AGGREGATION_ELF
}

/// Error returned when a compiled SP1 ELF cannot be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ElfLoadError {
    /// The program-specific environment variable is not set.
    #[error("{env_var} is not set")]
    MissingEnv { env_var: &'static str },
    /// Reading the ELF failed.
    #[error("failed to read {program:?} ELF at {path}: {source}")]
    Io {
        program: WorldSuccinctProgram,
        path: PathBuf,
        source: io::Error,
    },
}

/// Loads a compiled SP1 ELF from the program's environment variable.
pub fn load_program_elf(program: WorldSuccinctProgram) -> Result<Vec<u8>, ElfLoadError> {
    let env_var = program.env_var();
    let path = env::var_os(env_var).ok_or(ElfLoadError::MissingEnv { env_var })?;
    let path = PathBuf::from(path);
    fs::read(&path).map_err(|source| ElfLoadError::Io {
        program,
        path,
        source,
    })
}

/// Loads the range proof guest ELF.
pub fn load_range_ethereum_elf() -> Result<Vec<u8>, ElfLoadError> {
    load_program_elf(WorldSuccinctProgram::RangeEthereum)
}

/// Loads the aggregation guest ELF.
pub fn load_aggregation_elf() -> Result<Vec<u8>, ElfLoadError> {
    load_program_elf(WorldSuccinctProgram::Aggregation)
}

// ---------------------------------------------------------------------------
// Proof request types and prover trait
// ---------------------------------------------------------------------------

/// Host request for a single SP1 range proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeProofRequest {
    /// Witness consumed by the range guest.
    pub witness: WorldRangeWitness,
}

/// Host request for an aggregation proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregationProofRequest {
    /// Aggregation inputs consumed by the guest program.
    pub inputs: AggregationInputs,
    /// CBOR-encoded L1 headers, ordered from oldest to newest.
    pub l1_headers_cbor: Vec<u8>,
}

/// Interface expected from a concrete SP1 prover backend.
pub trait WorldSuccinctProver {
    /// Backend-specific error type.
    type Error;

    /// Proves one range witness.
    fn prove_range(&self, request: RangeProofRequest) -> Result<RangeProofArtifact, Self::Error>;

    /// Aggregates already-generated range proofs.
    fn prove_aggregation(
        &self,
        request: AggregationProofRequest,
    ) -> Result<AggregationProofArtifact, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_missing_env_var() {
        if std::env::var_os("WORLD_CHAIN_RANGE_ELF").is_some() {
            return;
        }
        let result = load_program_elf(WorldSuccinctProgram::RangeEthereum);
        assert!(matches!(
            result,
            Err(ElfLoadError::MissingEnv {
                env_var: "WORLD_CHAIN_RANGE_ELF"
            })
        ));
    }

    #[cfg(feature = "embedded-elfs")]
    #[test]
    fn exposes_world_elfs() {
        assert!(get_range_elf_embedded().starts_with(b"\x7fELF"));
        assert!(get_aggregation_elf_embedded().starts_with(b"\x7fELF"));
    }
}
