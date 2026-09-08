//! SP1 validity-proof backend for the defender's [`ProofWorker`].

use std::{collections::HashMap, time::Duration};

use alloy_primitives::B256;
use alloy_sol_types::SolValue;
use anyhow::Context;
use world_chain_proof_core::artifacts::{AggregationProofArtifact, RangeProofArtifact};
use world_chain_proof_kona_host::online::{
    OnlineHostConfig, RangeWitnessRequest, build_range_input, fetch_l1_header_by_hash,
    is_witness_generation_timeout,
};
use world_chain_proof_protocol::ProofGameProvider;
use world_chain_proof_sp1_host::{
    SuccinctProverError, WorldSuccinctProver, aggregation_artifact_from_sp1_proof,
    range_artifact_from_sp1_proof,
};
use world_chain_proof_sp1_types::{AggregationSessionRequest, RangeProofRequest, Sp1ProofRequest};
use world_chain_proof_worker::{ClaimedProofJobHandler, ProofJob};
use world_chain_prover_service::{
    BackendSession, BackendSessionStatus, ProofBackend, ProofData, ProofRequest, SessionType,
};

use crate::planner::{RangePlan, RangePlanConfig, fetch_range_gas};

const NETWORK_REQUEST_RETRY_BACKOFFS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

/// Per-request deadline for the gas-planning JSON-RPC batches.
const GAS_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Configuration for [`Sp1Backend`].
#[derive(Clone, Copy, Debug)]
pub struct Sp1BackendConfig {
    /// Allow proving blocks newer than the finalized L2 head.
    pub allow_unfinalized: bool,
    /// Aggregation-program verification key embedded in this worker.
    pub aggregation_vkey: B256,
    /// Range-program verification-key commitment embedded in this worker.
    pub range_vkey_commitment: B256,
    /// How a proof interval is split into range proofs.
    pub range_plan: RangePlanConfig,
}

/// [`ClaimedProofJobHandler`] for the [`ProofBackend::Sp1`] lane: builds witnesses over RPC
/// and proves them with a [`WorldSuccinctProver`] (the sp1-sdk env prover in production).
pub struct Sp1Backend<P, G> {
    host: OnlineHostConfig,
    prover: P,
    game_provider: G,
    config: Sp1BackendConfig,
}

impl<P, G> Sp1Backend<P, G> {
    /// Creates a backend over the given RPC host config and SP1 prover.
    pub const fn new(
        host: OnlineHostConfig,
        prover: P,
        game_provider: G,
        config: Sp1BackendConfig,
    ) -> Self {
        Self {
            host,
            prover,
            game_provider,
            config,
        }
    }
}

#[async_trait::async_trait]
impl<P, G> ClaimedProofJobHandler for Sp1Backend<P, G>
where
    P: WorldSuccinctProver + Send + Sync + 'static,
    G: ProofGameProvider,
{
    fn lane(&self) -> ProofBackend {
        ProofBackend::Sp1
    }

    fn verifier_id(&self) -> B256 {
        self.config.aggregation_vkey
    }

    fn range_vkey_commitment(&self) -> Option<B256> {
        Some(self.config.range_vkey_commitment)
    }

    async fn handle_claimed_job(&self, job: ProofJob) -> anyhow::Result<ProofData> {
        let request = &job.request;
        let start_block = self.start_block(request).await?;

        let retrying_aggregation =
            if let Some(snark_session) = self.get_session(&job, SessionType::Snark).await? {
                match self
                    .wait_for_aggregation(&job, snark_session.backend_session_id)
                    .await
                    .context("failed to resume aggregation proof")?
                {
                    Some(agg) => {
                        check_artifact(request, &agg)?;
                        return Ok(proof_data_from_aggregation(agg));
                    }
                    None => true,
                }
            } else {
                false
            };

        let mut plan = self
            .load_range_plan(&job, start_block, request)
            .await
            .context("failed to plan range proofs")?;
        let ranges = self
            .prove_ranges(&job, &mut plan, request)
            .await
            .context("failed to complete range proofs")?;

        self.validate_range_artifacts(start_block, request, &ranges)?;

        let aggregation_request = self
            .build_aggregation_request(request, &ranges)
            .await
            .context("failed to build aggregation proof request")?;

        let agg = self
            .submit_aggregation_with_retries(&job, aggregation_request, retrying_aggregation)
            .await
            .context("failed to complete aggregation proof")?;

        check_artifact(request, &agg)?;

        Ok(proof_data_from_aggregation(agg))
    }
}

impl<P: WorldSuccinctProver + Send + Sync, G: ProofGameProvider> Sp1Backend<P, G> {
    async fn start_block(&self, request: &ProofRequest) -> anyhow::Result<u64> {
        let context = self
            .game_provider
            .proof_game_context(request.game)
            .await
            .context("failed to read proof game context")?;
        let start_block = context
            .validated_start_block(
                request.game,
                request.root_claim,
                request.l2_block_number,
                request.l1_head,
                self.host.rollup_config_hash,
            )
            .context("proof request does not match its game")?;
        tracing::debug!(
            proof_id = %request.id(),
            game_address = %request.game,
            block_interval = context.block_interval,
            pre_state_block = start_block,
            "validated SP1 proof range against game"
        );
        Ok(start_block)
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
        failure_reason: Option<String>,
    ) -> anyhow::Result<()> {
        if !self.prover.supports_persistent_sessions() {
            return Ok(());
        }

        job.sessions
            .record(session_type, session_id.to_string(), status, failure_reason)
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
            None,
        )
        .await?;

        Ok(session_id)
    }

    /// Resumes the recorded range plan, or plans ranges by cumulative gas.
    async fn load_range_plan(
        &self,
        job: &ProofJob,
        start_block: u64,
        request: &ProofRequest,
    ) -> anyhow::Result<RangePlan> {
        let end_block = request.l2_block_number;
        if let Some(stark_session) = self.get_session(job, SessionType::Stark).await? {
            let plan = RangePlan::decode(&stark_session.backend_session_id, start_block, end_block);
            if plan.covers(start_block, end_block) {
                tracing::debug!(
                    proof_id = %request.id(),
                    ranges = plan.ranges.len(),
                    "resuming recorded SP1 range plan"
                );
                return Ok(plan);
            }
            tracing::warn!(
                proof_id = %request.id(),
                pre_state_block = start_block,
                claimed_block = end_block,
                "recorded SP1 range plan does not cover the proof interval; replanning"
            );
        }

        if end_block.saturating_sub(start_block) <= 1 {
            return Ok(RangePlan::single(start_block, end_block, None));
        }

        let client = reqwest::Client::builder()
            .timeout(GAS_FETCH_TIMEOUT)
            .build()
            .context("failed to build gas fetch HTTP client")?;
        let gas = fetch_range_gas(&client, &self.host.l2_rpc, start_block, end_block)
            .await
            .context("failed to fetch per-block gas for range planning")?;
        let total_gas = gas.iter().fold(0u64, |sum, gas| sum.saturating_add(*gas));
        let plan = RangePlan::by_gas(start_block, end_block, &gas, &self.config.range_plan)?;
        tracing::info!(
            proof_id = %request.id(),
            pre_state_block = start_block,
            claimed_block = end_block,
            total_gas,
            ranges = plan.ranges.len(),
            "planned SP1 range proofs by gas"
        );
        Ok(plan)
    }

    /// Proves every planned range, bisecting ranges the network reports unexecutable and
    /// resubmitting timed-out requests, and returns the artifacts in block order.
    async fn prove_ranges(
        &self,
        job: &ProofJob,
        plan: &mut RangePlan,
        request: &ProofRequest,
    ) -> anyhow::Result<Vec<RangeProofArtifact>> {
        let max_splits = self.config.range_plan.max_range_splits;
        let mut artifacts: HashMap<(u64, u64), RangeProofArtifact> = HashMap::new();
        let mut resubmissions: HashMap<(u64, u64), usize> = HashMap::new();

        loop {
            // Submit every unsubmitted range up front so a network backend proves them
            // concurrently; local backends prove synchronously inside submit().
            for index in 0..plan.ranges.len() {
                if plan.ranges[index].session_id.is_some() {
                    continue;
                }
                let (range_start, range_end) =
                    (plan.ranges[index].start_block, plan.ranges[index].end_block);
                let range_request = self
                    .build_range_request(range_start, range_end, request)
                    .await
                    .context("failed to build range proof request")?;
                let session_id = self
                    .prover
                    .submit(Sp1ProofRequest::Range(range_request))
                    .await
                    .context("failed to submit range proof")?;
                plan.ranges[index].session_id = Some(session_id);
                self.persist_plan(job, plan, BackendSessionStatus::Running, None)
                    .await?;
            }

            // Wait on the first unproved range; fulfilled network requests resolve instantly.
            let Some(index) = plan
                .ranges
                .iter()
                .position(|range| !artifacts.contains_key(&(range.start_block, range.end_block)))
            else {
                break;
            };
            let range = plan.ranges[index].clone();
            let key = (range.start_block, range.end_block);
            let session_id = range
                .session_id
                .clone()
                .context("planned range lost its session id before waiting")?;

            let error = match self.prover.wait(&session_id).await {
                Ok(proof) => {
                    artifacts.insert(key, range_artifact_from_sp1_proof(&proof)?);
                    continue;
                }
                Err(error) => error,
            };
            match error.downcast_ref::<SuccinctProverError>() {
                Some(session_error) if session_error.should_resubmit() => {
                    let attempts = resubmissions.entry(key).or_insert(0);
                    if *attempts >= NETWORK_REQUEST_RETRY_BACKOFFS.len() {
                        self.persist_plan(
                            job,
                            plan,
                            BackendSessionStatus::Failed,
                            Some(session_error.to_string()),
                        )
                        .await?;
                        return Err(error).with_context(|| {
                            format!(
                                "range ({}, {}] exhausted {} SP1 Network resubmissions",
                                range.start_block,
                                range.end_block,
                                NETWORK_REQUEST_RETRY_BACKOFFS.len()
                            )
                        });
                    }
                    let delay = NETWORK_REQUEST_RETRY_BACKOFFS[*attempts];
                    *attempts += 1;
                    self.wait_before_resubmission(SessionType::Stark, *attempts, delay)
                        .await;
                    plan.ranges[index].session_id = None;
                    self.persist_plan(job, plan, BackendSessionStatus::Running, None)
                        .await?;
                }
                Some(session_error @ SuccinctProverError::RequestUnexecutable { .. }) => {
                    let split = (range.splits < max_splits)
                        .then(|| range.bisect())
                        .flatten();
                    let Some((low, high)) = split else {
                        self.persist_plan(
                            job,
                            plan,
                            BackendSessionStatus::Failed,
                            Some(session_error.to_string()),
                        )
                        .await?;
                        return Err(error).with_context(|| {
                            format!(
                                "range ({}, {}] is unexecutable and cannot be split further \
                                 (splits {}, max {max_splits})",
                                range.start_block, range.end_block, range.splits
                            )
                        });
                    };
                    world_chain_proof_metrics::increment_sp1_range_bisections();
                    tracing::warn!(
                        proof_id = %request.id(),
                        start_block = range.start_block,
                        end_block = range.end_block,
                        splits = low.splits,
                        backend_session_id = session_id,
                        "SP1 range proof is unexecutable; bisecting the range"
                    );
                    plan.ranges.splice(index..=index, [low, high]);
                    self.persist_plan(job, plan, BackendSessionStatus::Running, None)
                        .await?;
                }
                Some(session_error) if session_error.is_terminal_session() => {
                    self.persist_plan(
                        job,
                        plan,
                        BackendSessionStatus::Failed,
                        Some(session_error.to_string()),
                    )
                    .await?;
                    return Err(error).with_context(|| {
                        format!(
                            "range ({}, {}] failed terminally on the SP1 Network",
                            range.start_block, range.end_block
                        )
                    });
                }
                _ => return Err(error).context("failed to wait for range proof"),
            }
        }

        self.persist_plan(job, plan, BackendSessionStatus::Completed, None)
            .await?;
        plan.ranges
            .iter()
            .map(|range| {
                artifacts
                    .remove(&(range.start_block, range.end_block))
                    .context("proved range plan is missing an artifact")
            })
            .collect()
    }

    /// Records the encoded range plan into the job's Stark session slot.
    async fn persist_plan(
        &self,
        job: &ProofJob,
        plan: &RangePlan,
        status: BackendSessionStatus,
        failure_reason: Option<String>,
    ) -> anyhow::Result<()> {
        let encoded = plan.encode()?;
        self.record_session(job, SessionType::Stark, &encoded, status, failure_reason)
            .await
    }

    async fn wait_for_aggregation(
        &self,
        job: &ProofJob,
        session_id: String,
    ) -> anyhow::Result<Option<AggregationProofArtifact>> {
        let session_type = SessionType::Snark;
        match self.prover.wait(&session_id).await {
            Ok(proof) => {
                let artifact = aggregation_artifact_from_sp1_proof(&proof)?;
                self.record_session(
                    job,
                    session_type,
                    &session_id,
                    BackendSessionStatus::Completed,
                    None,
                )
                .await?;
                Ok(Some(artifact))
            }
            Err(error) => {
                self.handle_wait_error(job, session_type, &session_id, error)
                    .await?;
                Ok(None)
            }
        }
    }

    async fn handle_wait_error(
        &self,
        job: &ProofJob,
        session_type: SessionType,
        session_id: &str,
        error: anyhow::Error,
    ) -> anyhow::Result<()> {
        let Some(session_error) = error.downcast_ref::<SuccinctProverError>() else {
            return Err(error);
        };
        if !session_error.is_terminal_session() {
            return Err(error);
        }

        let should_resubmit = session_error.should_resubmit();
        let reason = session_error.to_string();
        self.record_session(
            job,
            session_type.clone(),
            session_id,
            BackendSessionStatus::Failed,
            Some(reason.clone()),
        )
        .await?;

        if !should_resubmit {
            return Err(error);
        }

        tracing::warn!(
            session_type = session_type.as_str(),
            backend_session_id = session_id,
            reason,
            "SP1 Network request timed out; scheduling a replacement request"
        );
        Ok(())
    }

    async fn submit_aggregation_with_retries(
        &self,
        job: &ProofJob,
        request: AggregationSessionRequest,
        retrying_existing: bool,
    ) -> anyhow::Result<AggregationProofArtifact> {
        for (resubmission, delay) in network_request_attempts(retrying_existing) {
            if let Some(delay) = delay {
                self.wait_before_resubmission(SessionType::Snark, resubmission, delay)
                    .await;
            }
            let session_id = self
                .submit_session(
                    job,
                    SessionType::Snark,
                    Sp1ProofRequest::Aggregation(request.clone()),
                )
                .await?;
            if let Some(artifact) = self.wait_for_aggregation(job, session_id).await? {
                return Ok(artifact);
            }
        }
        anyhow::bail!(
            "{} proof exhausted {} SP1 Network resubmissions",
            SessionType::Snark.as_str(),
            NETWORK_REQUEST_RETRY_BACKOFFS.len()
        );
    }

    async fn wait_before_resubmission(
        &self,
        session_type: SessionType,
        resubmission: usize,
        delay: Duration,
    ) {
        tracing::warn!(
            session_type = session_type.as_str(),
            resubmission,
            delay_seconds = delay.as_secs(),
            "waiting before resubmitting SP1 Network request"
        );
        tokio::time::sleep(delay).await;
    }

    async fn build_aggregation_request(
        &self,
        request: &ProofRequest,
        ranges: &[RangeProofArtifact],
    ) -> anyhow::Result<AggregationSessionRequest> {
        // Every range witness is pinned to the game's l1 head, so one header covers them all.
        let l1_header =
            fetch_l1_header_by_hash(&reqwest::Client::new(), &self.host.l1_rpc, request.l1_head)
                .await?;

        let l1_headers_cbor = serde_cbor::to_vec(&vec![l1_header])?;

        Ok(AggregationSessionRequest {
            transition_public_values: ranges
                .iter()
                .map(|range| range.transition_public_values.clone())
                .collect(),
            latest_l1_checkpoint_head: request.l1_head,
            l1_headers_cbor,
            range_proofs: ranges.iter().map(|range| range.proof.clone()).collect(),
        })
    }

    async fn build_range_request(
        &self,
        start_block: u64,
        end_block: u64,
        request: &ProofRequest,
    ) -> anyhow::Result<RangeProofRequest> {
        let witness_collection_started_at = std::time::Instant::now();
        let input = match build_range_input(
            &self.host,
            RangeWitnessRequest {
                start_block,
                end_block,
                l1_head: Some(request.l1_head),
                allow_unfinalized: self.config.allow_unfinalized,
            },
        )
        .await
        {
            Ok(input) => {
                world_chain_proof_metrics::record_witness_collection(
                    "sp1",
                    "success",
                    witness_collection_started_at.elapsed(),
                );
                input
            }
            Err(error) => {
                let outcome = if is_witness_generation_timeout(&error) {
                    "timeout"
                } else {
                    "error"
                };
                world_chain_proof_metrics::record_witness_collection(
                    "sp1",
                    outcome,
                    witness_collection_started_at.elapsed(),
                );
                return Err(error).context("failed to build SP1 range witness");
            }
        };

        let range_request = RangeProofRequest::from_witness_data(&input.witness)
            .context("failed to serialize SP1 range witness")?;

        Ok(range_request)
    }

    fn validate_range_artifacts(
        &self,
        start_block: u64,
        request: &ProofRequest,
        ranges: &[RangeProofArtifact],
    ) -> anyhow::Result<()> {
        let (Some(first), Some(last)) = (ranges.first(), ranges.last()) else {
            anyhow::bail!("no range proofs were produced");
        };

        if first.transition_public_values.l2PreBlockNumber != start_block {
            anyhow::bail!(
                "range proof pre block mismatch: expected {}, got {}",
                start_block,
                first.transition_public_values.l2PreBlockNumber,
            );
        }

        for pair in ranges.windows(2) {
            let (previous, current) = (
                &pair[0].transition_public_values,
                &pair[1].transition_public_values,
            );
            if previous.l2PostRoot != current.l2PreRoot
                || previous.l2PostBlockNumber != current.l2PreBlockNumber
            {
                anyhow::bail!(
                    "range proofs do not chain: block {} post root {:?} vs block {} pre root {:?}",
                    previous.l2PostBlockNumber,
                    previous.l2PostRoot,
                    current.l2PreBlockNumber,
                    current.l2PreRoot,
                );
            }
        }

        for artifact in ranges {
            if artifact.transition_public_values.l1Head != request.l1_head {
                anyhow::bail!(
                    "range proof l1 head mismatch: expected {:?}, got {:?}",
                    request.l1_head,
                    artifact.transition_public_values.l1Head,
                );
            }

            if artifact.transition_public_values.rollupConfigHash != self.host.rollup_config_hash {
                anyhow::bail!(
                    "range proof rollup config hash mismatch: expected {:?}, got {:?}",
                    self.host.rollup_config_hash,
                    artifact.transition_public_values.rollupConfigHash,
                );
            }
        }

        if last.transition_public_values.l2PostRoot != request.root_claim {
            anyhow::bail!(
                "range proof post root mismatch: expected {:?}, got {:?}",
                request.root_claim,
                last.transition_public_values.l2PostRoot,
            );
        }

        if last.transition_public_values.l2PostBlockNumber != request.l2_block_number {
            anyhow::bail!(
                "range proof block mismatch: expected {}, got {}",
                request.l2_block_number,
                last.transition_public_values.l2PostBlockNumber,
            );
        }

        Ok(())
    }
}

fn network_request_attempts(
    retrying_existing: bool,
) -> impl Iterator<Item = (usize, Option<Duration>)> {
    let first_resubmission = usize::from(retrying_existing);
    std::iter::once(None)
        .chain(NETWORK_REQUEST_RETRY_BACKOFFS.into_iter().map(Some))
        .skip(first_resubmission)
        .enumerate()
        .map(move |(offset, delay)| (first_resubmission + offset, delay))
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

/// Checks that the aggregation public values defend exactly the requested root.
fn check_artifact(
    request: &ProofRequest,
    artifact: &AggregationProofArtifact,
) -> Result<(), ArtifactMismatch> {
    let transition = &artifact.public_values.transitionPublicValues;
    if transition.l2PostRoot != request.root_claim {
        return Err(ArtifactMismatch::PostRoot {
            expected: request.root_claim,
            actual: transition.l2PostRoot,
        });
    }
    if transition.l2PostBlockNumber != request.l2_block_number {
        return Err(ArtifactMismatch::BlockNumber {
            expected: request.l2_block_number,
            actual: transition.l2PostBlockNumber,
        });
    }
    if transition.l1Head != request.l1_head {
        return Err(ArtifactMismatch::L1Head {
            expected: request.l1_head,
            actual: transition.l1Head,
        });
    }
    Ok(())
}

fn proof_data_from_aggregation(artifact: AggregationProofArtifact) -> ProofData {
    ProofData::Sp1 {
        proof: artifact.proof.into(),
        public_values: artifact.public_values.abi_encode().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_request_resubmissions_use_fixed_conservative_backoff() {
        assert_eq!(
            NETWORK_REQUEST_RETRY_BACKOFFS,
            [
                Duration::from_secs(60),
                Duration::from_secs(120),
                Duration::from_secs(300),
            ]
        );
    }

    #[test]
    fn request_attempts_are_finite_and_skip_completed_initial_attempt() {
        assert_eq!(
            network_request_attempts(false).collect::<Vec<_>>(),
            vec![
                (0, None),
                (1, Some(Duration::from_secs(60))),
                (2, Some(Duration::from_secs(120))),
                (3, Some(Duration::from_secs(300))),
            ]
        );
        assert_eq!(
            network_request_attempts(true).collect::<Vec<_>>(),
            vec![
                (1, Some(Duration::from_secs(60))),
                (2, Some(Duration::from_secs(120))),
                (3, Some(Duration::from_secs(300))),
            ]
        );
    }
}
