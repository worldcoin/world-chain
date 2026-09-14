use crate::{
    error::DefenderError,
    traits::DefenderClient,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, Bytes, U256};
use alloy_provider::{PendingTransactionBuilder, Provider};
use async_trait::async_trait;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tracing::warn;
use world_chain_proof_protocol::{
    ClaimData, IAnchorStateRegistry, IDisputeGameFactory, IMultiProofGame, LineageAnchor,
    LineageError, LineageGame, LineageProvider, LineageTransition, PROOF_LANE_COUNT, ProofLane,
    RegisteredLineageConfig, ResolutionStatus, encode_compact_proof, read_game_for_transition,
    read_lineage_anchor, read_lineage_resolution_status, read_registered_lineage_config,
};

/// Alloy-backed implementation of [`DefenderClient`].
///
/// Binds the stock OP Stack `DisputeGameFactory` and the anchor registry configured by its
/// registered WIP-1006 implementation.
#[derive(Debug, Clone)]
pub struct AlloyDefenderClient<P> {
    factory: IDisputeGameFactory::IDisputeGameFactoryInstance<P>,
    anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance<P>,
    registered: RegisteredLineageConfig,
    confirmations: u64,
    receipt_timeout: Duration,
    /// Credited this lane's share of a forfeited challenger bond when the game resolves.
    reward_recipient: Address,
    semaphore: Arc<Semaphore>,
    provider: P,
    submission_provider: P,
}

impl<P> AlloyDefenderClient<P>
where
    P: Provider + Clone,
{
    /// Connects to the registered WIP-1006 implementation and its anchor registry.
    pub async fn new(
        provider: P,
        factory_address: Address,
        confirmations: u64,
        receipt_timeout: Duration,
        reward_recipient: Address,
    ) -> Result<Self, DefenderError> {
        let factory = IDisputeGameFactory::IDisputeGameFactoryInstance::new(
            factory_address,
            provider.clone(),
        );
        let registered = read_registered_lineage_config(&provider, &factory).await?;
        let anchor = IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
            registered.anchor_registry,
            provider.clone(),
        );
        let semaphore = Arc::new(Semaphore::new(1));

        Ok(Self {
            factory,
            anchor,
            registered,
            confirmations,
            receipt_timeout,
            reward_recipient,
            semaphore,
            submission_provider: provider.clone(),
            provider,
        })
    }

    /// Uses a separate provider for proof estimation and submission, without read-RPC fallback.
    /// It must use the same chain and signer as the read provider.
    pub fn with_submission_provider(mut self, provider: P) -> Self {
        self.submission_provider = provider;
        self
    }

    fn game(&self, address: Address) -> IMultiProofGame::IMultiProofGameInstance<P> {
        IMultiProofGame::IMultiProofGameInstance::new(address, self.provider.clone())
    }
}

#[async_trait]
impl<P> LineageProvider for AlloyDefenderClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    fn lineage_block_interval(&self) -> u64 {
        self.registered.block_interval
    }

    async fn lineage_anchor(&self) -> Result<LineageAnchor, LineageError> {
        read_lineage_anchor(&self.provider, &self.anchor).await
    }

    async fn game_for_transition(
        &self,
        transition: LineageTransition,
    ) -> Result<Option<LineageGame>, LineageError> {
        read_game_for_transition(&self.factory, self.registered.domain_hash, transition).await
    }

    async fn lineage_resolution_status(
        &self,
        game: Address,
    ) -> Result<ResolutionStatus, LineageError> {
        read_lineage_resolution_status(&self.game(game)).await
    }
}

#[async_trait]
impl<P> DefenderClient for AlloyDefenderClient<P>
where
    P: Provider + Clone + Send + Sync + 'static,
{
    async fn game_metadata(&self, address: Address) -> Result<GameMetadata, DefenderError> {
        let game = self.game(address);
        let (
            domain_hash,
            aggregation_vkey,
            range_vkey_commitment,
            tee_image_id,
            parent_ref,
            root_claim,
            l2_block_number,
            l1_origin_hash,
            l1_origin_number,
            challenge_deadline,
            proof_deadline,
            proof_threshold,
        ) = self
            .provider
            .multicall()
            .add(game.proposalDomainHash())
            .add(game.aggregationVKey())
            .add(game.rangeVKeyCommitment())
            .add(game.teeImageId())
            .add(game.parentRef())
            .add(game.rootClaim())
            .add(game.l2SequenceNumber())
            .add(game.l1Head())
            .add(game.l1OriginNumber())
            .add(game.challengeDeadline())
            .add(game.proofDeadline())
            .add(game.PROOF_THRESHOLD())
            .aggregate()
            .await?;
        if proof_threshold == 0 || proof_threshold > PROOF_LANE_COUNT {
            return Err(DefenderError::InvalidProofThreshold {
                proof_threshold,
                game: address,
            });
        }

        Ok(GameMetadata {
            address,
            domain_hash,
            aggregation_vkey,
            range_vkey_commitment,
            tee_image_id,
            parent_ref,
            root_claim,
            l2_block_number: u256_to_u64(l2_block_number)?,
            l1_origin_hash,
            l1_origin_number: u256_to_u64(l1_origin_number)?,
            challenge_deadline,
            proof_deadline,
            proof_threshold,
        })
    }

    async fn claim_data(&self, address: Address) -> Result<ClaimData, DefenderError> {
        let claim = self.game(address).claimData().call().await?;
        Ok(ClaimData {
            status: claim.status.try_into()?,
            challenger: claim.challenger,
            deadline: claim.deadline,
            proof_bitmap: claim.proofBitmap,
            invalidation_reason: claim.invalidationReason.try_into()?,
        })
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: ProofLane,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
        let _permit = self.semaphore.acquire().await?;
        if self.game(game).claimData().call().await?.proofBitmap & lane.mask() != 0 {
            return Err(DefenderError::LaneAlreadyProven { game, lane });
        }
        let started = Instant::now();
        let compact = encode_compact_proof(lane, self.reward_recipient, &proof);
        // Gas estimation also carries the proof, so it must use the submission provider.
        let pending =
            IMultiProofGame::IMultiProofGameInstance::new(game, self.submission_provider.clone())
                .submitProofLane(compact)
                .send()
                .await
                .map_err(|error| {
                    warn!(lifecycle_event = "proof_submission_failed", game_address = %game,
                        ?lane, %error, "proof submission failed after an empty-lane check; possible competing proof or frontrun if the lane changed");
                    DefenderError::from(error)
                })?;
        let tx_hash = *pending.tx_hash();
        let wait_started = Instant::now();
        let wait_for_receipt = |confirmations| {
            PendingTransactionBuilder::new(self.provider.root().clone(), tx_hash)
                .with_required_confirmations(confirmations)
                .with_timeout(Some(
                    self.receipt_timeout.saturating_sub(wait_started.elapsed()),
                ))
                .get_receipt()
        };
        let warn_failure = |error| {
            warn!(lifecycle_event = "proof_submission_failed", game_address = %game,
                ?lane, %tx_hash, %error,
                "proof confirmation failed; possible competing proof or frontrun, but timeout/RPC errors are inconclusive");
            error
        };
        let mut receipt = wait_for_receipt(1)
            .await
            .map_err(|error| warn_failure(DefenderError::from(error)))?;
        world_chain_proof_metrics::record_proof_submission_inclusion(started.elapsed());
        if self.confirmations > 1 {
            receipt = wait_for_receipt(self.confirmations)
                .await
                .map_err(|error| warn_failure(DefenderError::from(error)))?;
        }
        if !receipt.status() {
            return Err(warn_failure(DefenderError::Revert(tx_hash)));
        }
        world_chain_proof_metrics::refresh_wallet_balance(&self.provider, receipt.from).await;
        Ok(DefenderSubmission { tx_hash })
    }
}

fn u256_to_u64(value: U256) -> Result<u64, DefenderError> {
    value.try_into().map_err(|_| DefenderError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use alloy_provider::ProviderBuilder;
    use alloy_sol_types::SolValue;
    use alloy_transport::mock::Asserter;

    #[derive(Clone, Default)]
    struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn push_claim(reads: &Asserter, status: u8, bitmap: u8) {
        reads.push_success(&Bytes::from(
            (
                U256::from(status),
                Address::ZERO,
                U256::from(100),
                U256::from(bitmap),
                U256::ZERO,
            )
                .abi_encode(),
        ));
    }

    #[tokio::test]
    async fn already_proven_lane_skips_submission_without_warning() {
        let logs = LogWriter::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let reads = Asserter::new();
        let submissions = Asserter::new();
        let lane = ProofLane::TeeAttestation;
        push_claim(&reads, 0, lane.mask());
        submissions.push_failure_msg("must not submit");
        let provider = |asserter| {
            ProviderBuilder::new()
                .disable_recommended_fillers()
                .connect_mocked_client(asserter)
        };
        let client =
            client(provider(reads.clone())).with_submission_provider(provider(submissions.clone()));
        assert!(matches!(
            client.submit_proof(Address::ZERO, lane, Bytes::new()).await,
            Err(DefenderError::LaneAlreadyProven { .. })
        ));
        assert_eq!(submissions.read_q().len(), 1);
        assert!(reads.read_q().is_empty());
        assert!(logs.0.lock().unwrap().is_empty());
    }

    fn client<P: Provider + Clone>(provider: P) -> AlloyDefenderClient<P> {
        AlloyDefenderClient {
            factory: IDisputeGameFactory::IDisputeGameFactoryInstance::new(
                Address::ZERO,
                provider.clone(),
            ),
            anchor: IAnchorStateRegistry::IAnchorStateRegistryInstance::new(
                Address::ZERO,
                provider.clone(),
            ),
            registered: RegisteredLineageConfig {
                domain_hash: B256::ZERO,
                block_interval: 1,
                anchor_registry: Address::ZERO,
            },
            confirmations: 1,
            receipt_timeout: Duration::from_secs(1),
            reward_recipient: Address::repeat_byte(1),
            semaphore: Arc::new(Semaphore::new(1)),
            submission_provider: provider.clone(),
            provider,
        }
    }

    #[tokio::test]
    async fn submission_failure_never_falls_back_to_read_provider() {
        let reads = Asserter::new();
        push_claim(&reads, 0, 0);
        reads.push_failure_msg("public RPC must not receive proof");
        let submissions = Asserter::new();
        submissions.push_failure_msg("private submission unavailable");
        let provider = |asserter| {
            ProviderBuilder::new()
                .disable_recommended_fillers()
                .connect_mocked_client(asserter)
        };
        let client =
            client(provider(reads.clone())).with_submission_provider(provider(submissions.clone()));

        let error = client
            .submit_proof(Address::ZERO, ProofLane::TeeAttestation, Bytes::new())
            .await
            .unwrap_err();

        assert!(error.to_string().contains("private submission unavailable"));
        assert_eq!(reads.read_q().len(), 1);
        assert!(submissions.read_q().is_empty());
    }

    #[tokio::test]
    async fn gas_estimation_failure_never_uses_read_provider() {
        let reads = Asserter::new();
        push_claim(&reads, 0, 0);
        reads.push_failure_msg("public RPC must not receive proof");
        let submissions = Asserter::new();
        for _ in 0..4 {
            submissions.push_failure_msg("private estimation unavailable");
        }
        let provider = |asserter| {
            ProviderBuilder::new()
                .disable_recommended_fillers()
                .with_gas_estimation()
                .connect_mocked_client(asserter)
        };
        let client =
            client(provider(reads.clone())).with_submission_provider(provider(submissions));

        let error = client
            .submit_proof(Address::ZERO, ProofLane::TeeAttestation, Bytes::new())
            .await
            .unwrap_err();

        assert!(error.to_string().contains("private estimation unavailable"));
        assert_eq!(reads.read_q().len(), 1);
    }

    #[tokio::test]
    async fn receipt_is_checked_on_read_provider_after_private_submission() {
        let logs = LogWriter::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let reads = Asserter::new();
        push_claim(&reads, 0, 0);
        reads.push_failure_msg("read RPC receipt check");
        let submissions = Asserter::new();
        submissions.push_success(&B256::repeat_byte(2));
        let provider = |asserter| {
            ProviderBuilder::new()
                .disable_recommended_fillers()
                .connect_mocked_client(asserter)
        };
        let client =
            client(provider(reads.clone())).with_submission_provider(provider(submissions.clone()));

        let error = client
            .submit_proof(Address::ZERO, ProofLane::TeeAttestation, Bytes::new())
            .await
            .unwrap_err();

        assert!(error.to_string().contains("read RPC receipt check"));
        let output = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(output.contains("proof_submission_failed"));
        assert!(output.contains(&B256::repeat_byte(2).to_string()));
        assert!(reads.read_q().is_empty());
        assert!(submissions.read_q().is_empty());
    }

    #[tokio::test]
    async fn missing_receipt_waits_until_timeout_without_resubmitting() {
        let reads = Asserter::new();
        push_claim(&reads, 0, 0);
        for _ in 0..10 {
            reads.push_success(&Option::<bool>::None);
        }
        let submissions = Asserter::new();
        submissions.push_success(&B256::repeat_byte(2));
        let provider = |asserter| {
            ProviderBuilder::new()
                .disable_recommended_fillers()
                .connect_mocked_client(asserter)
        };
        let client =
            client(provider(reads)).with_submission_provider(provider(submissions.clone()));
        let started = Instant::now();
        let error = client
            .submit_proof(Address::ZERO, ProofLane::TeeAttestation, Bytes::new())
            .await
            .unwrap_err();
        assert!(matches!(error, DefenderError::PendingTransaction(_)));
        assert!(started.elapsed() >= client.receipt_timeout);
        assert!(submissions.read_q().is_empty());
    }
}
