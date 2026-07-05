//! [`WorldSuccinctProver`] backed by the sp1-sdk `EnvProver` (local CPU, mock, or the Succinct
//! proving network).
//!
//! Range proofs are produced in `Compressed` mode so the aggregation guest can recursively
//! verify them; the aggregation proof mode is configurable (Groth16 for on-chain verification).

use alloy_primitives::B256;
use anyhow::Context;
use sp1_sdk::{
    CpuProver, HashableKey, MockProver, ProveRequest, Prover, ProverClient, ProvingKey, SP1Proof,
    SP1ProofWithPublicValues, SP1ProvingKey, SP1Stdin,
    env::{EnvProver, EnvProvingKey},
};

pub use sp1_sdk::SP1ProofMode;
use world_chain_proof_core::{
    artifacts::{AggregationProofArtifact, RangeProofArtifact},
    boot::BootInfoStruct,
    types::AggregationOutputs,
};
use world_chain_proof_succinct_utils::{
    AggregationProofRequest, RangeProofRequest, WorldSuccinctProver,
};

/// Which sp1-sdk prover backs an [`SuccinctProver`].
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

/// Structured failures specific to [`SuccinctProver`]; surfaced wrapped in
/// [`anyhow::Error`] so callers can downcast when they need to match on them.
#[derive(Debug, thiserror::Error)]
pub enum SuccinctProverError {
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
pub struct SuccinctProver {
    kind: Sp1ProverKind,
    client: EnvProver,
    range_pk: EnvProvingKey,
    agg_pk: EnvProvingKey,
    network_client: Option<sp1_sdk::NetworkProver>,
    range_network_pk: Option<SP1ProvingKey>,
    agg_network_pk: Option<SP1ProvingKey>,
    multi_block_vkey: [u32; 8],
    agg_mode: SP1ProofMode,
    runtime: tokio::runtime::Runtime,
}

impl SuccinctProver {
    /// Creates the prover using caller-supplied ELFs. Use this in production binaries with
    /// ELFs embedded at compile time via `world_chain_proof_succinct_elfs`.
    pub fn new(kind: Sp1ProverKind, agg_mode: SP1ProofMode) -> anyhow::Result<Self> {
        let range_elf = world_chain_proof_succinct_elfs::range_elf();
        let agg_elf = world_chain_proof_succinct_elfs::aggregation_elf();
        let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let (client, range_pk, agg_pk, network_client, range_network_pk, agg_network_pk) = runtime
            .block_on(async {
                let client = match kind {
                    Sp1ProverKind::Cpu => EnvProver::Cpu(CpuProver::new().await),
                    Sp1ProverKind::Mock => EnvProver::Mock(MockProver::new().await),
                    Sp1ProverKind::Network => {
                        let private_key = std::env::var("SP1_PRIVATE_KEY").context(
                            "SP1_PRIVATE_KEY environment variable must be set to use the network prover",
                        )?;
                        EnvProver::Network(
                            ProverClient::builder()
                                .network()
                                .private_key(&private_key)
                                .build()
                                .await,
                        )
                    }
                };
                let range_pk = client
                    .setup(range_elf.clone())
                    .await
                    .context("range program setup failed")?;
                let agg_pk = client
                    .setup(agg_elf.clone())
                    .await
                    .context("aggregation program setup failed")?;
                let (network_client, range_network_pk, agg_network_pk) = match &client {
                    EnvProver::Network(network) => {
                        let range_network_pk = network
                            .setup(range_elf)
                            .await
                            .context("network range program setup failed")?;
                        let agg_network_pk = network
                            .setup(agg_elf)
                            .await
                            .context("network aggregation program setup failed")?;
                        (
                            Some(network.clone()),
                            Some(range_network_pk),
                            Some(agg_network_pk),
                        )
                    }
                    _ => (None, None, None),
                };
                anyhow::Ok((
                    client,
                    range_pk,
                    agg_pk,
                    network_client,
                    range_network_pk,
                    agg_network_pk,
                ))
            })?;
        let multi_block_vkey = range_pk.verifying_key().hash_u32();

        Ok(Self {
            kind,
            client,
            range_pk,
            agg_pk,
            network_client,
            range_network_pk,
            agg_network_pk,
            multi_block_vkey,
            agg_mode,
            runtime,
        })
    }

    fn network_parts(
        &self,
    ) -> anyhow::Result<(&sp1_sdk::NetworkProver, &SP1ProvingKey, &SP1ProvingKey)> {
        let client = self
            .network_client
            .as_ref()
            .context("SP1 prover is not configured for network requests")?;
        let range_pk = self
            .range_network_pk
            .as_ref()
            .context("missing network range proving key")?;
        let agg_pk = self
            .agg_network_pk
            .as_ref()
            .context("missing network aggregation proving key")?;
        Ok((client, range_pk, agg_pk))
    }
}

impl WorldSuccinctProver for SuccinctProver {
    type Error = anyhow::Error;

    fn multi_block_vkey(&self) -> [u32; 8] {
        self.multi_block_vkey
    }

    fn prove_range(&self, request: RangeProofRequest) -> Result<RangeProofArtifact, Self::Error> {
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(request.witness_rkyv);

        let proof = self
            .runtime
            .block_on(async { self.client.prove(&self.range_pk, stdin).compressed().await })
            .context("range proving failed")?;

        let boot_info: BootInfoStruct = bincode::deserialize(proof.public_values.as_slice())
            .context("range proof public values deserialization failed")?;

        if let Some(expected) = request.expected_public_values {
            let expected_boot = BootInfoStruct::from(expected.boot_info);
            if expected_boot != boot_info {
                return Err(SuccinctProverError::BootInfoMismatch {
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
                return Err(SuccinctProverError::NotCompressed.into());
            };
            stdin.write_proof(*inner, range_vk.clone());
        }
        stdin.write(&request.inputs);
        stdin.write_vec(request.l1_headers_cbor);

        let proof = self
            .runtime
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

    fn supports_async_requests(&self) -> bool {
        self.kind == Sp1ProverKind::Network
    }

    fn request_range(&self, request: RangeProofRequest) -> Result<B256, Self::Error> {
        let (network, range_pk, _) = self.network_parts()?;
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(request.witness_rkyv);
        self.runtime
            .block_on(async { network.prove(range_pk, stdin).compressed().request().await })
            .context("range proof network request failed")
    }

    fn poll_range(&self, id: B256) -> Result<Option<RangeProofArtifact>, Self::Error> {
        let (network, _, _) = self.network_parts()?;
        let maybe_proof = self
            .runtime
            .block_on(async { network.get_proof_status(id).await })
            .context("range proof network status failed")?
            .1;

        maybe_proof
            .map(range_artifact_from_proof)
            .transpose()
            .context("range proof conversion failed")
    }

    fn request_aggregation(&self, request: AggregationProofRequest) -> Result<B256, Self::Error> {
        let (network, range_pk, agg_pk) = self.network_parts()?;
        let mut stdin = SP1Stdin::new();
        let range_vk = range_pk.verifying_key().vk.clone();
        for proof_bytes in &request.range_proofs {
            let proof: sp1_sdk::SP1ProofWithPublicValues =
                bincode::deserialize(proof_bytes).context("range proof deserialization failed")?;
            let SP1Proof::Compressed(inner) = proof.proof else {
                return Err(SuccinctProverError::NotCompressed.into());
            };
            stdin.write_proof(*inner, range_vk.clone());
        }
        stdin.write(&request.inputs);
        stdin.write_vec(request.l1_headers_cbor);

        self.runtime
            .block_on(async {
                network
                    .prove(agg_pk, stdin)
                    .mode(self.agg_mode)
                    .request()
                    .await
            })
            .context("aggregation proof network request failed")
    }

    fn poll_aggregation(&self, id: B256) -> Result<Option<AggregationProofArtifact>, Self::Error> {
        let (network, _, agg_pk) = self.network_parts()?;
        let maybe_proof = self
            .runtime
            .block_on(async { network.get_proof_status(id).await })
            .context("aggregation proof network status failed")?
            .1;

        let Some(proof) = maybe_proof else {
            return Ok(None);
        };

        network
            .verify(&proof, agg_pk.verifying_key(), None)
            .context("aggregation proof verification failed")?;
        aggregation_artifact_from_proof(&proof)
            .map(Some)
            .context("aggregation proof conversion failed")
    }
}

fn range_artifact_from_proof(
    proof: SP1ProofWithPublicValues,
) -> anyhow::Result<RangeProofArtifact> {
    let boot_info: BootInfoStruct = bincode::deserialize(proof.public_values.as_slice())
        .context("range proof public values deserialization failed")?;
    let proof_bytes = bincode::serialize(&proof).context("range proof serialization failed")?;
    Ok(RangeProofArtifact {
        boot_info,
        proof: proof_bytes,
    })
}

fn aggregation_artifact_from_proof(
    proof: &SP1ProofWithPublicValues,
) -> anyhow::Result<AggregationProofArtifact> {
    let outputs = <AggregationOutputs as alloy_sol_types::SolValue>::abi_decode(
        proof.public_values.as_slice(),
    )
    .context("aggregation outputs abi decoding failed")?;
    let proof_bytes = match &proof.proof {
        SP1Proof::Groth16(_) | SP1Proof::Plonk(_) => proof.bytes(),
        _ => bincode::serialize(proof).context("aggregation proof serialization failed")?,
    };
    Ok(AggregationProofArtifact {
        outputs,
        proof: proof_bytes,
    })
}
