use crate::{
    challenger::WorldChainChallenger, config::ChallengerConfig, error::ChallengerError,
    traits::ChallengerClient, types::ChallengeSubmission,
};
use alloy_primitives::{Address, B256, BlockNumber, U256, address};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use world_chain_proofs::{ConsensusError, ConsensusProvider, GameCreated, RootState};

const GAME_1: Address = address!("0000000000000000000000000000000000000001");
const GAME_2: Address = address!("0000000000000000000000000000000000000002");
const FINALIZED_L1_BLOCK: BlockNumber = 10_000;
const L2_BLOCK: u64 = 100;

/// State discriminant matching [`RootState::Proposed`].
const STATE_PROPOSED: u8 = 1;
/// State discriminant matching [`RootState::Challenged`].
const STATE_CHALLENGED: u8 = 2;

#[derive(Debug, Clone)]
struct MockClient {
    finalized: BlockNumber,
    games: Vec<(Address, B256, u64)>,
    states: HashMap<Address, u8>,
    deadlines: HashMap<Address, u64>,
    submissions: Arc<Mutex<Vec<Address>>>,
}

#[async_trait]
impl ChallengerClient for MockClient {
    async fn root_state(&self, game: Address) -> Result<RootState, ChallengerError> {
        let raw = self.states.get(&game).copied().unwrap_or(STATE_PROPOSED);
        RootState::try_from(raw).map_err(Into::into)
    }

    async fn finalized_l1_block_num(&self) -> Result<BlockNumber, ChallengerError> {
        Ok(self.finalized)
    }

    async fn games_created(
        &self,
        _from: BlockNumber,
        _to: BlockNumber,
    ) -> Result<Vec<GameCreated>, ChallengerError> {
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
                l1_origin_hash: B256::ZERO,
                l1_origin_number: 0,
            })
            .collect())
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError> {
        Ok(self.deadlines.get(&game).copied().unwrap_or(u64::MAX))
    }

    async fn submit_challenge(
        &self,
        game: Address,
        _challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        self.submissions.lock().expect("not poisoned").push(game);
        Ok(ChallengeSubmission {
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

fn config() -> ChallengerConfig {
    ChallengerConfig {
        challenger_bond: U256::from(1),
        poll_interval: Duration::from_secs(1),
        ..ChallengerConfig::default()
    }
}

#[tokio::test]
async fn scan_once_challenges_invalid_root() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, proposed_root, L2_BLOCK)],
        states: HashMap::new(),
        deadlines: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    assert_eq!(
        submissions.lock().expect("not poisoned").as_slice(),
        &[GAME_1]
    );
}

#[tokio::test]
async fn scan_once_leaves_valid_root() {
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, canonical_root, L2_BLOCK)],
        states: HashMap::new(),
        deadlines: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    assert!(submissions.lock().expect("not poisoned").is_empty());
    assert!(challenger.retry_games().is_empty());
}

#[tokio::test]
async fn scan_once_skips_non_proposed_game() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, proposed_root, L2_BLOCK)],
        states: HashMap::from([(GAME_1, STATE_CHALLENGED)]),
        deadlines: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    assert!(submissions.lock().expect("not poisoned").is_empty());
    assert!(challenger.retry_games().is_empty());
}

#[tokio::test]
async fn scan_once_skips_expired_challenge_deadline() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, proposed_root, L2_BLOCK)],
        states: HashMap::new(),
        deadlines: HashMap::from([(GAME_1, 0)]),
        submissions: Arc::clone(&submissions),
    };
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    assert!(submissions.lock().expect("not poisoned").is_empty());
    assert!(challenger.retry_games().is_empty());
}

#[tokio::test]
async fn scan_once_rejects_unfinalized_l2_block() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, proposed_root, L2_BLOCK)],
        states: HashMap::new(),
        deadlines: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let (output_roots, _finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK - 1);
    let mut challenger = WorldChainChallenger::new(config(), client, output_roots);

    // The game references an L2 block that is not finalized yet. This is a
    // transient failure, so the game must not be challenged and instead be
    // queued for a later retry, while the scan itself succeeds.
    challenger.scan_once().await.unwrap();

    assert!(submissions.lock().expect("not poisoned").is_empty());
    assert_eq!(challenger.retry_games(), [GAME_1]);
}

#[tokio::test]
async fn scan_once_processes_retry_games_without_new_l1_blocks() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![(GAME_1, proposed_root, L2_BLOCK)],
        states: HashMap::new(),
        deadlines: HashMap::new(),
        submissions: Arc::clone(&submissions),
    };
    let (output_roots, finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK - 1);
    let mut challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();
    assert_eq!(challenger.retry_games(), [GAME_1]);

    finalized_l2_block.store(L2_BLOCK, Ordering::SeqCst);
    challenger.scan_once().await.unwrap();

    assert_eq!(
        submissions.lock().expect("not poisoned").as_slice(),
        &[GAME_1]
    );
    assert!(challenger.retry_games().is_empty());
}

#[tokio::test]
async fn scan_once_processes_retry_games_by_challenge_deadline() {
    let proposed_root_1 = B256::repeat_byte(0x10);
    let proposed_root_2 = B256::repeat_byte(0x11);
    let canonical_root_1 = B256::repeat_byte(0x20);
    let canonical_root_2 = B256::repeat_byte(0x21);
    let l2_block_2 = L2_BLOCK + 1;

    let submissions = Arc::default();
    let client = MockClient {
        finalized: FINALIZED_L1_BLOCK,
        games: vec![
            (GAME_1, proposed_root_1, L2_BLOCK),
            (GAME_2, proposed_root_2, l2_block_2),
        ],
        states: HashMap::new(),
        deadlines: HashMap::from([(GAME_1, u64::MAX), (GAME_2, u64::MAX - 1)]),
        submissions: Arc::clone(&submissions),
    };
    let (output_roots, finalized_l2_block) = mock_output_roots(
        HashMap::from([(L2_BLOCK, canonical_root_1), (l2_block_2, canonical_root_2)]),
        L2_BLOCK - 1,
    );
    let mut challenger = WorldChainChallenger::new(config(), client, output_roots);

    challenger.scan_once().await.unwrap();

    finalized_l2_block.store(l2_block_2, Ordering::SeqCst);
    challenger.scan_once().await.unwrap();

    assert_eq!(
        submissions.lock().expect("not poisoned").as_slice(),
        &[GAME_2, GAME_1]
    );
    assert!(challenger.retry_games().is_empty());
}
