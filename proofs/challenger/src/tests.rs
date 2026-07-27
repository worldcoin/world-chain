use crate::{
    BondManager, BondManagerClient, BondManagerConfig, ChallengeSubmission, ChallengerClient,
    ChallengerConfig, ChallengerError, GameMetadata, OwnedGames, ResolutionManager,
    ResolutionManagerClient, ResolutionManagerConfig, ResolveSubmission, WithdrawSubmission,
    challenger::WorldChainChallenger,
};
use alloy_primitives::{Address, B256, BlockNumber, U256, address};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use world_chain_proofs::{
    ConsensusError, ConsensusProvider, InvalidationReason, ResolutionStatus, RootState,
};

const CHALLENGER: Address = address!("00000000000000000000000000000000000000cc");
const GAME_1: Address = address!("0000000000000000000000000000000000000001");
const GAME_2: Address = address!("0000000000000000000000000000000000000002");
const GAME_3: Address = address!("0000000000000000000000000000000000000003");
const L2_BLOCK: u64 = 100;

const STATE_PROPOSED: u8 = 1;
const STATE_CHALLENGED: u8 = 2;
const STATE_FINALIZED: u8 = 3;
const STATE_INVALIDATED: u8 = 4;
const REASON_NONE: u8 = 0;
const REASON_PROOF_TIMEOUT: u8 = 1;

#[derive(Debug, Clone, Copy)]
struct MockGame {
    metadata: GameMetadata,
    state: u8,
    challenge_deadline: u64,
    challenger: Address,
    resolvable: bool,
    resolution_outcome: u8,
    resolution_reason: u8,
    claimable: U256,
}

impl MockGame {
    fn proposed(address: Address, root_claim: B256, l2_block_number: u64) -> Self {
        Self {
            metadata: GameMetadata {
                address,
                root_claim,
                l2_block_number,
            },
            state: STATE_PROPOSED,
            challenge_deadline: u64::MAX,
            challenger: Address::ZERO,
            resolvable: false,
            resolution_outcome: STATE_PROPOSED,
            resolution_reason: REASON_NONE,
            claimable: U256::ZERO,
        }
    }
}

#[derive(Debug, Default)]
struct MockState {
    order: Vec<Address>,
    games: HashMap<Address, MockGame>,
    requested_indices: Vec<u64>,
    challenges: Vec<Address>,
    resolutions: Vec<Address>,
    withdrawals: Vec<Address>,
    fail_withdraw_once: HashSet<Address>,
}

#[derive(Debug, Clone)]
struct MockClient {
    state: Arc<Mutex<MockState>>,
}

impl MockClient {
    fn new(games: Vec<MockGame>) -> Self {
        let order = games.iter().map(|game| game.metadata.address).collect();
        let games = games
            .into_iter()
            .map(|game| (game.metadata.address, game))
            .collect();
        Self {
            state: Arc::new(Mutex::new(MockState {
                order,
                games,
                ..MockState::default()
            })),
        }
    }

    fn challenges(&self) -> Vec<Address> {
        self.state.lock().expect("not poisoned").challenges.clone()
    }

    fn resolutions(&self) -> Vec<Address> {
        self.state.lock().expect("not poisoned").resolutions.clone()
    }

    fn withdrawals(&self) -> Vec<Address> {
        self.state.lock().expect("not poisoned").withdrawals.clone()
    }
}

#[async_trait]
impl ChallengerClient for MockClient {
    async fn challenger_bond(&self) -> Result<U256, ChallengerError> {
        Ok(U256::from(1))
    }

    async fn game_count(&self) -> Result<u64, ChallengerError> {
        Ok(self.state.lock().expect("not poisoned").order.len() as u64)
    }

    async fn game_address_at(&self, index: u64) -> Result<Address, ChallengerError> {
        let mut state = self.state.lock().expect("not poisoned");
        state.requested_indices.push(index);
        state
            .order
            .get(index as usize)
            .copied()
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game index {index}")))
    }

    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.metadata)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))
    }

    async fn root_state(&self, game: Address) -> Result<RootState, ChallengerError> {
        let raw = self
            .state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map_or(0, |game| game.state);
        RootState::try_from(raw).map_err(Into::into)
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.challenge_deadline)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))
    }

    async fn submit_challenge(
        &self,
        game: Address,
        _challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        let mut state = self.state.lock().expect("not poisoned");
        let record = state
            .games
            .get_mut(&game)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))?;
        record.state = STATE_CHALLENGED;
        record.challenger = CHALLENGER;
        record.resolution_outcome = STATE_CHALLENGED;
        state.challenges.push(game);
        Ok(ChallengeSubmission {
            tx_hash: B256::with_last_byte(state.challenges.len() as u8),
        })
    }
}

#[async_trait]
impl ResolutionManagerClient for MockClient {
    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ChallengerError> {
        let record = self
            .state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .copied()
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))?;
        Ok(ResolutionStatus {
            resolvable: record.resolvable,
            root_state: RootState::try_from(record.resolution_outcome)?,
            invalidation_reason: InvalidationReason::try_from(record.resolution_reason)
                .map_err(|error| ChallengerError::Contract(error.to_string()))?,
        })
    }

    async fn resolve(&self, game: Address) -> Result<ResolveSubmission, ChallengerError> {
        let mut state = self.state.lock().expect("not poisoned");
        let record = state
            .games
            .get_mut(&game)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))?;
        record.state = record.resolution_outcome;
        record.resolvable = false;
        state.resolutions.push(game);
        Ok(ResolveSubmission {
            tx_hash: B256::with_last_byte(state.resolutions.len() as u8),
        })
    }
}

#[async_trait]
impl BondManagerClient for MockClient {
    fn challenger_address(&self) -> Address {
        CHALLENGER
    }

    async fn game_count(&self) -> Result<u64, ChallengerError> {
        ChallengerClient::game_count(self).await
    }

    async fn game_address_at(&self, index: u64) -> Result<Address, ChallengerError> {
        ChallengerClient::game_address_at(self, index).await
    }

    async fn game_challenger(&self, game: Address) -> Result<Address, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.challenger)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))
    }

    async fn claimable(&self, game: Address) -> Result<U256, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.claimable)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))
    }

    async fn withdraw(&self, game: Address) -> Result<WithdrawSubmission, ChallengerError> {
        let mut state = self.state.lock().expect("not poisoned");
        if state.fail_withdraw_once.remove(&game) {
            return Err(ChallengerError::Contract(
                "injected withdrawal failure".into(),
            ));
        }
        let record = state
            .games
            .get_mut(&game)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))?;
        let amount = record.claimable;
        record.claimable = U256::ZERO;
        state.withdrawals.push(game);
        Ok(WithdrawSubmission {
            tx_hash: B256::with_last_byte(state.withdrawals.len() as u8),
            amount,
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
        max_game_concurrency: 10,
        max_games_per_tick: 100,
    }
}

#[tokio::test]
async fn scan_once_challenges_invalid_root_and_tracks_game() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);
    let client = MockClient::new(vec![MockGame::proposed(GAME_1, proposed_root, L2_BLOCK)]);
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let owned_games = OwnedGames::default();
    let mut challenger = WorldChainChallenger::with_owned_games(
        config(),
        client.clone(),
        output_roots,
        owned_games.clone(),
    );

    challenger.scan_once().await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_1]);
    assert!(owned_games.contains(GAME_1));
    assert_eq!(challenger.next_game_index(), Some(1));
}

#[tokio::test]
async fn startup_binary_search_skips_expired_games() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);
    let mut expired = MockGame::proposed(GAME_1, proposed_root, L2_BLOCK);
    expired.challenge_deadline = 0;
    let active = MockGame::proposed(GAME_2, proposed_root, L2_BLOCK);
    let client = MockClient::new(vec![expired, active]);
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client.clone(), output_roots);

    challenger.scan_once().await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_2]);
    assert_eq!(challenger.next_game_index(), Some(2));
}

#[tokio::test]
async fn scan_once_respects_new_game_tick_budget() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);
    let client = MockClient::new(vec![
        MockGame::proposed(GAME_1, proposed_root, L2_BLOCK),
        MockGame::proposed(GAME_2, proposed_root, L2_BLOCK),
        MockGame::proposed(GAME_3, proposed_root, L2_BLOCK),
    ]);
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut limited_config = config();
    limited_config.max_games_per_tick = 2;
    let mut challenger = WorldChainChallenger::new(limited_config, client.clone(), output_roots);

    challenger.scan_once().await.unwrap();
    assert_eq!(client.challenges(), vec![GAME_1, GAME_2]);
    assert_eq!(challenger.next_game_index(), Some(2));

    challenger.scan_once().await.unwrap();
    assert_eq!(client.challenges(), vec![GAME_1, GAME_2, GAME_3]);
    assert_eq!(challenger.next_game_index(), Some(3));
}

#[tokio::test]
async fn scan_once_leaves_valid_and_non_proposed_games() {
    let canonical_root = B256::repeat_byte(0x20);
    let valid = MockGame::proposed(GAME_1, canonical_root, L2_BLOCK);
    let mut challenged = MockGame::proposed(GAME_2, B256::repeat_byte(0x10), L2_BLOCK);
    challenged.state = STATE_CHALLENGED;
    let client = MockClient::new(vec![valid, challenged]);
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client.clone(), output_roots);

    challenger.scan_once().await.unwrap();

    assert!(client.challenges().is_empty());
    assert!(challenger.retry_games().is_empty());
}

#[tokio::test]
async fn retry_game_is_challenged_after_l2_finalizes() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);
    let client = MockClient::new(vec![MockGame::proposed(GAME_1, proposed_root, L2_BLOCK)]);
    let (output_roots, finalized_l2_block) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK - 1);
    let mut challenger = WorldChainChallenger::new(config(), client.clone(), output_roots);

    challenger.scan_once().await.unwrap();
    assert_eq!(challenger.retry_games(), vec![GAME_1]);
    assert!(client.challenges().is_empty());

    finalized_l2_block.store(L2_BLOCK, Ordering::SeqCst);
    challenger.scan_once().await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_1]);
    assert!(challenger.retry_games().is_empty());
}

#[tokio::test]
async fn resolution_manager_obeys_transaction_budget() {
    let mut first = MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK);
    first.state = STATE_CHALLENGED;
    first.resolvable = true;
    first.resolution_outcome = STATE_INVALIDATED;
    first.resolution_reason = REASON_PROOF_TIMEOUT;
    let mut second = first;
    second.metadata.address = GAME_2;
    let client = MockClient::new(vec![first, second]);
    let owned_games = OwnedGames::default();
    owned_games.insert(GAME_1);
    owned_games.insert(GAME_2);
    let manager = ResolutionManager::new(
        ResolutionManagerConfig {
            poll_interval: Duration::from_secs(30),
            max_resolutions_per_tick: 1,
        },
        client.clone(),
        owned_games,
    );

    manager.resolve_games().await.unwrap();

    assert_eq!(client.resolutions(), vec![GAME_1]);
}

#[tokio::test]
async fn bond_manager_recovers_only_recent_owned_games() {
    let mut old_owned = MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK);
    old_owned.challenger = CHALLENGER;
    let unowned = MockGame::proposed(GAME_2, B256::ZERO, L2_BLOCK);
    let mut recent_owned = MockGame::proposed(GAME_3, B256::ZERO, L2_BLOCK);
    recent_owned.challenger = CHALLENGER;
    let client = MockClient::new(vec![old_owned, unowned, recent_owned]);
    let owned_games = OwnedGames::default();
    let mut manager = BondManager::new(
        BondManagerConfig {
            poll_interval: Duration::from_secs(300),
            initial_scan_limit: 2,
        },
        client,
        owned_games.clone(),
    );

    manager.scan_games().await.unwrap();

    assert!(!owned_games.contains(GAME_1));
    assert!(owned_games.contains(GAME_3));
    assert_eq!(manager.next_game_index(), Some(3));
}

#[tokio::test]
async fn bond_manager_scans_games_appended_after_recovery() {
    let client = MockClient::new(vec![MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK)]);
    let owned_games = OwnedGames::default();
    let mut manager = BondManager::new(
        BondManagerConfig::default(),
        client.clone(),
        owned_games.clone(),
    );
    manager.scan_games().await.unwrap();

    let mut appended = MockGame::proposed(GAME_2, B256::ZERO, L2_BLOCK);
    appended.challenger = CHALLENGER;
    {
        let mut state = client.state.lock().expect("not poisoned");
        state.order.push(GAME_2);
        state.games.insert(GAME_2, appended);
    }

    manager.scan_games().await.unwrap();

    assert!(owned_games.contains(GAME_2));
    assert_eq!(manager.next_game_index(), Some(2));
}

#[tokio::test]
async fn bond_manager_withdraws_and_prunes_terminal_games() {
    let mut withdrawable = MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK);
    withdrawable.state = STATE_INVALIDATED;
    withdrawable.resolution_outcome = STATE_INVALIDATED;
    withdrawable.resolution_reason = REASON_PROOF_TIMEOUT;
    withdrawable.claimable = U256::from(10);
    let mut zero_credit = MockGame::proposed(GAME_2, B256::ZERO, L2_BLOCK);
    zero_credit.state = STATE_FINALIZED;
    zero_credit.resolution_outcome = STATE_FINALIZED;
    let client = MockClient::new(vec![withdrawable, zero_credit]);
    let owned_games = OwnedGames::default();
    owned_games.insert(GAME_1);
    owned_games.insert(GAME_2);
    let manager = BondManager::new(
        BondManagerConfig::default(),
        client.clone(),
        owned_games.clone(),
    );

    manager.withdraw_credits().await.unwrap();

    assert_eq!(client.withdrawals(), vec![GAME_1]);
    assert!(!owned_games.contains(GAME_1));
    assert!(!owned_games.contains(GAME_2));
}

#[tokio::test]
async fn bond_manager_retries_failed_withdrawal() {
    let mut game = MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK);
    game.state = STATE_INVALIDATED;
    game.resolution_outcome = STATE_INVALIDATED;
    game.resolution_reason = REASON_PROOF_TIMEOUT;
    game.claimable = U256::from(10);
    let client = MockClient::new(vec![game]);
    client
        .state
        .lock()
        .expect("not poisoned")
        .fail_withdraw_once
        .insert(GAME_1);
    let owned_games = OwnedGames::default();
    owned_games.insert(GAME_1);
    let manager = BondManager::new(
        BondManagerConfig::default(),
        client.clone(),
        owned_games.clone(),
    );

    manager.withdraw_credits().await.unwrap();
    assert!(owned_games.contains(GAME_1));

    manager.withdraw_credits().await.unwrap();
    assert!(!owned_games.contains(GAME_1));
    assert_eq!(client.withdrawals(), vec![GAME_1]);
}
