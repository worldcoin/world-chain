//! ELF loading helpers for World Chain SP1 programs.

use std::{env, fs, io, path::PathBuf};

pub use world_chain_proof_succinct_build_utils::WorldSuccinctProgram;

/// Embedded World aggregation guest ELF.
pub const AGGREGATION_ELF: &[u8] = include_bytes!("../../../elf/world-chain-aggregation");

/// Embedded World range guest ELF.
pub const RANGE_ELF_EMBEDDED: &[u8] = include_bytes!("../../../elf/world-chain-range-ethereum");

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

    #[test]
    fn embedded_elfs_are_present() {
        assert!(RANGE_ELF_EMBEDDED.starts_with(b"\x7fELF"));
        assert!(AGGREGATION_ELF.starts_with(b"\x7fELF"));
    }
}
