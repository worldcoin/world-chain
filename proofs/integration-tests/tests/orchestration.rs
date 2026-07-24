use std::time::Duration;

use alloy_primitives::{B256, Bytes, U256};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use world_chain_challenger::{ChallengerConfig, WorldChainChallenger};
use world_chain_defender::{DefenderClient, DefenderConfig, WorldChainDefender};
use world_chain_proof_integration_tests::{
    BLOCK_INTERVAL, FAKE_PROPOSER, FakeConsensus, FakeExecution, FakeProofBackend,
    SharedProverService,
};
use world_chain_proof_worker::{
    ProofWorker, ProofWorkerConfig, RetryConfig, WorkerHeartbeatConfig,
};
use world_chain_proofs::{ProofLane, RootState, has_threshold};
use world_chain_proposer::{ProposerClient, ProposerConfig, WorldChainProposer};
use world_chain_prover_service::{ProofBackend, ProverServiceConfig};

fn proposer_config() -> ProposerConfig {
    ProposerConfig {
        block_interval: BLOCK_INTERVAL,
        proposer_bond: U256::from(1),
        poll_interval: Duration::from_secs(1),
        max_resolutions_per_tick: 1,
    }
}

fn challenger_config() -> ChallengerConfig {
    ChallengerConfig {
        challenger_bond: U256::from(1),
        poll_interval: Duration::from_secs(1),
        ..ChallengerConfig::default()
    }
}

fn defender_config() -> DefenderConfig {
    DefenderConfig {
        allowed_proposer: FAKE_PROPOSER,
        poll_interval: Duration::from_secs(1),
        max_proof_attempts: 2,
        ..DefenderConfig::default()
    }
}

fn assert_defense_lanes(lanes: Vec<ProofLane>) {
    assert_eq!(lanes.len(), 2);
    assert!(lanes.contains(&ProofLane::ValidityProof));
    assert!(lanes.contains(&ProofLane::TeeAttestation));
}

async fn settle_with_proposer(proposer: &WorldChainProposer<FakeExecution, FakeConsensus>) {
    let canonical_scan = proposer
        .anchor_and_canonical_line()
        .await
        .expect("canonical line reconstructed");
    let finalized_games = proposer
        .resolve_games(canonical_scan.canonical_line())
        .await
        .expect("canonical games resolved");
    proposer
        .advance_anchor(finalized_games)
        .await
        .expect("anchor advanced");
}

#[tokio::test]
async fn fake_resolution_matches_contract_transition_semantics() {
    let chain = FakeExecution::new();
    let canonical_root = B256::repeat_byte(0x20);
    let consensus = FakeConsensus::new(BLOCK_INTERVAL).with_root(BLOCK_INTERVAL, canonical_root);
    let proposer = WorldChainProposer::new(proposer_config(), chain.clone(), consensus);

    let canonical_scan = proposer
        .anchor_and_canonical_line()
        .await
        .expect("canonical line reconstructed");
    proposer
        .propose(&canonical_scan)
        .await
        .expect("proposal posted");
    let game = chain.latest_game().expect("game created").game;
    chain.challenge_game(game);

    chain
        .submit_proof(
            game,
            ProofLane::ValidityProof as u8,
            Bytes::from_static(&[1]),
        )
        .await
        .expect("validity proof submitted");
    chain
        .submit_proof(
            game,
            ProofLane::TeeAttestation as u8,
            Bytes::from_static(&[1]),
        )
        .await
        .expect("TEE proof submitted");

    let ready = ProposerClient::resolution_status(&chain, game)
        .await
        .expect("resolution status available");
    assert!(ready.resolvable);
    assert_eq!(ready.root_state, RootState::Finalized);
    assert_eq!(chain.game_state(game), RootState::Challenged);

    settle_with_proposer(&proposer).await;

    let finalized = ProposerClient::resolution_status(&chain, game)
        .await
        .expect("resolution status available");
    assert!(!finalized.resolvable);
    assert_eq!(finalized.root_state, RootState::Finalized);
    assert!(chain.resolve_game(game).await.is_err());
}

struct ProofStack {
    service: SharedProverService,
    _postgres: ContainerAsync<postgres::Postgres>,
    tokens: Vec<CancellationToken>,
    handles: Vec<JoinHandle<()>>,
}

async fn start_proof_stack() -> Option<ProofStack> {
    start_proof_stack_with(
        FakeProofBackend::new(ProofBackend::Sp1),
        FakeProofBackend::new(ProofBackend::Nitro),
    )
    .await
}

async fn start_proof_stack_with(
    sp1_backend: FakeProofBackend,
    nitro_backend: FakeProofBackend,
) -> Option<ProofStack> {
    let postgres = match postgres::Postgres::default().start().await {
        Ok(postgres) => postgres,
        Err(error) => {
            eprintln!("skipping postgres-backed orchestration test: {error}");
            return None;
        }
    };
    let database_url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        postgres.get_host().await.expect("postgres host"),
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres port")
    );
    let service = SharedProverService::connect(
        &database_url,
        ProverServiceConfig {
            // Failed attempts re-queue only after their lock expires (there is no explicit
            // fail API), so keep the timeout short enough that the transient-failure test
            // observes the retry within its polling budget.
            lock_timeout: Duration::from_millis(500),
            max_attempts: 2,
            max_retries: 2,
            backend_poll_interval: Duration::from_millis(5),
            status_poller_interval: Duration::from_millis(5),
        },
    )
    .await
    .expect("valid prover-service config");

    let sp1_worker = ProofWorker::new(
        service.clone(),
        sp1_backend,
        ProofWorkerConfig {
            worker_id: "orchestration-sp1-worker".to_string(),
            poll_interval: Duration::from_millis(5),
            max_concurrent_jobs: 1,
            retry_config: RetryConfig::default(),
            heartbeat_config: WorkerHeartbeatConfig::default(),
        },
    );
    let sp1_cancel = sp1_worker.cancellation_token();
    let sp1_handle = tokio::spawn(sp1_worker);

    let nitro_worker = ProofWorker::new(
        service.clone(),
        nitro_backend,
        ProofWorkerConfig {
            worker_id: "orchestration-nitro-worker".to_string(),
            poll_interval: Duration::from_millis(5),
            max_concurrent_jobs: 1,
            retry_config: RetryConfig::default(),
            heartbeat_config: WorkerHeartbeatConfig::default(),
        },
    );
    let nitro_cancel = nitro_worker.cancellation_token();
    let nitro_handle = tokio::spawn(nitro_worker);

    Some(ProofStack {
        service,
        _postgres: postgres,
        tokens: vec![sp1_cancel, nitro_cancel],
        handles: vec![sp1_handle, nitro_handle],
    })
}

async fn stop_proof_stack(stack: ProofStack) {
    for token in stack.tokens {
        token.cancel();
    }
    for handle in stack.handles {
        handle.await.expect("worker shuts down");
    }
}

#[tokio::test]
async fn invalid_root_is_challenged_by_real_challenger() {
    let chain = FakeExecution::new();
    let canonical_root = B256::repeat_byte(0x20);
    let bad_root = B256::repeat_byte(0x99);
    let bad_proposer_consensus =
        FakeConsensus::new(BLOCK_INTERVAL).with_root(BLOCK_INTERVAL, bad_root);
    let honest_consensus =
        FakeConsensus::new(BLOCK_INTERVAL).with_root(BLOCK_INTERVAL, canonical_root);

    let proposer =
        WorldChainProposer::new(proposer_config(), chain.clone(), bad_proposer_consensus);
    let canonical_scan = proposer
        .anchor_and_canonical_line()
        .await
        .expect("canonical line reconstructed");
    proposer
        .propose(&canonical_scan)
        .await
        .expect("bad proposal posted");
    let game = chain.latest_game().expect("game created").game;

    let mut challenger =
        WorldChainChallenger::new(challenger_config(), chain.clone(), honest_consensus);
    challenger
        .scan_once()
        .await
        .expect("challenger scan succeeds");

    assert_eq!(chain.game_state(game), RootState::Challenged);
    assert_eq!(chain.challenge_count(game), 1);
}

#[tokio::test]
async fn valid_challenged_root_is_defended_through_workers() {
    let chain = FakeExecution::new();
    let canonical_root = B256::repeat_byte(0x20);
    let consensus = FakeConsensus::new(BLOCK_INTERVAL).with_root(BLOCK_INTERVAL, canonical_root);

    let proposer = WorldChainProposer::new(proposer_config(), chain.clone(), consensus.clone());
    let canonical_scan = proposer
        .anchor_and_canonical_line()
        .await
        .expect("canonical line reconstructed");
    proposer
        .propose(&canonical_scan)
        .await
        .expect("proposal posted");
    let game = chain.latest_game().expect("game created").game;
    chain.challenge_game(game);

    let Some(stack) = start_proof_stack().await else {
        return;
    };
    let mut defender = WorldChainDefender::new(
        defender_config(),
        chain.clone(),
        consensus,
        stack.service.clone(),
    );

    for _ in 0..200 {
        defender.tick().await.expect("defender scan succeeds");
        if has_threshold(chain.proof_bitmap(game)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(chain.game_state(game), RootState::Challenged);
    assert_defense_lanes(chain.submitted_lanes(game));

    settle_with_proposer(&proposer).await;

    assert_eq!(chain.game_state(game), RootState::Finalized);

    stop_proof_stack(stack).await;
}

#[tokio::test]
async fn valid_challenged_root_survives_transient_proof_failure() {
    let chain = FakeExecution::new();
    let canonical_root = B256::repeat_byte(0x20);
    let consensus = FakeConsensus::new(BLOCK_INTERVAL).with_root(BLOCK_INTERVAL, canonical_root);

    let proposer = WorldChainProposer::new(proposer_config(), chain.clone(), consensus.clone());
    let canonical_scan = proposer
        .anchor_and_canonical_line()
        .await
        .expect("canonical line reconstructed");
    proposer
        .propose(&canonical_scan)
        .await
        .expect("proposal posted");
    let game = chain.latest_game().expect("game created").game;
    chain.challenge_game(game);

    let Some(stack) = start_proof_stack_with(
        FakeProofBackend::flaky(ProofBackend::Sp1, 1),
        FakeProofBackend::new(ProofBackend::Nitro),
    )
    .await
    else {
        return;
    };
    let mut defender = WorldChainDefender::new(
        defender_config(),
        chain.clone(),
        consensus,
        stack.service.clone(),
    );

    for _ in 0..200 {
        defender.tick().await.expect("defender scan succeeds");
        if has_threshold(chain.proof_bitmap(game)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(chain.game_state(game), RootState::Challenged);
    assert_defense_lanes(chain.submitted_lanes(game));

    settle_with_proposer(&proposer).await;

    assert_eq!(chain.game_state(game), RootState::Finalized);

    stop_proof_stack(stack).await;
}

#[tokio::test]
async fn defender_ignores_challenged_invalid_root() {
    let chain = FakeExecution::new();
    let canonical_root = B256::repeat_byte(0x20);
    let bad_root = B256::repeat_byte(0x99);
    let bad_proposer_consensus =
        FakeConsensus::new(BLOCK_INTERVAL).with_root(BLOCK_INTERVAL, bad_root);
    let honest_consensus =
        FakeConsensus::new(BLOCK_INTERVAL).with_root(BLOCK_INTERVAL, canonical_root);

    let proposer =
        WorldChainProposer::new(proposer_config(), chain.clone(), bad_proposer_consensus);
    let canonical_scan = proposer
        .anchor_and_canonical_line()
        .await
        .expect("canonical line reconstructed");
    proposer
        .propose(&canonical_scan)
        .await
        .expect("bad proposal posted");
    let game = chain.latest_game().expect("game created").game;
    chain.challenge_game(game);

    let Some(stack) = start_proof_stack().await else {
        return;
    };
    let mut defender = WorldChainDefender::new(
        defender_config(),
        chain.clone(),
        honest_consensus,
        stack.service.clone(),
    );

    for _ in 0..10 {
        defender.tick().await.expect("defender scan succeeds");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(chain.game_state(game), RootState::Challenged);
    assert_eq!(chain.proof_bitmap(game), 0);
    assert!(chain.submitted_lanes(game).is_empty());

    stop_proof_stack(stack).await;
}
