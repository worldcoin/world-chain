use crate::{
    config::DefenderConfig, defender::WorldChainDefender, error::DefenderError,
    traits::DefenderClient, types::DefenderSubmission,
};
use alloy_primitives::{Address, B256, BlockNumber, Bytes, address};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use world_chain_proofs::{ConsensusError, ConsensusProvider, GameCreated, ProofLane, RootState};
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequestError, ProofRequestId, ProofRequester,
    ProofResponse, ProofStatus,
};

const GAME_1: Address = address!("0000000000000000000000000000000000000001");
const FINALIZED_L1_BLOCK: BlockNumber = 10_000;
const L2_BLOCK: u64 = 100;
const L1_ORIGIN_HASH: B256 = B256::repeat_byte(0x42);

/// State discriminant matching [`RootState::Proposed`].
const STATE_PROPOSED: u8 = 1;
/// State discriminant matching [`RootState::Challenged`].
const STATE_CHALLENGED: u8 = 2;
/// State discriminant matching [`RootState::Finalized`].
const STATE_FINALIZED: u8 = 3;

#[derive(Debug, Clone)]
struct MockClient {
    finalized: BlockNumber,
    games: Vec<(Address, B256, u64)>,
    states: Arc<Mutex<HashMap<Address, u8>>>,
    deadlines: HashMap<Address, u64>,
    bitmaps: HashMap<Address, u8>,
    submissions: Arc<Mutex<Vec<(Address, u8)>>>,
}

impl MockClient {
    fn new(games: Vec<(Address, B256, u64)>, states: HashMap<Address, u8>) -> Self {
        Self {
            finalized: FINALIZED_L1_BLOCK,
            games,
            states: Arc::new(Mutex::new(states)),
            deadlines: HashMap::new(),
            bitmaps: HashMap::new(),
            submissions: Arc::default(),
        }
    }

    fn set_state(&self, game: Address, state: u8) {
        self.states
            .lock()
            .expect("not poisoned")
            .insert(game, state);
    }

    fn submissions(&self) -> Vec<(Address, u8)> {
        self.submissions.lock().expect("not poisoned").clone()
    }
}

#[async_trait]
impl DefenderClient for MockClient {
    async fn root_state(&self, game: Address) -> Result<RootState, DefenderError> {
        let raw = self
            .states
            .lock()
            .expect("not poisoned")
            .get(&game)
            .copied()
            .unwrap_or(STATE_PROPOSED);
        RootState::try_from(raw).map_err(Into::into)
    }

    async fn finalized_l1_block_num(&self) -> Result<BlockNumber, DefenderError> {
        Ok(self.finalized)
    }

    async fn games_created(
        &self,
        _from: BlockNumber,
        _to: BlockNumber,
    ) -> Result<Vec<GameCreated>, DefenderError> {
        Ok(self
            .games
            .iter()
            .map(|&(game, root_claim, l2_block_number)| GameCreated {
                proposal_key: B256::ZERO,
                root_id: B256::ZERO,
                game,
                proposer: Address::ZERO,
                root_claim,
                l2_block_number,
                parent_ref: Address::ZERO,
                l1_origin_hash: L1_ORIGIN_HASH,
                l1_origin_number: 0,
            })
            .collect())
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, DefenderError> {
        Ok(self.deadlines.get(&game).copied().unwrap_or(u64::MAX))
    }

    async fn proof_bitmap(&self, game: Address) -> Result<u8, DefenderError> {
        Ok(self.bitmaps.get(&game).copied().unwrap_or(0))
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        _proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
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
            .ok_or(ProofRequestError::NotFound(proof_id))?;
        let proof = match request.backend {
            ProofBackend::Sp1 => ProofData::Sp1 {
                proof: Bytes::from_static(b"proof"),
                public_values: Bytes::from_static(b"public values"),
            },
            ProofBackend::Nitro => ProofData::Nitro {
                attestation: Bytes::from_static(b"attestation"),
                signature: Bytes::from_static(b"signature"),
            },
        };
        Ok(ProofResponse {
            id: proof_id,
            proof,
        })
    }
}

fn config() -> DefenderConfig {
    DefenderConfig {
        poll_interval: Duration::from_secs(1),
        ..DefenderConfig::default()
    }
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

    // first tick: the challenged game is promoted to an active defense and
    // both lane proofs are requested
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

    // second tick: both proofs are completed, fetched and submitted on-chain
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

    assert_eq!(prover.requests().len(), 2);
    assert_eq!(defender.active_defenses(), [GAME_1]);
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

    assert_eq!(prover.requests().len(), 2);
    assert_eq!(defender.active_defenses(), [GAME_1]);
}

#[tokio::test]
async fn scan_once_skips_lane_already_proven() {
    let canonical_root = B256::repeat_byte(0x20);

    let mut client = MockClient::new(
        vec![(GAME_1, canonical_root, L2_BLOCK)],
        HashMap::from([(GAME_1, STATE_CHALLENGED)]),
    );
    client
        .bitmaps
        .insert(GAME_1, ProofLane::ValidityProof.mask());
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let prover = MockProver::default();
    let mut defender =
        WorldChainDefender::new(config(), client.clone(), output_roots, prover.clone());

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

    // tick 1: first request per lane; tick 2: failed, re-requested once per
    // lane; tick 3: failed again, attempts exhausted, lanes abandoned
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
