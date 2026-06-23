//! Compile-time embedded World Chain SP1 guest program ELFs.
//!
//! This crate is a **production-only** dependency: it runs the SP1 Docker
//! build at compile time (via `build.rs`) and bakes the resulting ELF bytes
//! into the binary with `sp1_sdk::include_elf!()`.

#[cfg(clippy)]
use sp1_sdk::Elf;
#[cfg(not(clippy))]
use sp1_sdk::{Elf, include_elf};

/// Returns the compile-time embedded World Chain range-proof guest ELF.
pub fn range_elf() -> Elf {
    #[cfg(not(clippy))]
    {
        include_elf!("world-chain-proof-succinct-range-ethereum")
    }
    #[cfg(clippy)]
    {
        panic!("ELFs are not available in clippy mode — run a real build")
    }
}

/// Returns the compile-time embedded World Chain aggregation guest ELF.
pub fn aggregation_elf() -> Elf {
    #[cfg(not(clippy))]
    {
        include_elf!("world-chain-proof-succinct-aggregation")
    }
    #[cfg(clippy)]
    {
        panic!("ELFs are not available in clippy mode — run a real build")
    }
}
