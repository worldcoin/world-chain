use crate::{
    config::DefenderConfig,
    defender::WorldChainDefender,
    error::DefenderError,
    traits::DefenderClient,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, B256, BlockNumber, Bytes, address};
use alloy_sol_types::SolValue;
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use world_chain_proof_core::boot::TransitionPublicValues;
use world_chain_proofs::{
    ClaimData, ConsensusError, ConsensusProvider, GameStatus, InvalidationReason, LineageAnchor,
    LineageError, LineageGame, LineageProvider, LineageTransition, MAX_ATTEMPT_SCAN,
    PROOF_THRESHOLD, ProofLane, ProposalCommitment, ProposalStatus, ResolutionStatus, proof_count,
};
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequestError, ProofRequestId, ProofRequester,
    ProofResponse, ProofStatus, RequestProofResponse, SucceededProofResponse,
    TooManyRetriesErrorData,
};

const ANCHOR: Address = address!("00000000000000000000000000000000000000a0");
const GAME_1: Address = address!("0000000000000000000000000000000000000001");
const GAME_2: Address = address!("0000000000000000000000000000000000000002");
const BLOCK_INTERVAL: u64 = 10;
const L2_BLOCK: u64 = 100;
const L1_ORIGIN_HASH: B256 = B256::repeat_byte(0x42);
const DOMAIN_HASH: B256 = B256::repeat_byte(0x43);

/// The mock's game lifecycle, from which both the claim status and the resolution outcome
/// reported to the defender are derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MockGameState {
    Proposed,
    Challenged,
    Invalidated,
}

#[derive(Debug, Clone)]
struct GameRecord {
    metadata: GameMetadata,
    state: MockGameState,
    invalidation_reason: InvalidationReason,
    proof_bitmap: u8,
}

#[derive(Debug)]
struct MockState {
    anchor: LineageAnchor,
    games_by_uuid: HashMap<B256, Address>,
    games: HashMap<Address, GameRecord>,
    submissions: Vec<(Address, u8)>,
}

#[derive(Debug, Clone)]
struct MockClient {
    state: Arc<Mutex<MockState>>,
}

impl MockClient {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState {
                anchor: LineageAnchor {
                    address: ANCHOR,
                    l2_block_number: L2_BLOCK - BLOCK_INTERVAL,
                },
                games_by_uuid: HashMap::new(),
                games: HashMap::new(),
                submissions: Vec::new(),
            })),
        }
    }

    fn insert_game(
        &self,
        address: Address,
        parent_ref: Address,
        root_claim: B256,
        l2_block_number: u64,
        attempt: u64,
        state: MockGameState,
    ) {
        let metadata = GameMetadata {
            address,
            domain_hash: DOMAIN_HASH,
            parent_ref,
            root_claim,
            l2_block_number,
            l1_origin_hash: L1_ORIGIN_HASH,
            l1_origin_number: 42,
            challenge_deadline: u64::MAX,
            proof_deadline: u64::MAX,
            proof_threshold: PROOF_THRESHOLD,
        };
        let mut guard = self.state.lock().expect("not poisoned");
        guard.games_by_uuid.insert(
            ProposalCommitment {
                parent_ref,
                root_claim,
                l2_block_number,
                attempt,
            }
            .game_uuid(DOMAIN_HASH),
            address,
        );
        guard.games.insert(
            address,
            GameRecord {
                metadata,
                state,
                invalidation_reason: InvalidationReason::None,
                proof_bitmap: 0,
            },
        );
    }

    fn set_anchor(&self, address: Address, l2_block_number: u64) {
        self.state.lock().expect("not poisoned").anchor = LineageAnchor {
            address,
            l2_block_number,
        };
    }

    fn set_state(&self, game: Address, state: MockGameState, reason: InvalidationReason) {
        let mut guard = self.state.lock().expect("not poisoned");
        let record = guard.games.get_mut(&game).expect("game exists");
        record.state = state;
        record.invalidation_reason = reason;
    }

    fn set_bitmap(&self, game: Address, proof_bitmap: u8) {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get_mut(&game)
            .expect("game exists")
            .proof_bitmap = proof_bitmap;
    }

    fn set_deadlines(&self, game: Address, challenge_deadline: u64, proof_deadline: u64) {
        let mut guard = self.state.lock().expect("not poisoned");
        let metadata = &mut guard.games.get_mut(&game).expect("game exists").metadata;
        metadata.challenge_deadline = challenge_deadline;
        metadata.proof_deadline = proof_deadline;
    }

    fn submissions(&self) -> Vec<(Address, u8)> {
        self.state.lock().expect("not poisoned").submissions.clone()
    }
}

#[async_trait]
impl LineageProvider for MockClient {
    fn lineage_block_interval(&self) -> u64 {
        BLOCK_INTERVAL
    }

    async fn lineage_anchor(&self) -> Result<LineageAnchor, LineageError> {
        Ok(self.state.lock().expect("not poisoned").anchor)
    }

    async fn game_for_transition(
        &self,
        transition: LineageTransition,
    ) -> Result<Option<LineageGame>, LineageError> {
        let guard = self.state.lock().expect("not poisoned");
        let mut latest = None;
        for attempt in 0..MAX_ATTEMPT_SCAN {
            let uuid = ProposalCommitment {
                parent_ref: transition.parent_ref,
                root_claim: transition.root_claim,
                l2_block_number: transition.l2_block_number,
                attempt,
            }
            .game_uuid(DOMAIN_HASH);
            let Some(address) = guard.games_by_uuid.get(&uuid).copied() else {
                break;
            };
            latest = Some(LineageGame { address, attempt });
        }
        Ok(latest)
    }

    async fn lineage_resolution_status(
        &self,
        game: Address,
    ) -> Result<ResolutionStatus, LineageError> {
        let guard = self.state.lock().expect("not poisoned");
        let record = guard
            .games
            .get(&game)
            .ok_or_else(|| LineageError::Contract(format!("unknown game {game}")))?;
        let threshold_met = proof_count(record.proof_bitmap) >= record.metadata.proof_threshold;
        let (resolvable, outcome) = match record.state {
            MockGameState::Invalidated => (false, GameStatus::ChallengerWins),
            MockGameState::Proposed | MockGameState::Challenged if threshold_met => {
                (true, GameStatus::DefenderWins)
            }
            MockGameState::Proposed | MockGameState::Challenged => (false, GameStatus::InProgress),
        };
        Ok(ResolutionStatus {
            resolvable,
            outcome,
            invalidation_reason: record.invalidation_reason,
        })
    }
}

#[async_trait]
impl DefenderClient for MockClient {
    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, DefenderError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|record| record.metadata)
            .ok_or_else(|| DefenderError::message(format!("unknown game {game}")))
    }

    async fn claim_data(&self, game: Address) -> Result<ClaimData, DefenderError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|record| ClaimData {
                status: match record.state {
                    MockGameState::Proposed => ProposalStatus::Unchallenged,
                    MockGameState::Challenged => ProposalStatus::Challenged,
                    MockGameState::Invalidated => ProposalStatus::Resolved,
                },
                proof_bitmap: record.proof_bitmap,
                invalidation_reason: record.invalidation_reason,
            })
            .ok_or_else(|| DefenderError::message(format!("unknown game {game}")))
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        _proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
        let mut guard = self.state.lock().expect("not poisoned");
        guard
            .games
            .get_mut(&game)
            .ok_or_else(|| DefenderError::message(format!("unknown game {game}")))?
            .proof_bitmap |= 1 << lane;
        guard.submissions.push((game, lane));
        Ok(DefenderSubmission {
            tx_hash: B256::repeat_byte(0xaa),
        })
    }
}

#[derive(Debug, Clone)]
struct MockOutputRoots {
    roots: HashMap<u64, B256>,
    finalized_l2_block: Arc<AtomicU64>,
}

#[async_trait]
impl ConsensusProvider for MockOutputRoots {
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ConsensusError> {
        self.roots
            .get(&l2_block_number)
            .copied()
            .ok_or_else(|| ConsensusError::Rpc(format!("missing root for {l2_block_number}")))
    }

    async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError> {
        Ok(self.finalized_l2_block.load(Ordering::SeqCst))
    }
}

fn output_roots(entries: &[(u64, B256)], finalized: u64) -> MockOutputRoots {
    MockOutputRoots {
        roots: entries.iter().copied().collect(),
        finalized_l2_block: Arc::new(AtomicU64::new(finalized)),
    }
}

#[derive(Debug, Clone, Default)]
struct MockProver {
    fail: bool,
    max_requests_per_proof: Option<u32>,
    requests: Arc<Mutex<Vec<ProofRequest>>>,
    request_counts: Arc<Mutex<HashMap<ProofRequestId, u32>>>,
    by_id: Arc<Mutex<HashMap<ProofRequestId, ProofRequest>>>,
}

impl MockProver {
    fn failing(max_requests_per_proof: u32) -> Self {
        Self {
            fail: true,
            max_requests_per_proof: Some(max_requests_per_proof),
            ..Self::default()
        }
    }

    fn requests(&self) -> Vec<ProofRequest> {
        self.requests.lock().expect("not poisoned").clone()
    }
}

#[async_trait]
impl ProofRequester for MockProver {
    async fn request_proof(
        &self,
        proof_request: ProofRequest,
    ) -> Result<RequestProofResponse, ProofRequestError> {
        let id = proof_request.id();
        let l1_head = proof_request.l1_head;
        self.requests
            .lock()
            .expect("not poisoned")
            .push(proof_request.clone());
        if let Some(max_requests_per_proof) = self.max_requests_per_proof {
            let mut request_counts = self.request_counts.lock().expect("not poisoned");
            let request_count = request_counts.entry(id).or_default();
            if *request_count >= max_requests_per_proof {
                return Err(ProofRequestError::TooManyRetries(TooManyRetriesErrorData {
                    proof_id: id,
                    max_retries: max_requests_per_proof.saturating_sub(1),
                }));
            }
            *request_count += 1;
        }
        self.by_id
            .lock()
            .expect("not poisoned")
            .insert(id, proof_request);
        Ok(RequestProofResponse {
            proof_id: id,
            l1_head,
        })
    }

    async fn proof_status(
        &self,
        _proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        Ok(if self.fail {
            ProofStatus::Failed
        } else {
            ProofStatus::Succeeded
        })
    }

    async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        let request = self
            .by_id
            .lock()
            .expect("not poisoned")
            .get(&proof_id)
            .cloned()
            .ok_or(ProofRequestError::ProofIdNotFound(proof_id))?;
        let proof = match request.backend {
            ProofBackend::Sp1 => ProofData::Sp1 {
                proof: Bytes::from_static(b"proof"),
                public_values: Bytes::from_static(b"public values"),
            },
            ProofBackend::Nitro => ProofData::Nitro {
                attestation: Bytes::from_static(b"attestation"),
                public_values: TransitionPublicValues {
                    l1Head: request.l1_head,
                    l2PreRoot: B256::ZERO,
                    l2PreBlockNumber: 0,
                    l2PostRoot: request.root_claim,
                    l2PostBlockNumber: request.l2_block_number,
                    rollupConfigHash: B256::ZERO,
                }
                .abi_encode()
                .into(),
                signature: Bytes::from_static(b"signature"),
            },
        };
        Ok(ProofResponse::Succeeded(SucceededProofResponse {
            id: proof_id,
            proof,
        }))
    }
}

fn config() -> DefenderConfig {
    DefenderConfig {
        poll_interval: Duration::from_secs(1),
        max_game_concurrency: 10,
    }
}

#[tokio::test]
async fn selected_proposed_game_gets_initial_tee_proof() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Proposed);
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client.clone(),
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert_eq!(prover.requests().len(), 1);
    assert_eq!(prover.requests()[0].backend, ProofBackend::Nitro);

    defender.tick().await.unwrap();
    assert_eq!(
        client.submissions(),
        vec![(GAME_1, ProofLane::TeeAttestation as u8)]
    );
}

#[tokio::test]
async fn selected_proposed_game_with_council_support_needs_no_tee_proof() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Proposed);
    client.set_bitmap(GAME_1, ProofLane::SecurityCouncil.mask());
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert!(prover.requests().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn selected_challenged_game_gets_threshold_lanes() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Challenged);
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client.clone(),
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    let requests = prover.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .any(|request| request.backend == ProofBackend::Sp1)
    );
    assert!(
        requests
            .iter()
            .any(|request| request.backend == ProofBackend::Nitro)
    );

    defender.tick().await.unwrap();
    assert_eq!(client.submissions().len(), 2);
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn selected_challenged_game_with_council_support_only_requests_tee() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Challenged);
    client.set_bitmap(GAME_1, ProofLane::SecurityCouncil.mask());
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client.clone(),
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert_eq!(prover.requests().len(), 1);
    assert_eq!(prover.requests()[0].backend, ProofBackend::Nitro);

    defender.tick().await.unwrap();
    assert_eq!(
        client.submissions(),
        vec![(GAME_1, ProofLane::TeeAttestation as u8)]
    );

    defender.tick().await.unwrap();
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn selected_descendant_is_defended_before_parent_resolves() {
    let root_1 = B256::repeat_byte(0x20);
    let root_2 = B256::repeat_byte(0x21);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root_1, L2_BLOCK, 0, MockGameState::Proposed);
    client.set_bitmap(GAME_1, ProofLane::TeeAttestation.mask());
    client.insert_game(
        GAME_2,
        GAME_1,
        root_2,
        L2_BLOCK + BLOCK_INTERVAL,
        0,
        MockGameState::Challenged,
    );
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(
            &[(L2_BLOCK, root_1), (L2_BLOCK + BLOCK_INTERVAL, root_2)],
            L2_BLOCK + BLOCK_INTERVAL,
        ),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert_eq!(prover.requests().len(), 2);
    assert!(
        prover
            .requests()
            .iter()
            .all(|request| request.game == GAME_2)
    );
}

#[tokio::test]
async fn game_for_a_different_root_is_not_selected() {
    let expected_root = B256::repeat_byte(0x20);
    let other_root = B256::repeat_byte(0x21);
    let client = MockClient::new();
    client.insert_game(
        GAME_1,
        ANCHOR,
        other_root,
        L2_BLOCK,
        0,
        MockGameState::Challenged,
    );
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(&[(L2_BLOCK, expected_root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert!(prover.requests().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn retry_replaces_active_old_attempt() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Challenged);
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client.clone(),
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover,
    );

    defender.tick().await.unwrap();
    assert_eq!(defender.active_defenses(), [GAME_1]);

    client.set_state(
        GAME_1,
        MockGameState::Invalidated,
        InvalidationReason::ProofTimeout,
    );
    client.insert_game(GAME_2, ANCHOR, root, L2_BLOCK, 1, MockGameState::Proposed);
    defender.tick().await.unwrap();

    assert_eq!(defender.active_defenses(), [GAME_2]);
    assert!(client.submissions().iter().all(|(game, _)| *game != GAME_1));
}

#[tokio::test]
async fn anchor_advance_drops_the_old_prefix() {
    let root_1 = B256::repeat_byte(0x20);
    let root_2 = B256::repeat_byte(0x21);
    let client = MockClient::new();
    client.insert_game(
        GAME_1,
        ANCHOR,
        root_1,
        L2_BLOCK,
        0,
        MockGameState::Challenged,
    );
    client.insert_game(
        GAME_2,
        GAME_1,
        root_2,
        L2_BLOCK + BLOCK_INTERVAL,
        0,
        MockGameState::Proposed,
    );
    let mut defender = WorldChainDefender::new(
        config(),
        client.clone(),
        output_roots(
            &[(L2_BLOCK, root_1), (L2_BLOCK + BLOCK_INTERVAL, root_2)],
            L2_BLOCK + BLOCK_INTERVAL,
        ),
        MockProver::default(),
    );

    defender.tick().await.unwrap();
    assert!(defender.active_defenses().contains(&GAME_1));

    client.set_anchor(GAME_1, L2_BLOCK);
    defender.tick().await.unwrap();
    assert!(!defender.active_defenses().contains(&GAME_1));
    assert!(defender.active_defenses().contains(&GAME_2));
}

#[tokio::test]
async fn invalidated_selected_attempt_is_left_for_the_proposer() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(
        GAME_1,
        ANCHOR,
        root,
        L2_BLOCK,
        0,
        MockGameState::Invalidated,
    );
    client.set_state(
        GAME_1,
        MockGameState::Invalidated,
        InvalidationReason::ProofTimeout,
    );
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert!(prover.requests().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn unfinalized_transition_is_not_selected_yet() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Challenged);
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK - 1),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert!(prover.requests().is_empty());
}

#[tokio::test]
async fn existing_lane_is_not_requested_again() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Challenged);
    client.set_bitmap(GAME_1, ProofLane::ValidityProof.mask());
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert_eq!(prover.requests().len(), 1);
    assert_eq!(prover.requests()[0].backend, ProofBackend::Nitro);
}

#[tokio::test]
async fn existing_tee_lane_only_requests_sp1_when_challenged() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Challenged);
    client.set_bitmap(GAME_1, ProofLane::TeeAttestation.mask());
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick().await.unwrap();
    assert_eq!(prover.requests().len(), 1);
    assert_eq!(prover.requests()[0].backend, ProofBackend::Sp1);
}

#[tokio::test]
async fn exhausted_challenged_proofs_are_not_restarted() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Challenged);
    let prover = MockProver::failing(2);
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    for _ in 0..8 {
        defender.tick().await.unwrap();
    }

    assert_eq!(prover.requests().len(), 6);
    assert!(defender.active_defenses().is_empty());
    assert_eq!(defender.abandoned_defenses(), [GAME_1]);
}

#[tokio::test]
async fn selected_game_deadline_stops_proof_work() {
    let root = B256::repeat_byte(0x20);
    let client = MockClient::new();
    client.insert_game(GAME_1, ANCHOR, root, L2_BLOCK, 0, MockGameState::Proposed);
    client.set_deadlines(GAME_1, 10, 20);
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(
        config(),
        client,
        output_roots(&[(L2_BLOCK, root)], L2_BLOCK),
        prover.clone(),
    );

    defender.tick_at(10).await.unwrap();
    assert!(prover.requests().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[test]
fn config_rejects_zero_concurrency() {
    let config = DefenderConfig {
        poll_interval: Duration::from_secs(1),
        max_game_concurrency: 0,
    };
    assert!(config.validate().is_err());
}
