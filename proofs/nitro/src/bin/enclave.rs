//! Entry point for the `world-chain-nitro-enclave` binary.
//!
//! This file runs **inside** the Nitro Enclave. It is the same role the SP1 range program
//! plays for the Succinct backend: it ingests a witness, drives the OP Stack derivation
//! pipeline end-to-end, and emits the canonical `BootInfoStruct`. The difference is that
//! integrity is established by an NSM-attested `COSE_Sign1` document rather than a ZK proof.
//!
//! Communication with the host happens over vsock using the framing defined in
//! [`world_chain_proof_nitro::protocol`].
//!
//! Build with the `enclave` feature:
//!
//! ```sh
//! cargo build --release --bin world-chain-nitro-enclave \
//!     -p world-chain-proof-nitro --features enclave
//! ```

#![cfg(feature = "enclave")]

#[cfg(target_os = "linux")]
use anyhow::Result;
#[cfg(target_os = "linux")]
use tracing_subscriber::EnvFilter;
#[cfg(target_os = "linux")]
use world_chain_proof_nitro::enclave;

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    enclave::run().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("world-chain-nitro-enclave is only supported on Linux");
}
