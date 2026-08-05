//! Binding the worker to whichever enclave is currently running.
//!
//! A Nitro enclave's signing key is ephemeral: it is generated inside the enclave at
//! startup and never persisted (see `world_chain_proof_nitro::enclave`). The worker is
//! therefore bound to one specific enclave instance — its vsock CID *and* the on-chain
//! registration of that instance's key. Resolving either one once at process start makes
//! an enclave replacement unrecoverable: the worker keeps dialling a dead CID, or proves
//! with a key no verifier accepts, while the process itself looks perfectly healthy.
//!
//! [`EnclaveBinding::bind`] re-establishes both before every job. The CID is re-read from
//! its source and the key registration is re-asserted, so replacing the enclave costs one
//! failed job instead of requiring an operator to restart the pod.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};
use world_chain_proof_nitro::{
    ExpectedPcrs,
    host::{EnclaveEndpoint, NitroProver},
    register::{RegisterParams, RegistrationOutcome, register_enclave_key},
};

/// Where the running enclave's vsock CID comes from.
#[derive(Clone, Debug)]
pub enum EnclaveCidSource {
    /// A CID fixed at startup, from `--enclave-cid`.
    Fixed(u32),
    /// A file the enclave launcher rewrites whenever it starts an enclave. Re-read on every
    /// bind so a replacement is picked up without restarting the worker.
    File(PathBuf),
}

impl EnclaveCidSource {
    fn resolve(&self) -> Result<u32> {
        match self {
            Self::Fixed(cid) => Ok(*cid),
            Self::File(path) => {
                let raw = fs::read_to_string(path).with_context(|| {
                    format!("failed to read enclave CID from {}", path.display())
                })?;
                raw.trim()
                    .parse()
                    .with_context(|| format!("{} does not contain a vsock CID", path.display()))
            }
        }
    }
}

/// Credentials for asserting the current enclave's key is registered on-chain.
#[derive(Clone, Debug)]
pub struct RegistrationCredentials {
    /// L1 execution RPC used to read the registry and submit `registerKey`.
    pub l1_rpc_url: String,
    /// `NitroEnclaveKeyRegistry` address on L1.
    pub registry: String,
    /// Funded key that pays for `registerKey`. Not owner-gated; any funded key works.
    pub private_key: String,
}

/// Resolves the running enclave and guarantees its key is registered before it is used.
#[derive(Clone, Debug)]
pub struct EnclaveBinding {
    cid_source: EnclaveCidSource,
    port: u32,
    expected_pcrs: ExpectedPcrs,
    /// `None` disables the registration check — the operator is registering out of band.
    registration: Option<RegistrationCredentials>,
}

/// A worker bound to a specific, registered enclave.
#[derive(Clone, Debug)]
pub struct EnclaveSession {
    /// Prover pinned to the enclave resolved by this bind.
    pub prover: NitroProver,
    /// vsock CID this session is bound to.
    pub cid: u32,
    /// Registration result, or `None` when the check is disabled.
    pub registration: Option<RegistrationOutcome>,
}

impl EnclaveBinding {
    pub fn new(
        cid_source: EnclaveCidSource,
        port: u32,
        expected_pcrs: ExpectedPcrs,
        registration: Option<RegistrationCredentials>,
    ) -> Self {
        Self {
            cid_source,
            port,
            expected_pcrs,
            registration,
        }
    }

    /// Resolves the current enclave and asserts its key is registered.
    ///
    /// `register_enclave_key` is idempotent — it fetches the attestation over vsock, derives
    /// the signer, and returns [`RegistrationOutcome::AlreadyRegistered`] without sending a
    /// transaction when the registry already knows it. So the steady-state cost is one vsock
    /// round trip plus one `eth_call`, and the failure modes we care about (dead CID, swapped
    /// enclave, unregistered key) all surface here rather than as an unusable proof.
    pub async fn bind(&self) -> Result<EnclaveSession> {
        let cid = self.cid_source.resolve()?;
        let endpoint = EnclaveEndpoint::with_port(cid, self.port);

        let Some(credentials) = self.registration.clone() else {
            return Ok(EnclaveSession {
                prover: NitroProver::new(endpoint, self.expected_pcrs),
                cid,
                registration: None,
            });
        };

        let outcome = register_enclave_key(RegisterParams {
            enclave_cid: cid,
            enclave_port: self.port,
            expected_pcrs: self.expected_pcrs,
            l1_rpc_url: credentials.l1_rpc_url,
            registry: credentials.registry,
            private_key: credentials.private_key,
        })
        .await;

        match outcome {
            Ok(outcome) => {
                let label = match &outcome {
                    RegistrationOutcome::AlreadyRegistered => "already_registered",
                    RegistrationOutcome::Registered { tx_hash } => {
                        // Only ever logged when the enclave is new to the registry, which
                        // outside of startup means it was replaced underneath us.
                        info!(%tx_hash, enclave_cid = cid, "registered enclave key on-chain");
                        "registered"
                    }
                };
                world_chain_proof_metrics::increment_enclave_registration_attempts(label);
                world_chain_proof_metrics::set_enclave_key_registered(true);
                Ok(EnclaveSession {
                    prover: NitroProver::new(endpoint, self.expected_pcrs),
                    cid,
                    registration: Some(outcome),
                })
            }
            Err(error) => {
                world_chain_proof_metrics::increment_enclave_registration_attempts("failed");
                world_chain_proof_metrics::set_enclave_key_registered(false);
                warn!(?error, enclave_cid = cid, "failed to bind to enclave");
                Err(error.context(format!("failed to bind to enclave at cid {cid}")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn fixed_source_returns_its_cid() {
        assert_eq!(EnclaveCidSource::Fixed(16).resolve().unwrap(), 16);
    }

    /// The launcher writes the CID with a trailing newline.
    #[test]
    fn file_source_reads_and_trims() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "34").unwrap();
        let source = EnclaveCidSource::File(file.path().to_path_buf());
        assert_eq!(source.resolve().unwrap(), 34);
    }

    /// A replaced enclave must be observed, not cached: the same source resolves to the new
    /// CID after the launcher rewrites the file.
    #[test]
    fn file_source_observes_replacement() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let source = EnclaveCidSource::File(file.path().to_path_buf());
        fs::write(file.path(), "34\n").unwrap();
        assert_eq!(source.resolve().unwrap(), 34);
        fs::write(file.path(), "36\n").unwrap();
        assert_eq!(source.resolve().unwrap(), 36);
    }

    #[test]
    fn file_source_rejects_garbage() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "not-a-cid").unwrap();
        let source = EnclaveCidSource::File(file.path().to_path_buf());
        assert!(source.resolve().is_err());
    }

    #[test]
    fn file_source_errors_when_missing() {
        let source = EnclaveCidSource::File(PathBuf::from("/nonexistent/enclave-cid"));
        assert!(source.resolve().is_err());
    }
}
