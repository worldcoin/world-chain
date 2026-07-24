use crate::{
    config::DefenderConfig,
    defender::WorldChainDefender,
    error::DefenderError,
    traits::DefenderClient,
    types::{DefenderSubmission, GameMetadata},
};
use alloy_primitives::{Address, B256, BlockNumber, Bytes, address};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use world_chain_proofs::{
    ConsensusError, ConsensusProvider, InvalidationReason, PROOF_THRESHOLD, ProofLane,
    ResolutionStatus, RootState, proof_count,
};
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequestError, ProofRequestId, ProofRequester,
    ProofResponse, ProofStatus, SucceededProofResponse,
};

const GAME_1: Address = address!("0000000000000000000000000000000000000001");
const GAME_2: Address = address!("0000000000000000000000000000000000000002");
const ALLOWED_PROPOSER: Address = address!("00000000000000000000000000000000000000a1");
const OTHER_PROPOSER: Address = address!("00000000000000000000000000000000000000b2");
const L2_BLOCK: u64 = 100;
const L1_ORIGIN_HASH: B256 = B256::repeat_byte(0x42);

/// State discriminant matching [`RootState::Proposed`].
const STATE_PROPOSED: u8 = 1;
/// State discriminant matching [`RootState::Challenged`].
const STATE_CHALLENGED: u8 = 2;
/// State discriminant matching [`RootState::Finalized`].
const STATE_FINALIZED: u8 = 3;
/// State discriminant matching [`RootState::Invalidated`].
const STATE_INVALIDATED: u8 = 4;

#[derive(Debug, Clone)]
struct MockClient {
    games: Vec<GameMetadata>,
    proposers: HashMap<Address, Address>,
    states: Arc<Mutex<HashMap<Address, u8>>>,
    bitmaps: Arc<Mutex<HashMap<Address, u8>>>,
    parent_unresolved: Arc<Mutex<HashSet<Address>>>,
    proof_bitmap_reads: Arc<AtomicUsize>,
    fail_game_count: Arc<AtomicBool>,
    submissions: Arc<Mutex<Vec<(Address, u8)>>>,
}

impl MockClient {
    fn new(games: Vec<(Address, B256, u64)>, states: HashMap<Address, u8>) -> Self {
        let games: Vec<_> = games
            .into_iter()
            .map(|(address, root_claim, l2_block_number)| GameMetadata {
                address,
                root_claim,
                l2_block_number,
                l1_origin_hash: L1_ORIGIN_HASH,
                challenge_deadline: u64::MAX,
                proof_deadline: u64::MAX,
                proof_threshold: PROOF_THRESHOLD,
            })
            .collect();
        let proposers = games
            .iter()
            .map(|game| (game.address, ALLOWED_PROPOSER))
            .collect();
        Self {
            games,
            proposers,
            states: Arc::new(Mutex::new(states)),
            bitmaps: Arc::default(),
            parent_unresolved: Arc::default(),
            proof_bitmap_reads: Arc::default(),
            fail_game_count: Arc::default(),
            submissions: Arc::default(),
        }
    }

    fn set_proposer(&mut self, game: Address, proposer: Address) {
        self.proposers.insert(game, proposer);
    }

    fn set_deadlines(&mut self, game: Address, challenge_deadline: u64, proof_deadline: u64) {
        let metadata = self
            .games
            .iter_mut()
            .find(|metadata| metadata.address == game)
            .expect("game exists");
        metadata.challenge_deadline = challenge_deadline;
        metadata.proof_deadline = proof_deadline;
    }

    fn set_proof_threshold(&mut self, game: Address, proof_threshold: u8) {
        self.games
            .iter_mut()
            .find(|metadata| metadata.address == game)
            .expect("game exists")
            .proof_threshold = proof_threshold;
    }

    fn set_state(&self, game: Address, state: u8) {
        self.states
            .lock()
            .expect("not poisoned")
            .insert(game, state);
    }

    fn state(&self, game: Address) -> Result<RootState, DefenderError> {
        let raw = self
            .states
            .lock()
            .expect("not poisoned")
            .get(&game)
            .copied()
            .unwrap_or(STATE_PROPOSED);
        RootState::try_from(raw).map_err(Into::into)
    }

    fn set_bitmap(&self, game: Address, bitmap: u8) {
        self.bitmaps
            .lock()
            .expect("not poisoned")
            .insert(game, bitmap);
    }

    fn bitmap(&self, game: Address) -> u8 {
        self.bitmaps
            .lock()
            .expect("not poisoned")
            .get(&game)
            .copied()
            .unwrap_or(0)
    }

    fn set_parent_unresolved(&self, game: Address) {
        self.parent_unresolved
            .lock()
            .expect("not poisoned")
            .insert(game);
    }

    fn reset_proof_bitmap_reads(&self) {
        self.proof_bitmap_reads.store(0, Ordering::SeqCst);
    }

    fn proof_bitmap_reads(&self) -> usize {
        self.proof_bitmap_reads.load(Ordering::SeqCst)
    }

    fn set_game_count_failure(&self, fail: bool) {
        self.fail_game_count.store(fail, Ordering::SeqCst);
    }

    fn submissions(&self) -> Vec<(Address, u8)> {
        self.submissions.lock().expect("not poisoned").clone()
    }
}

#[async_trait]
impl DefenderClient for MockClient {
    async fn game_count(&self) -> Result<u64, DefenderError> {
        if self.fail_game_count.load(Ordering::SeqCst) {
            return Err(DefenderError::Contract(
                "configured game_count failure".into(),
            ));
        }
        Ok(self.games.len() as u64)
    }

    async fn game_address_at(&self, index: u64) -> Result<Address, DefenderError> {
        self.games
            .get(index as usize)
            .map(|game| game.address)
            .ok_or_else(|| DefenderError::Contract(format!("unknown game index {index}")))
    }

    async fn game_proposer(&self, game: Address) -> Result<Address, DefenderError> {
        self.proposers
            .get(&game)
            .copied()
            .ok_or_else(|| DefenderError::Contract(format!("unknown game {game}")))
    }

    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, DefenderError> {
        self.games
            .iter()
            .find(|metadata| metadata.address == game)
            .copied()
            .ok_or_else(|| DefenderError::Contract(format!("unknown game {game}")))
    }

    async fn proof_deadline(&self, game: Address) -> Result<u64, DefenderError> {
        self.games
            .iter()
            .find(|metadata| metadata.address == game)
            .map(|metadata| metadata.proof_deadline)
            .ok_or_else(|| DefenderError::Contract(format!("unknown game {game}")))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, DefenderError> {
        let state = self.state(game)?;
        if self
            .parent_unresolved
            .lock()
            .expect("not poisoned")
            .contains(&game)
        {
            return Ok(ResolutionStatus {
                resolvable: false,
                root_state: state,
                invalidation_reason: InvalidationReason::None,
            });
        }
        if state == RootState::Challenged {
            let proof_threshold = self
                .games
                .iter()
                .find(|metadata| metadata.address == game)
                .expect("game exists")
                .proof_threshold;
            if proof_count(self.bitmap(game)) >= proof_threshold {
                return Ok(ResolutionStatus {
                    resolvable: true,
                    root_state: RootState::Finalized,
                    invalidation_reason: InvalidationReason::None,
                });
            }
            if self.proof_deadline(game).await? == 0 {
                return Ok(ResolutionStatus {
                    resolvable: true,
                    root_state: RootState::Invalidated,
                    invalidation_reason: InvalidationReason::ProofTimeout,
                });
            }
        }
        Ok(ResolutionStatus {
            resolvable: false,
            root_state: state,
            invalidation_reason: InvalidationReason::None,
        })
    }

    async fn proof_bitmap(&self, game: Address) -> Result<u8, DefenderError> {
        self.proof_bitmap_reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.bitmap(game))
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        _proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
        {
            let mut bitmaps = self.bitmaps.lock().expect("not poisoned");
            let bitmap = bitmaps.entry(game).or_default();
            *bitmap |= 1 << lane;
        }
        self.submissions
            .lock()
            .expect("not poisoned")
            .push((game, lane));
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

fn mock_output_roots(
    roots: HashMap<u64, B256>,
    finalized_l2_block: BlockNumber,
) -> (MockOutputRoots, Arc<AtomicU64>) {
    let finalized_l2_block = Arc::new(AtomicU64::new(finalized_l2_block));
    (
        MockOutputRoots {
            roots,
            finalized_l2_block: Arc::clone(&finalized_l2_block),
        },
        finalized_l2_block,
    )
}

#[derive(Debug, Clone, Default)]
struct MockProver {
    /// When true, every requested proof reports [`ProofStatus::Failed`].
    fail: bool,
    requests: Arc<Mutex<Vec<ProofRequest>>>,
    by_id: Arc<Mutex<HashMap<ProofRequestId, ProofRequest>>>,
}

impl MockProver {
    fn failing() -> Self {
        Self {
            fail: true,
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
    ) -> Result<ProofRequestId, ProofRequestError> {
        let id = proof_request.id();
        self.requests
            .lock()
            .expect("not poisoned")
            .push(proof_request.clone());
        self.by_id
            .lock()
            .expect("not poisoned")
            .insert(id, proof_request);
        Ok(id)
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
                public_values: Bytes::from_static(b"public values"),
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
        allowed_proposer: ALLOWED_PROPOSER,
        poll_interval: Duration::from_secs(1),
        ..DefenderConfig::default()
    }
}

#[tokio::test]
async fn scan_once_requires_an_allowed_proposer() {
    let client = MockClient::new(Vec::new(), HashMap::new());
    let (output_roots, _finalized_l2_block) = mock_output_roots(HashMap::new(), 0);
    let mut defender = WorldChainDefender::new(
        DefenderConfig::default(),
        client,
        output_roots,
        MockProver::default(),
    );

    let error = defender.scan_once().await.unwrap_err();
    assert!(matches!(error, DefenderError::InvalidConfig(_)));
}

#[tokio::test]
async fn scan_once_binary_searches_for_first_unexpired_proof_deadline() {
    let canonical_root = B256::repeat_byte(0x20);
    let mut client = MockClient::new(
        vec![
            (GAME_1, canonical_root, L2_BLOCK),
            (GAME_2, canonical_root, L2_BLOCK),
        ],
        HashMap::new(),
    );
    client.set_deadlines(GAME_1, 0, 0);
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut defender =
        WorldChainDefender::new(config(), client, output_roots, MockProver::default());

    defender.scan_once().await.unwrap();

    assert_eq!(defender.next_game_index(), Some(2));
    assert_eq!(defender.watched_games(), [GAME_2]);
}

#[tokio::test]
async fn scan_once_respects_factory_scan_budget() {
    let canonical_root = B256::repeat_byte(0x20);
    let client = MockClient::new(
        vec![
            (GAME_1, canonical_root, L2_BLOCK),
            (GAME_2, canonical_root, L2_BLOCK),
        ],
        HashMap::new(),
    );
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut limited_config = config();
    limited_config.max_games_per_tick = 1;
    let mut defender =
        WorldChainDefender::new(limited_config, client, output_roots, MockProver::default());

    defender.scan_once().await.unwrap();
    assert_eq!(defender.next_game_index(), Some(1));
    assert_eq!(defender.watched_games(), [GAME_1]);

    defender.scan_once().await.unwrap();
    assert_eq!(defender.next_game_index(), Some(2));
    let watched = defender.watched_games();
    assert_eq!(watched.len(), 2);
    assert!(watched.contains(&GAME_1));
    assert!(watched.contains(&GAME_2));
}

#[tokio::test]
async fn scan_once_ignores_games_from_other_proposers() {
    let canonical_root = B256::repeat_byte(0x20);
    let mut client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    client.set_proposer(GAME_1, OTHER_PROPOSER);
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(config(), client, output_roots, prover.clone());

    defender.scan_once().await.unwrap();

    assert_eq!(defender.next_game_index(), Some(1));
    assert!(prover.requests().is_empty());
    assert!(defender.watched_games().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn scan_once_discards_invalidated_games() {
    let canonical_root = B256::repeat_byte(0x20);
    let client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_INVALIDATED)]),
    );
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(config(), client, output_roots, prover.clone());

    defender.scan_once().await.unwrap();

    assert!(prover.requests().is_empty());
    assert!(defender.watched_games().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn proposed_game_is_removed_after_challenge_deadline() {
    let canonical_root = B256::repeat_byte(0x20);
    let mut client = MockClient::new(vec![(GAME_1, canonical_root, L2_BLOCK)], HashMap::new());
    client.set_deadlines(GAME_1, 10, 20);
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut defender =
        WorldChainDefender::new(config(), client, output_roots, MockProver::default());

    defender.scan_once_with_timestamp(5).await.unwrap();
    assert_eq!(defender.watched_games(), [GAME_1]);

    defender.scan_once_with_timestamp(10).await.unwrap();
    assert!(defender.watched_games().is_empty());
}

#[tokio::test]
async fn active_defense_is_removed_after_proof_deadline() {
    let canonical_root = B256::repeat_byte(0x20);
    let mut client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    client.set_deadlines(GAME_1, 10, 20);
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender = WorldChainDefender::new(config(), client, output_roots, prover.clone());

    defender.scan_once_with_timestamp(5).await.unwrap();
    assert_eq!(defender.active_defenses(), [GAME_1]);
    assert!(prover.requests().is_empty());

    defender.scan_once_with_timestamp(6).await.unwrap();
    assert_eq!(prover.requests().len(), 2);

    defender.scan_once_with_timestamp(20).await.unwrap();
    assert!(defender.active_defenses().is_empty());
    assert!(defender.watched_games().is_empty());

    // Removal makes the critical deadline condition one-shot.
    defender.scan_once_with_timestamp(21).await.unwrap();
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn active_defense_advances_before_discovery_failure() {
    let canonical_root = B256::repeat_byte(0x20);
    let client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    // Discovery and validation promote the game, but active work starts on
    // the next tick because existing defenses run first.
    defender.scan_once().await.unwrap();
    assert_eq!(defender.active_defenses(), [GAME_1]);
    assert!(prover.requests().is_empty());

    client.set_game_count_failure(true);
    let error = defender.scan_once().await.unwrap_err();

    assert!(matches!(error, DefenderError::Contract(_)));
    assert_eq!(prover.requests().len(), 2);
    assert_eq!(defender.active_defenses(), [GAME_1]);
}

#[tokio::test]
async fn scan_accepts_sufficient_proof_support_hidden_by_an_unresolved_parent() {
    let canonical_root = B256::repeat_byte(0x20);
    let mut client = MockClient::new(
        vec![
            (GAME_1, canonical_root, L2_BLOCK),
            (GAME_2, canonical_root, L2_BLOCK),
        ],
        HashMap::from([(GAME_2, STATE_CHALLENGED)]),
    );
    client.set_deadlines(GAME_1, 20, 20);
    client.set_deadlines(GAME_2, 20, 20);
    client.set_proof_threshold(GAME_2, 1);
    client.set_parent_unresolved(GAME_2);
    client.set_bitmap(GAME_2, ProofLane::ValidityProof.mask());
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut limited_config = config();
    limited_config.max_games_per_tick = 1;
    let mut defender = WorldChainDefender::new(
        limited_config,
        client.clone(),
        output_roots,
        MockProver::default(),
    );

    defender.scan_once_with_timestamp(5).await.unwrap();
    assert_eq!(defender.next_game_index(), Some(1));

    client.reset_proof_bitmap_reads();
    defender.scan_once_with_timestamp(20).await.unwrap();

    assert_eq!(client.proof_bitmap_reads(), 1);
    assert_eq!(defender.next_game_index(), Some(2));
    assert!(defender.watched_games().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn watched_game_accepts_sufficient_proof_support_hidden_by_an_unresolved_parent() {
    let canonical_root = B256::repeat_byte(0x20);
    let mut client = MockClient::new(vec![(GAME_1, canonical_root, L2_BLOCK)], HashMap::new());
    client.set_deadlines(GAME_1, 10, 20);
    client.set_proof_threshold(GAME_1, 1);
    client.set_parent_unresolved(GAME_1);
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    defender.scan_once_with_timestamp(5).await.unwrap();
    assert_eq!(defender.watched_games(), [GAME_1]);

    client.set_state(GAME_1, STATE_CHALLENGED);
    client.set_bitmap(GAME_1, ProofLane::ValidityProof.mask());
    client.reset_proof_bitmap_reads();
    defender.scan_once_with_timestamp(20).await.unwrap();

    assert_eq!(client.proof_bitmap_reads(), 1);
    assert!(prover.requests().is_empty());
    assert!(defender.watched_games().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn active_defense_accepts_sufficient_proof_support_hidden_by_an_unresolved_parent() {
    let canonical_root = B256::repeat_byte(0x20);
    let mut client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    client.set_deadlines(GAME_1, 10, 20);
    client.set_parent_unresolved(GAME_1);
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    defender.scan_once_with_timestamp(5).await.unwrap();
    assert_eq!(defender.active_defenses(), [GAME_1]);
    assert!(prover.requests().is_empty());

    defender.scan_once_with_timestamp(6).await.unwrap();
    assert_eq!(prover.requests().len(), 2);

    client.set_bitmap(
        GAME_1,
        ProofLane::ValidityProof.mask() | ProofLane::TeeAttestation.mask(),
    );
    client.reset_proof_bitmap_reads();
    defender.scan_once_with_timestamp(20).await.unwrap();

    assert_eq!(client.proof_bitmap_reads(), 1);
    assert!(client.submissions().is_empty());
    assert!(defender.watched_games().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn scan_once_defends_challenged_valid_root() {
    let canonical_root = B256::repeat_byte(0x20);

    let client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    // First tick: discovery and validation promote the challenged game.
    defender.scan_once().await.unwrap();
    assert_eq!(defender.active_defenses(), [GAME_1]);
    assert!(prover.requests().is_empty());

    // Second tick: both lane proofs are requested.
    defender.scan_once().await.unwrap();
    let requests = prover.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].backend, ProofBackend::Sp1);
    assert_eq!(requests[1].backend, ProofBackend::Nitro);
    for request in &requests {
        assert_eq!(request.game, GAME_1);
        assert_eq!(request.root_claim, canonical_root);
        assert_eq!(request.l2_block_number, L2_BLOCK);
        assert_eq!(request.l1_head, L1_ORIGIN_HASH);
    }
    assert!(client.submissions().is_empty());

    // Third tick: both proofs are completed, fetched and submitted on-chain.
    defender.scan_once().await.unwrap();

    assert_eq!(
        client.submissions(),
        vec![
            (GAME_1, ProofLane::ValidityProof as u8),
            (GAME_1, ProofLane::TeeAttestation as u8),
        ]
    );
    assert!(defender.active_defenses().is_empty());
    assert!(defender.watched_games().is_empty());
}

#[tokio::test]
async fn scan_once_waits_for_proposed_game_to_be_challenged() {
    let canonical_root = B256::repeat_byte(0x20);

    let client = MockClient::new(vec![(GAME_1, canonical_root, L2_BLOCK)], HashMap::new());
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    // the game is only proposed: keep watching, no proof requests
    defender.scan_once().await.unwrap();
    assert!(prover.requests().is_empty());
    assert_eq!(defender.watched_games(), [GAME_1]);

    client.set_state(GAME_1, STATE_CHALLENGED);
    defender.scan_once().await.unwrap();

    assert!(prover.requests().is_empty());
    assert_eq!(defender.active_defenses(), [GAME_1]);

    defender.scan_once().await.unwrap();
    assert_eq!(prover.requests().len(), 2);
}

#[tokio::test]
async fn scan_once_ignores_challenged_invalid_root() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let client = MockClient::new(
        vec![(GAME_1, proposed_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    defender.scan_once().await.unwrap();

    // an invalid root is the challenger's business, not ours
    assert!(prover.requests().is_empty());
    assert!(defender.watched_games().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn scan_once_defers_validity_check_until_l2_block_is_finalized() {
    let canonical_root = B256::repeat_byte(0x20);

    let client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    let (output_roots, finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK - 1);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    // the game's L2 block is not finalized yet, so the root cannot be judged
    defender.scan_once().await.unwrap();
    assert!(prover.requests().is_empty());
    assert_eq!(defender.watched_games(), [GAME_1]);

    finalized_l2_block.store(L2_BLOCK, Ordering::SeqCst);
    defender.scan_once().await.unwrap();

    assert!(prover.requests().is_empty());
    assert_eq!(defender.active_defenses(), [GAME_1]);

    defender.scan_once().await.unwrap();
    assert_eq!(prover.requests().len(), 2);
}

#[tokio::test]
async fn scan_once_skips_lane_already_proven() {
    let canonical_root = B256::repeat_byte(0x20);

    let client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    client.set_bitmap(GAME_1, ProofLane::ValidityProof.mask());
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    defender.scan_once().await.unwrap();
    defender.scan_once().await.unwrap();
    defender.scan_once().await.unwrap();

    // the validity lane is already proven on-chain: only the TEE lane runs
    let requests = prover.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].backend, ProofBackend::Nitro);
    assert_eq!(
        client.submissions(),
        vec![(GAME_1, ProofLane::TeeAttestation as u8)]
    );
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn failed_proofs_are_rerequested_up_to_the_attempt_bound() {
    let canonical_root = B256::repeat_byte(0x20);

    let client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::failing();
    let mut defender = WorldChainDefender::new(
        DefenderConfig {
            max_proof_attempts: 2,
            ..config()
        },
        client.clone(),
        output_roots,
        prover.clone(),
    );

    // Tick 1 promotes the game. Tick 2 makes the first request per lane;
    // tick 3 re-requests each failed proof; tick 4 exhausts the attempts.
    defender.scan_once().await.unwrap();
    defender.scan_once().await.unwrap();
    defender.scan_once().await.unwrap();
    defender.scan_once().await.unwrap();

    assert_eq!(prover.requests().len(), 4);
    assert!(client.submissions().is_empty());
    assert!(defender.active_defenses().is_empty());
}

#[tokio::test]
async fn defense_closes_when_game_leaves_challenged_state() {
    let canonical_root = B256::repeat_byte(0x20);

    let client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

    defender.scan_once().await.unwrap();
    assert_eq!(defender.active_defenses(), [GAME_1]);

    client.set_state(GAME_1, STATE_FINALIZED);
    defender.scan_once().await.unwrap();

    assert!(client.submissions().is_empty());
    assert!(defender.active_defenses().is_empty());
}
