//! Custom crypto provider for KZG proof verification.
//!
//! # Why a custom `Crypto` impl?
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
//!
use kzg_rs::{Bytes32, Bytes48, KzgProof, KzgSettings};
use revm::precompile::{Crypto, PrecompileHalt};

/// Custom cryptography provider using kzg-rs for KZG proof verification.
///
/// Uses `kzg-rs` (pure Rust) instead of `c-kzg` so it compiles inside the SP1 zkVM
/// guest where the blst C library is unavailable.
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

        // Well-formed but invalid openings return Ok(false), not an error.
        // https://github.com/succinctlabs/op-succinct/security/advisories/GHSA-pq4w-5vv8-gxhr
        match KzgProof::verify_kzg_proof(&commitment, &z, &y, &proof, &self.kzg_settings) {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(PrecompileHalt::BlobVerifyKzgProofFailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::hex_literal::hex;

    #[test]
    fn invalid_kzg_opening_is_rejected() {
        let commitment = hex!(
            "8f59a8d2a1a625a17f3fea0fe5eb8c896db3764f3185481bc22f91b4aaffcca25f26936857bc3a7c2539ea8ec3a952b7"
        );
        let z = hex!("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000");
        let mut y = hex!("1522a4a7f34e1ea350ae07c29c96c7e79655aa926122e95fe69fcbd932ca49e9");
        let proof = hex!(
            "a62ad71d14c5719385c0686f1871430475bf3a00f0aa3f7b8dd99a9abc2160744faf0070725e00b60ad9a026a15b1a8c"
        );

        // Keep the input well-formed, but make the claimed evaluation wrong.
        y[31] ^= 1;

        let crypto = CustomCrypto::default();
        let raw_result = KzgProof::verify_kzg_proof(
            &Bytes48::from_slice(&commitment).unwrap(),
            &Bytes32::from_slice(&z).unwrap(),
            &Bytes32::from_slice(&y).unwrap(),
            &Bytes48::from_slice(&proof).unwrap(),
            &crypto.kzg_settings,
        );
        assert!(
            matches!(raw_result, Ok(false)),
            "raw kzg-rs result: {raw_result:?}"
        );

        assert!(matches!(
            crypto.verify_kzg_proof(&z, &y, &commitment, &proof),
            Err(PrecompileHalt::BlobVerifyKzgProofFailed)
        ));
    }

    #[test]
    fn valid_kzg_opening_is_accepted() {
        let commitment = hex!(
            "8f59a8d2a1a625a17f3fea0fe5eb8c896db3764f3185481bc22f91b4aaffcca25f26936857bc3a7c2539ea8ec3a952b7"
        );
        let z = hex!("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000");
        let y = hex!("1522a4a7f34e1ea350ae07c29c96c7e79655aa926122e95fe69fcbd932ca49e9");
        let proof = hex!(
            "a62ad71d14c5719385c0686f1871430475bf3a00f0aa3f7b8dd99a9abc2160744faf0070725e00b60ad9a026a15b1a8c"
        );

        let crypto = CustomCrypto::default();
        assert_eq!(crypto.verify_kzg_proof(&z, &y, &commitment, &proof), Ok(()));
    }
}
