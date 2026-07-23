use std::time::Duration;

use alloy_primitives::{B256, Bytes, U256};
use testcontainers::{ContainerAsync, runners::AsyncRunner};
use testcontainers_modules::postgres;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use world_chain_challenger::{ChallengerConfig, WorldChainChallenger};
use world_chain_defender::{DefenderClient, DefenderConfig, WorldChainDefender};
use world_chain_proof_integration_tests::{
    BLOCK_INTERVAL, FakeConsensus, FakeExecution, FakeProofBackend, SharedProverService,
};
use world_chain_proof_worker::{
    ProofWorker, ProofWorkerConfig, RetryConfig, WorkerHeartbeatConfig,
};
use world_chain_proofs::{ProofLane, RootState, has_threshold};
use world_chain_proposer::{
    NextProposalAction, Proposal, ProposerClient, ProposerConfig, WorldChainProposer,
};
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

    let ready = chain
        .resolution_status(game)
        .await
        .expect("resolution status available");
    assert!(ready.resolvable);
    assert_eq!(ready.root_state, RootState::Finalized);
    assert_eq!(chain.game_state(game), RootState::Challenged);

    settle_with_proposer(&proposer).await;

    let finalized = chain
        .resolution_status(game)
        .await
        .expect("resolution status available");
    assert!(!finalized.resolvable);
    assert_eq!(finalized.root_state, RootState::Finalized);
    assert!(chain.resolve_game(game).await.is_err());
}

#[tokio::test]
async fn proposer_defers_close_inside_finality_airgap() {
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

    // The game resolves, but is still inside the registry finality airgap: the proposer must
    // resolve it yet defer `closeGame`, leaving the anchor at genesis.
    chain.set_game_finalized(game, false);
    settle_with_proposer(&proposer).await;
    assert_eq!(chain.game_state(game), RootState::Finalized);
    assert_eq!(chain.anchor_l2_block(), 0);
    assert!(chain.anchor_game().is_none());

    // Once the airgap elapses, the next settle advances the anchor onto the finalized game.
    chain.set_game_finalized(game, true);
    settle_with_proposer(&proposer).await;
    assert_eq!(chain.anchor_l2_block(), BLOCK_INTERVAL);
    assert_eq!(chain.anchor_game(), Some(game));
}

#[tokio::test]
async fn proposer_continues_chain_from_index_addressed_child() {
    let chain = FakeExecution::new();
    let root_10 = B256::repeat_byte(0x10);
    let root_20 = B256::repeat_byte(0x20);
    let root_30 = B256::repeat_byte(0x30);
    let consensus = FakeConsensus::new(3 * BLOCK_INTERVAL)
        .with_root(BLOCK_INTERVAL, root_10)
        .with_root(2 * BLOCK_INTERVAL, root_20)
        .with_root(3 * BLOCK_INTERVAL, root_30);
    let proposer = WorldChainProposer::new(proposer_config(), chain.clone(), consensus);

    // The proposer creates the block-10 game against the anchor sentinel.
    let canonical_scan = proposer
        .anchor_and_canonical_line()
        .await
        .expect("canonical line reconstructed");
    proposer
        .propose(&canonical_scan)
        .await
        .expect("anchor child proposed");
    let anchor_game = chain.latest_game().expect("anchor game created").game;
    let anchor_index = chain
        .game_index(anchor_game)
        .await
        .expect("anchor game index");

    // Before the anchor advances, a block-20 child is created referencing the block-10 game by
    // its factory index (the only way to address a not-yet-anchor parent).
    let child = chain
        .submit_proposal(
            &Proposal {
                parent_index: anchor_index,
                parent_ref: anchor_game,
                root_claim: root_20,
                l2_block_number: 2 * BLOCK_INTERVAL,
                attempt: U256::ZERO,
            },
            U256::from(1),
        )
        .await
        .expect("index-addressed child created")
        .game_address;

    // Finalize the block-10 game and advance the anchor onto it.
    chain.challenge_game(anchor_game);
    chain
        .submit_proof(
            anchor_game,
            ProofLane::ValidityProof as u8,
            Bytes::from_static(&[1]),
        )
        .await
        .expect("validity proof submitted");
    chain
        .submit_proof(
            anchor_game,
            ProofLane::TeeAttestation as u8,
            Bytes::from_static(&[1]),
        )
        .await
        .expect("TEE proof submitted");
    settle_with_proposer(&proposer).await;
    assert_eq!(chain.anchor_game(), Some(anchor_game));
    assert_eq!(chain.anchor_l2_block(), BLOCK_INTERVAL);

    // The child now references the anchor game by index; the proposer must discover it and
    // continue the canonical line from it rather than re-proposing block 20.
    let canonical_scan = proposer
        .anchor_and_canonical_line()
        .await
        .expect("canonical line reconstructed");
    let games = canonical_scan.canonical_line().games();
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].address, child);
    match canonical_scan.next_action() {
        NextProposalAction::Propose(proposal) => {
            assert_eq!(proposal.l2_block_number, 3 * BLOCK_INTERVAL);
            assert_eq!(proposal.parent_ref, child);
        }
        other => panic!("expected new proposal on the index-addressed child, got {other:?}"),
    }
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
        defender.scan_once().await.expect("defender scan succeeds");
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
        defender.scan_once().await.expect("defender scan succeeds");
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
        defender.scan_once().await.expect("defender scan succeeds");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(chain.game_state(game), RootState::Challenged);
    assert_eq!(chain.proof_bitmap(game), 0);
    assert!(chain.submitted_lanes(game).is_empty());

    stop_proof_stack(stack).await;
}
