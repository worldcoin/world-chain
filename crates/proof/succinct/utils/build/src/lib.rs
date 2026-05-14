//! Build metadata for the World Chain SP1 guest programs.

use serde::{Deserialize, Serialize};

/// Root of the World OP Succinct Lite proof tree.
pub const PROOF_TREE_ROOT: &str = "crates/proof/succinct";
/// SP1 guest program tree excluded from the normal workspace.
pub const PROGRAMS_ROOT: &str = "crates/proof/succinct/programs";
/// Range proof guest package manifest.
pub const RANGE_ETHEREUM_MANIFEST: &str =
    "crates/proof/succinct/programs/range/ethereum/Cargo.toml";
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
