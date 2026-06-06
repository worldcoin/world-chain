//! Custom crypto provider for KZG proof verification.
//!
//! # Why a custom `Crypto` impl?
//!
//! `revm-precompile`'s default [`Crypto`] implementation uses `c-kzg` (backed by the
//! blst C library) for KZG point-evaluation. blst cannot be compiled inside the SP1
//! zkVM guest because:
//!   1. It requires a C toolchain (not available in the SP1 build environment), and
//!   2. It uses platform SIMD / assembly that is not supported in the RISC-V zkVM ISA.
//!
//! [`kzg-rs`](https://crates.io/crates/kzg-rs) is a pure-Rust KZG implementation that
//! compiles cleanly inside the zkVM. This shim wires it into revm's crypto interface.
//!
//! There is no upstream crate that already provides a `kzg-rs`-backed [`Crypto`] impl,
//! so we roll a minimal one here. SP1's BLS12-381 syscalls only accelerate G1
//! add/double/decompress — they do not expose a pairing precompile — so a full zkVM
//! hardware-accelerated KZG path is not yet feasible and remains a future optimisation.

use kzg_rs::{Bytes32, Bytes48, KzgProof, KzgSettings};
use revm::precompile::{Crypto, PrecompileHalt};

/// Custom cryptography provider using kzg-rs for KZG proof verification.
///
/// See the module-level documentation for the rationale behind this type.
#[derive(Debug)]
pub struct CustomCrypto {
    kzg_settings: KzgSettings,
}

impl Default for CustomCrypto {
    fn default() -> Self {
        Self {
            kzg_settings: KzgSettings::load_trusted_setup_file().unwrap(),
        }
    }
}

impl Crypto for CustomCrypto {
    fn verify_kzg_proof(
        &self,
        z: &[u8; 32],
        y: &[u8; 32],
        commitment: &[u8; 48],
        proof: &[u8; 48],
    ) -> Result<(), PrecompileHalt> {
        let z = Bytes32::from_slice(z).map_err(|_| PrecompileHalt::BlobVerifyKzgProofFailed)?;
        let y = Bytes32::from_slice(y).map_err(|_| PrecompileHalt::BlobVerifyKzgProofFailed)?;
        let commitment = Bytes48::from_slice(commitment)
            .map_err(|_| PrecompileHalt::BlobVerifyKzgProofFailed)?;
        let proof =
            Bytes48::from_slice(proof).map_err(|_| PrecompileHalt::BlobVerifyKzgProofFailed)?;

        KzgProof::verify_kzg_proof(&commitment, &z, &y, &proof, &self.kzg_settings)
            .map_err(|_| PrecompileHalt::BlobVerifyKzgProofFailed)?;

        Ok(())
    }
}
