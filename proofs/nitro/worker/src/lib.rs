//! The `nitro-worker` is the AWS Nitro TEE proving worker of the World Chain defender stack.
//!
//! It is the TEE analogue of the `sp1-worker`: it leases [`ProofBackend::Nitro`] jobs from the
//! `prover-service`, builds the range witness over RPC, hands it to a running Nitro Enclave for
//! attested derivation, and submits the resulting attestation + signature pair back to the
//! `prover-service`. It performs no on-chain interaction — the defender turns completed proofs
//! into transactions.
//!
//! ```text
//! PROVER-SERVICE --getNextProof(Nitro)--> NITRO-WORKER --witness--> NITRO ENCLAVE
//!        ^                                                                 |
//!        +------------------------- submitProof(attestation, signature) <-+
//! ```
//!
//! The worker only compiles on Linux because it relies on AF_VSOCK to talk to the enclave.
//! On non-Linux hosts the crate exposes no public items; the binary in `src/main.rs` aborts
//! with a clear error message.
//!
//! [`ProofBackend::Nitro`]: world_chain_prover_service::ProofBackend::Nitro

#[cfg(target_os = "linux")]
mod backend;

#[cfg(target_os = "linux")]
pub use backend::{NitroBackend, NitroBackendConfig};

// Re-exported so binaries and tests can build a worker without depending on
// `world-chain-proof-worker` directly. Mirrors the layout of `world-chain-sp1-worker`.
#[cfg(target_os = "linux")]
pub use world_chain_proof_worker::{ProofJobBackend, ProofWorker, ProofWorkerConfig};
