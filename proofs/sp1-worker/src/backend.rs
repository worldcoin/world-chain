//! SP1 validity-proof backend for the defender's [`ProofWorker`].

use std::time::Duration;

use alloy_primitives::{Address, B256};
use alloy_sol_types::SolValue;
use anyhow::Context;
use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};
use world_chain_proof_kona_host_utils::online::{
    OnlineHostConfig, RangeWitnessRequest, build_range_input, fetch_l1_header_by_hash,
};
use world_chain_proof_succinct_host_utils::{
    WorldSuccinctProver, aggregation_artifact_from_sp1_proof, range_artifact_from_sp1_proof,
};
use world_chain_proof_succinct_utils::{
    AggregationSessionRequest, RangeProofRequest, Sp1ProofRequest, Sp1SessionStatus,
};
use world_chain_proof_worker::{ClaimedProofJobHandler, ProofJob};
use world_chain_prover_service::{
    BackendSession, BackendSessionStatus, ProofBackend, ProofData, ProofRequest, SessionType,
};

/// Configuration for [`Sp1Backend`].
#[derive(Clone, Copy, Debug)]
pub struct Sp1BackendConfig {
    /// L2 blocks between a proposal's parent and its claimed block (the proof system's
    /// `blockInterval` domain constant). The proved range is
    /// `(l2_block_number - block_interval, l2_block_number]`.
    pub block_interval: u64,
    /// Number of equal-length sub-ranges proved independently before aggregation.
    pub split_count: u64,
    /// Prover address committed by the aggregation guest for on-chain attribution.
    pub prover_address: Address,
    /// Allow proving blocks newer than the finalized L2 head.
    pub allow_unfinalized: bool,
}

/// [`ClaimedProofJobHandler`] for the [`ProofBackend::Sp1`] lane: builds witnesses over RPC
/// and proves them with a [`WorldSuccinctProver`] (the sp1-sdk env prover in production).
pub struct Sp1Backend<P> {
    host: OnlineHostConfig,
    prover: P,
    config: Sp1BackendConfig,
}

impl<P> Sp1Backend<P> {
    /// Creates a backend over the given RPC host config and SP1 prover.
    pub const fn new(host: OnlineHostConfig, prover: P, config: Sp1BackendConfig) -> Self {
        Self {
            host,
            prover,
            config,
        }
    }
}

#[async_trait::async_trait]
impl<P> ClaimedProofJobHandler for Sp1Backend<P>
where
    P: WorldSuccinctProver + Send + Sync + 'static,
{
    fn lane(&self) -> ProofBackend {
        ProofBackend::Sp1
    }

    async fn handle_claimed_job(&self, job: ProofJob) -> anyhow::Result<ProofData> {
        let request = &job.request;
        let start_block = self.start_block(request)?;

        if let Some(snark_session) = self.get_session(&job, SessionType::Snark).await? {
            let agg = self
                .wait_and_download_aggregation(&job, snark_session.backend_session_id)
                .await
                .context("failed to resume aggregation proof")?;

            check_artifact(request, &agg)?;

            return Ok(proof_data_from_aggregation(agg));
        }

        let range = if let Some(stark_session) = self.get_session(&job, SessionType::Stark).await? {
            self.wait_and_download_range(&job, stark_session.backend_session_id)
                .await
                .context("failed to resume range proof")?
        } else {
            let range_request = self
                .build_range_request(start_block, request)
                .await
                .context("failed to build range proof request")?;

            let session_id = self
                .submit_session(
                    &job,
                    SessionType::Stark,
                    Sp1ProofRequest::Range(range_request),
                )
                .await
                .context("failed to submit range proof")?;

            self.wait_and_download_range(&job, session_id)
                .await
                .context("failed to complete range proof")?
        };

        self.validate_range_artifact(request, &range)?;

        let aggregation_request = self
            .build_aggregation_request(request, &range)
            .await
            .context("failed to build aggregation proof request")?;

        let snark_session_id = self
            .submit_session(
                &job,
                SessionType::Snark,
                Sp1ProofRequest::Aggregation(aggregation_request),
            )
            .await
            .context("failed to submit aggregation proof")?;

        let agg = self
            .wait_and_download_aggregation(&job, snark_session_id)
            .await
            .context("failed to complete aggregation proof")?;

        check_artifact(request, &agg)?;

        Ok(proof_data_from_aggregation(agg))
    }
}

impl<P: WorldSuccinctProver> Sp1Backend<P> {
    fn start_block(&self, request: &ProofRequest) -> anyhow::Result<u64> {
        request
            .l2_block_number
            .checked_sub(self.config.block_interval)
            .with_context(|| {
                format!(
                    "l2 block number {} is below the block interval {}",
                    request.l2_block_number, self.config.block_interval
                )
            })
    }

    async fn get_session(
        &self,
        job: &ProofJob,
        session_type: SessionType,
    ) -> anyhow::Result<Option<BackendSession>> {
        if !self.prover.supports_persistent_sessions() {
            return Ok(None);
        }

        Ok(job.sessions.get(session_type).await?)
    }

    async fn record_session(
        &self,
        job: &ProofJob,
        session_type: SessionType,
        session_id: &str,
        status: BackendSessionStatus,
    ) -> anyhow::Result<()> {
        if !self.prover.supports_persistent_sessions() {
            return Ok(());
        }

        job.sessions
            .record(session_type, session_id.to_string(), status)
            .await?;

        Ok(())
    }

    async fn submit_session(
        &self,
        job: &ProofJob,
        session_type: SessionType,
        request: Sp1ProofRequest,
    ) -> anyhow::Result<String> {
        let session_id = self.prover.submit(request).await?;

        self.record_session(
            job,
            session_type,
            &session_id,
            BackendSessionStatus::Running,
        )
        .await?;

        Ok(session_id)
    }

    async fn wait_and_download_range(
        &self,
        job: &ProofJob,
        session_id: String,
    ) -> anyhow::Result<RangeProofArtifact> {
        let session_type = SessionType::Stark;
        let session_label = session_type.as_str();
        loop {
            match self.prover.poll(&session_id).await? {
                Sp1SessionStatus::Running => {
                    // TODO: replace this hardcoded duration with a cli flag
                    let duration = Duration::from_secs(10);
                    tokio::time::sleep(duration).await;
                }
                Sp1SessionStatus::Completed => {
                    let proof = self.prover.download(&session_id).await?;
                    let artifact = range_artifact_from_sp1_proof(&proof)?;

                    self.record_session(
                        job,
                        session_type.clone(),
                        &session_id,
                        BackendSessionStatus::Completed,
                    )
                    .await?;

                    return Ok(artifact);
                }
                Sp1SessionStatus::Failed(reason) => {
                    self.record_session(
                        job,
                        session_type.clone(),
                        &session_id,
                        BackendSessionStatus::Failed,
                    )
                    .await?;

                    anyhow::bail!("{session_label} proof session {session_id} failed: {reason}");
                }
                Sp1SessionStatus::NotFound => {
                    anyhow::bail!("{session_label} proof session {session_id} not found by prover");
                }
            }
        }
    }

    async fn wait_and_download_aggregation(
        &self,
        job: &ProofJob,
        session_id: String,
    ) -> anyhow::Result<AggregationProofArtifact> {
        let session_type = SessionType::Snark;
        let session_label = session_type.as_str();
        loop {
            match self.prover.poll(&session_id).await? {
                Sp1SessionStatus::Running => {
                    // TODO: replace this hardcoded duration with a cli flag
                    let duration = Duration::from_secs(10);
                    tokio::time::sleep(duration).await;
                }
                Sp1SessionStatus::Completed => {
                    let proof = self.prover.download(&session_id).await?;
                    let artifact = aggregation_artifact_from_sp1_proof(&proof)?;

                    self.record_session(
                        job,
                        session_type.clone(),
                        &session_id,
                        BackendSessionStatus::Completed,
                    )
                    .await?;

                    return Ok(artifact);
                }
                Sp1SessionStatus::Failed(reason) => {
                    self.record_session(
                        job,
                        session_type.clone(),
                        &session_id,
                        BackendSessionStatus::Failed,
                    )
                    .await?;

                    anyhow::bail!("{session_label} proof session {session_id} failed: {reason}");
                }
                Sp1SessionStatus::NotFound => {
                    anyhow::bail!("{session_label} proof session {session_id} not found by prover");
                }
            }
        }
    }

    async fn build_aggregation_request(
        &self,
        request: &ProofRequest,
        range: &RangeProofArtifact,
    ) -> anyhow::Result<AggregationSessionRequest> {
        let l1_header =
            fetch_l1_header_by_hash(&reqwest::Client::new(), &self.host.l1_rpc, request.l1_head)
                .await?;

        let l1_headers_cbor = serde_cbor::to_vec(&vec![l1_header])?;

        Ok(AggregationSessionRequest {
            boot_infos: vec![range.boot_info.clone()],
            latest_l1_checkpoint_head: request.l1_head,
            prover_address: self.config.prover_address,
            l1_headers_cbor,
            range_proofs: vec![range.proof.clone()],
        })
    }

    async fn build_range_request(
        &self,
        start_block: u64,
        request: &ProofRequest,
    ) -> anyhow::Result<RangeProofRequest> {
        let input = build_range_input(
            &self.host,
            RangeWitnessRequest {
                start_block,
                end_block: request.l2_block_number,
                l1_head: Some(request.l1_head),
                allow_unfinalized: self.config.allow_unfinalized,
            },
        )
        .await
        .context("failed to build SP1 range witness")?;

        let range_request = RangeProofRequest::from_witness_data(&input.witness)
            .context("failed to serialize SP1 range witness")?;

        Ok(range_request)
    }

    fn validate_range_artifact(
        &self,
        request: &ProofRequest,
        artifact: &RangeProofArtifact,
    ) -> anyhow::Result<()> {
        if artifact.boot_info.l2PostRoot != request.root_claim {
            anyhow::bail!(
                "range proof post root mismatch: expected {:?}, got {:?}",
                request.root_claim,
                artifact.boot_info.l2PostRoot,
            );
        }

        if artifact.boot_info.l2BlockNumber != request.l2_block_number {
            anyhow::bail!(
                "range proof block mismatch: expected {}, got {}",
                request.l2_block_number,
                artifact.boot_info.l2BlockNumber,
            );
        }

        if artifact.boot_info.l1Head != request.l1_head {
            anyhow::bail!(
                "range proof l1 head mismatch: expected {:?}, got {:?}",
                request.l1_head,
                artifact.boot_info.l1Head,
            );
        }

        if artifact.boot_info.rollupConfigHash != self.host.rollup_config_hash {
            anyhow::bail!(
                "range proof rollup config hash mismatch: expected {:?}, got {:?}",
                self.host.rollup_config_hash,
                artifact.boot_info.rollupConfigHash,
            );
        }

        Ok(())
    }
}

/// A proof artifact whose committed outputs do not defend the requested root.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum ArtifactMismatch {
    #[error("aggregation post root {actual:?} does not match root claim {expected:?}")]
    PostRoot { expected: B256, actual: B256 },
    #[error("aggregation block number {actual} does not match request {expected}")]
    BlockNumber { expected: u64, actual: u64 },
    #[error("aggregation l1 head {actual:?} does not match request {expected:?}")]
    L1Head { expected: B256, actual: B256 },
}

/// Checks that the aggregation outputs defend exactly the requested root.
fn check_artifact(
    request: &ProofRequest,
    artifact: &AggregationProofArtifact,
) -> Result<(), ArtifactMismatch> {
    let outputs = &artifact.outputs;
    if outputs.l2PostRoot != request.root_claim {
        return Err(ArtifactMismatch::PostRoot {
            expected: request.root_claim,
            actual: outputs.l2PostRoot,
        });
    }
    if outputs.l2BlockNumber != request.l2_block_number {
        return Err(ArtifactMismatch::BlockNumber {
            expected: request.l2_block_number,
            actual: outputs.l2BlockNumber,
        });
    }
    if outputs.l1Head != request.l1_head {
        return Err(ArtifactMismatch::L1Head {
            expected: request.l1_head,
            actual: outputs.l1Head,
        });
    }
    Ok(())
}

fn proof_data_from_aggregation(artifact: AggregationProofArtifact) -> ProofData {
    ProofData::Sp1 {
        proof: artifact.proof.into(),
        public_values: artifact.outputs.abi_encode().into(),
    }
}
