//! [`WorldSuccinctProver`] backed by the sp1-sdk `EnvProver` (local CPU, mock, or the Succinct
//! proving network).
//!
//! Range proofs are produced in `Compressed` mode so the aggregation guest can recursively
//! verify them; the aggregation proof mode is configurable (Groth16 for on-chain verification).

use anyhow::Context;
use sp1_sdk::{
    CpuProver, HashableKey, MockProver, ProveRequest, Prover, ProverClient, ProvingKey, SP1Proof,
    SP1Stdin,
    env::{EnvProver, EnvProvingKey},
};

use alloy_primitives::B256;
pub use sp1_sdk::SP1ProofMode;
use world_chain_proof_core::{
    artifacts::{AggregationProofArtifact, RangeProofArtifact},
    boot::BootInfoStruct,
    types::{AggregationOutputs, u32_to_u8},
};
use world_chain_proof_succinct_utils::{
    AggregationProofRequest, RangeProofRequest, WorldSuccinctProver,
};

/// Which sp1-sdk prover backs an [`EnvSuccinctProver`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sp1ProverKind {
    /// Local CPU prover (requires 32–128 GB RAM).
    Cpu,
    /// Mock prover — no real ZK, for integration testing only.
    Mock,
    /// Succinct proving network (requires `SP1_PRIVATE_KEY`).
    Network,
}

impl std::str::FromStr for Sp1ProverKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cpu" => Ok(Self::Cpu),
            "mock" => Ok(Self::Mock),
            "network" => Ok(Self::Network),
            other => Err(anyhow::anyhow!(
                "unknown sp1 prover kind {other:?} (expected cpu, mock, or network)"
            )),
        }
    }
}

/// Structured failures specific to [`EnvSuccinctProver`]; surfaced wrapped in
/// [`anyhow::Error`] so callers can downcast when they need to match on them.
#[derive(Debug, thiserror::Error)]
pub enum EnvSuccinctProverError {
    /// The guest committed boot info that differs from the host-computed expectation.
    #[error("range proof boot info mismatch: expected {expected:?}, got {actual:?}")]
    BootInfoMismatch {
        expected: Box<BootInfoStruct>,
        actual: Box<BootInfoStruct>,
    },
    /// Aggregation requires compressed range proofs for recursive verification.
    #[error("range proof was not in compressed mode")]
    NotCompressed,
}

/// [`WorldSuccinctProver`] implementation over the sp1-sdk environment provers.
///
/// Synchronous like the trait it implements: holds its own Tokio runtime and blocks on the
/// async sp1-sdk calls, mirroring `NitroProver`. Construct and call it from blocking-capable
/// threads only.
pub struct EnvSuccinctProver {
    kind: Sp1ProverKind,
    client: EnvProver,
    range_pk: EnvProvingKey,
    agg_pk: EnvProvingKey,
    multi_block_vkey: [u32; 8],
    agg_mode: SP1ProofMode,
    // `Option` so [`Drop`] can take and shut it down without blocking: a `Runtime` dropped
    // inside an async context (e.g. when the worker task is aborted) otherwise panics.
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for EnvSuccinctProver {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            // Non-blocking shutdown; avoids "Cannot drop a runtime in a context where blocking
            // is not allowed" when the owning worker future is dropped on an async runtime.
            runtime.shutdown_background();
        }
    }
}

impl EnvSuccinctProver {
    /// Creates the prover and runs SP1 setup for the range and aggregation ELFs.
    pub fn new(
        kind: Sp1ProverKind,
        range_elf: Vec<u8>,
        agg_elf: Vec<u8>,
        agg_mode: SP1ProofMode,
    ) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let (client, range_pk, agg_pk) = runtime.block_on(async {
            let client = match kind {
                Sp1ProverKind::Cpu => EnvProver::Cpu(CpuProver::new().await),
                Sp1ProverKind::Mock => EnvProver::Mock(MockProver::new().await),
                Sp1ProverKind::Network => {
                    EnvProver::Network(ProverClient::builder().network().build().await)
                }
            };
            let range_pk = client
                .setup(range_elf.into())
                .await
                .context("range program setup failed")?;
            let agg_pk = client
                .setup(agg_elf.into())
                .await
                .context("aggregation program setup failed")?;
            anyhow::Ok((client, range_pk, agg_pk))
        })?;
        let multi_block_vkey = range_pk.verifying_key().hash_u32();

        Ok(Self {
            kind,
            client,
            range_pk,
            agg_pk,
            multi_block_vkey,
            agg_mode,
            runtime: Some(runtime),
        })
    }

    /// The owned Tokio runtime; present for the prover's whole lifetime (taken only on drop).
    fn rt(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("EnvSuccinctProver runtime is present until drop")
    }

    /// The aggregation program's verification key, as the `bytes32` digest the on-chain
    /// SP1 verifier expects as its `programVKey`.
    pub fn aggregation_vkey(&self) -> B256 {
        self.agg_pk.verifying_key().bytes32_raw().into()
    }

    /// The range program's vkey commitment (`multiBlockVKey`), as `bytes32`. This is the value
    /// the aggregation proof commits to and the on-chain verifier pins.
    pub fn range_vkey_commitment(&self) -> B256 {
        B256::from(u32_to_u8(self.multi_block_vkey))
    }
}

impl WorldSuccinctProver for EnvSuccinctProver {
    type Error = anyhow::Error;

    fn multi_block_vkey(&self) -> [u32; 8] {
        self.multi_block_vkey
    }

    fn prove_range(&self, request: RangeProofRequest) -> Result<RangeProofArtifact, Self::Error> {
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(request.witness_rkyv);

        let proof = self
            .rt()
            .block_on(async { self.client.prove(&self.range_pk, stdin).compressed().await })
            .context("range proving failed")?;

        let boot_info: BootInfoStruct = bincode::deserialize(proof.public_values.as_slice())
            .context("range proof public values deserialization failed")?;

        if let Some(expected) = request.expected_public_values {
            let expected_boot = BootInfoStruct::from(expected.boot_info);
            if expected_boot != boot_info {
                return Err(EnvSuccinctProverError::BootInfoMismatch {
                    expected: Box::new(expected_boot),
                    actual: Box::new(boot_info),
                }
                .into());
            }
        }

        let proof_bytes = bincode::serialize(&proof).context("range proof serialization failed")?;

        Ok(RangeProofArtifact {
            boot_info,
            proof: proof_bytes,
        })
    }

    fn prove_aggregation(
        &self,
        request: AggregationProofRequest,
    ) -> Result<AggregationProofArtifact, Self::Error> {
        let mut stdin = SP1Stdin::new();
        let range_vk = self.range_pk.verifying_key().vk.clone();
        for proof_bytes in &request.range_proofs {
            let proof: sp1_sdk::SP1ProofWithPublicValues =
                bincode::deserialize(proof_bytes).context("range proof deserialization failed")?;
            let SP1Proof::Compressed(inner) = proof.proof else {
                return Err(EnvSuccinctProverError::NotCompressed.into());
            };
            stdin.write_proof(*inner, range_vk.clone());
        }
        stdin.write(&request.inputs);
        stdin.write_vec(request.l1_headers_cbor);

        let proof = self
            .rt()
            .block_on(async {
                let mut prove = self.client.prove(&self.agg_pk, stdin).mode(self.agg_mode);
                if self.kind == Sp1ProverKind::Mock {
                    // Mock range proofs are dummies; skip recursive verification in the guest.
                    prove = prove.deferred_proof_verification(false);
                }
                prove.await
            })
            .context("aggregation proving failed")?;

        if self.kind != Sp1ProverKind::Mock {
            self.client
                .verify(&proof, self.agg_pk.verifying_key(), None)
                .context("aggregation proof verification failed")?;
        }

        let outputs = <AggregationOutputs as alloy_sol_types::SolValue>::abi_decode(
            proof.public_values.as_slice(),
        )
        .context("aggregation outputs abi decoding failed")?;

        // Groth16/Plonk proofs serialize to their on-chain calldata representation; other
        // modes (mock runs, compressed) keep the full sdk proof for offline use.
        let proof_bytes = match &proof.proof {
            SP1Proof::Groth16(_) | SP1Proof::Plonk(_) => proof.bytes(),
            _ => bincode::serialize(&proof).context("aggregation proof serialization failed")?,
        };

        Ok(AggregationProofArtifact {
            outputs,
            proof: proof_bytes,
        })
    }
}
