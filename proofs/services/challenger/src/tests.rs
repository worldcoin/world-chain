use crate::{
    BondManager, BondManagerClient, BondManagerConfig, ChallengeSubmission, ChallengerClient,
    ChallengerConfig, ChallengerError, ClaimSubmission, GameMetadata, OwnedGames,
    PendingWithdrawal, ResolutionManager, ResolutionManagerClient, ResolutionManagerConfig,
    ResolveSubmission, challenger::WorldChainChallenger,
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
use world_chain_proof_protocol::{
    ConsensusError, ConsensusProvider, GameStatus, InvalidationReason, ProposalStatus,
    ResolutionStatus,
};

const CHALLENGER: Address = address!("00000000000000000000000000000000000000cc");
const GAME_1: Address = address!("0000000000000000000000000000000000000001");
const GAME_2: Address = address!("0000000000000000000000000000000000000002");
const GAME_3: Address = address!("0000000000000000000000000000000000000003");
const L2_BLOCK: u64 = 100;

const REASON_NONE: u8 = 0;
const REASON_PROOF_TIMEOUT: u8 = 1;

#[derive(Debug, Clone, Copy)]
struct MockGame {
    metadata: GameMetadata,
    proposal_status: ProposalStatus,
    challenge_deadline: u64,
    challenger: Address,
    resolvable: bool,
    resolution_outcome: GameStatus,
    resolution_reason: u8,
    credit: U256,
    pending: PendingWithdrawal,
    /// Whether the registry's finality airgap has elapsed for this game.
    finalized: bool,
}

impl MockGame {
    fn proposed(address: Address, root_claim: B256, l2_block_number: u64) -> Self {
        Self {
            metadata: GameMetadata {
                address,
                root_claim,
                l2_block_number,
            },
            proposal_status: ProposalStatus::Unchallenged,
            challenge_deadline: u64::MAX,
            challenger: Address::ZERO,
            resolvable: false,
            resolution_outcome: GameStatus::InProgress,
            resolution_reason: REASON_NONE,
            credit: U256::ZERO,
            pending: PendingWithdrawal {
                amount: U256::ZERO,
                unlock_at: 0,
            },
            finalized: true,
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
    unlocks: Vec<Address>,
    withdrawals: Vec<Address>,
    fail_claim_once: HashSet<Address>,
    latest_l1_timestamp: u64,
    /// Factory indices holding a game of a different type.
    foreign_indices: HashSet<u64>,
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
    async fn game_count(&self) -> Result<u64, ChallengerError> {
        Ok(self.state.lock().expect("not poisoned").order.len() as u64)
    }

    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        let mut state = self.state.lock().expect("not poisoned");
        state.requested_indices.push(index);
        if state.foreign_indices.contains(&index) {
            return Ok(None);
        }
        state
            .order
            .get(index as usize)
            .copied()
            .map(Some)
            .ok_or_else(|| ChallengerError::message(format!("unknown game index {index}")))
    }

    async fn game_metadata(&self, game: Address) -> Result<GameMetadata, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.metadata)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))
    }

    async fn proposal_status(&self, game: Address) -> Result<ProposalStatus, ChallengerError> {
        Ok(self
            .state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map_or(ProposalStatus::Resolved, |game| game.proposal_status))
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.challenge_deadline)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))
    }

    async fn submit_challenge(
        &self,
        game: Address,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        let mut state = self.state.lock().expect("not poisoned");
        let record = state
            .games
            .get_mut(&game)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))?;
        record.proposal_status = ProposalStatus::Challenged;
        record.challenger = CHALLENGER;
        record.resolution_outcome = GameStatus::InProgress;
        state.challenges.push(game);
        Ok(ChallengeSubmission {
            tx_hash: B256::with_last_byte(state.challenges.len() as u8),
            bond: U256::from(1),
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
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))?;
        Ok(ResolutionStatus {
            resolvable: record.resolvable,
            outcome: record.resolution_outcome,
            invalidation_reason: InvalidationReason::try_from(record.resolution_reason)?,
        })
    }

    async fn resolve(&self, game: Address) -> Result<ResolveSubmission, ChallengerError> {
        let mut state = self.state.lock().expect("not poisoned");
        let record = state
            .games
            .get_mut(&game)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))?;
        record.proposal_status = ProposalStatus::Resolved;
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

    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        ChallengerClient::game_address_at(self, index).await
    }

    async fn game_challenger(&self, game: Address) -> Result<Address, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.challenger)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.finalized)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))
    }

    async fn credit(&self, game: Address) -> Result<U256, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.credit)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))
    }

    async fn pending_withdrawal(
        &self,
        game: Address,
    ) -> Result<PendingWithdrawal, ChallengerError> {
        self.state
            .lock()
            .expect("not poisoned")
            .games
            .get(&game)
            .map(|game| game.pending)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))
    }

    async fn latest_l1_timestamp(&self) -> Result<u64, ChallengerError> {
        Ok(self.state.lock().expect("not poisoned").latest_l1_timestamp)
    }

    /// Mirrors `MultiProofGame.claimCredit`: the first call unlocks credit into a pending
    /// `DelayedWETH` withdrawal, the second drains it.
    async fn claim_credit(&self, game: Address) -> Result<ClaimSubmission, ChallengerError> {
        let mut state = self.state.lock().expect("not poisoned");
        if state.fail_claim_once.remove(&game) {
            return Err(ChallengerError::message("injected claim failure"));
        }
        let record = state
            .games
            .get_mut(&game)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))?;

        if record.credit > U256::ZERO {
            let amount = record.credit;
            record.credit = U256::ZERO;
            record.pending = PendingWithdrawal {
                amount,
                unlock_at: 0,
            };
            state.unlocks.push(game);
            return Ok(ClaimSubmission {
                tx_hash: B256::with_last_byte(state.unlocks.len() as u8),
                amount,
                withdrawn: false,
            });
        }

        let amount = record.pending.amount;
        record.pending = PendingWithdrawal::default();
        state.withdrawals.push(game);
        Ok(ClaimSubmission {
            tx_hash: B256::with_last_byte(state.withdrawals.len() as u8),
            amount,
            withdrawn: true,
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
        poll_interval: Duration::from_secs(1),
        max_game_concurrency: 10,
        max_games_per_tick: 100,
        game_scan_lookback: 100,
    }
}

#[tokio::test]
async fn tick_challenges_invalid_root_and_tracks_game() {
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

    challenger.tick_at(1).await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_1]);
    assert!(owned_games.contains(GAME_1));
    assert_eq!(challenger.next_game_index(), Some(1));
}

#[tokio::test]
async fn startup_binary_search_finds_first_live_game_by_deadline() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);
    let mut expired = MockGame::proposed(GAME_1, proposed_root, L2_BLOCK);
    expired.challenge_deadline = 0;
    let active = MockGame::proposed(GAME_2, proposed_root, L2_BLOCK);
    let client = MockClient::new(vec![expired, active]);
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client.clone(), output_roots);

    challenger.tick_at(1).await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_2]);
    assert_eq!(challenger.next_game_index(), Some(2));
}

#[tokio::test]
async fn startup_binary_search_skips_games_older_than_max_age() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);
    let mut old = MockGame::proposed(GAME_1, proposed_root, L2_BLOCK);
    old.challenge_deadline = 0;
    let recent = MockGame::proposed(GAME_2, proposed_root, L2_BLOCK);
    let client = MockClient::new(vec![old, recent]);
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client.clone(), output_roots);

    challenger.tick_at(1).await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_2]);
    assert_eq!(challenger.next_game_index(), Some(2));
}

#[tokio::test]
async fn tick_respects_new_game_budget() {
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

    challenger.tick_at(1).await.unwrap();
    assert_eq!(client.challenges(), vec![GAME_1, GAME_2]);
    assert_eq!(challenger.next_game_index(), Some(2));

    challenger.tick_at(1).await.unwrap();
    assert_eq!(client.challenges(), vec![GAME_1, GAME_2, GAME_3]);
    assert_eq!(challenger.next_game_index(), Some(3));
}

#[tokio::test]
async fn tick_rechecks_lookback_without_reducing_forward_progress() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);
    let client = MockClient::new(vec![
        MockGame::proposed(GAME_1, proposed_root, L2_BLOCK),
        MockGame::proposed(GAME_2, proposed_root, L2_BLOCK),
        MockGame::proposed(GAME_3, proposed_root, L2_BLOCK),
    ]);
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger_config = config();
    challenger_config.max_game_concurrency = 1;
    challenger_config.max_games_per_tick = 2;
    challenger_config.game_scan_lookback = 1;
    let mut challenger = WorldChainChallenger::new(challenger_config, client.clone(), output_roots);

    challenger.tick_at(1).await.unwrap();
    client
        .state
        .lock()
        .expect("not poisoned")
        .games
        .get_mut(&GAME_2)
        .expect("game exists")
        .proposal_status = ProposalStatus::Unchallenged;

    challenger.tick_at(1).await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_1, GAME_2, GAME_2, GAME_3]);
    assert_eq!(challenger.next_game_index(), Some(3));
}

#[tokio::test]
async fn tick_leaves_valid_and_non_proposed_games() {
    let canonical_root = B256::repeat_byte(0x20);
    let valid = MockGame::proposed(GAME_1, canonical_root, L2_BLOCK);
    let mut challenged = MockGame::proposed(GAME_2, B256::repeat_byte(0x10), L2_BLOCK);
    challenged.proposal_status = ProposalStatus::Challenged;
    let client = MockClient::new(vec![valid, challenged]);
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client.clone(), output_roots);

    challenger.tick_at(1).await.unwrap();

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

    challenger.tick_at(1).await.unwrap();
    assert_eq!(challenger.retry_games(), vec![GAME_1]);
    assert!(client.challenges().is_empty());

    finalized_l2_block.store(L2_BLOCK, Ordering::SeqCst);
    challenger.tick_at(1).await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_1]);
    assert!(challenger.retry_games().is_empty());
}

#[tokio::test]
async fn resolution_manager_obeys_transaction_budget() {
    let mut first = MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK);
    first.proposal_status = ProposalStatus::Challenged;
    first.resolvable = true;
    first.resolution_outcome = GameStatus::ChallengerWins;
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
async fn bond_manager_completes_two_phase_claim_and_prunes_terminal_games() {
    let mut claimable = MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK);
    claimable.proposal_status = ProposalStatus::Resolved;
    claimable.resolution_outcome = GameStatus::ChallengerWins;
    claimable.resolution_reason = REASON_PROOF_TIMEOUT;
    claimable.credit = U256::from(10);
    let mut zero_credit = MockGame::proposed(GAME_2, B256::ZERO, L2_BLOCK);
    zero_credit.proposal_status = ProposalStatus::Resolved;
    zero_credit.resolution_outcome = GameStatus::DefenderWins;
    // Resolved but still inside the registry's finality airgap: `closeGame` would revert.
    let mut awaiting_airgap = MockGame::proposed(GAME_3, B256::ZERO, L2_BLOCK);
    awaiting_airgap.proposal_status = ProposalStatus::Resolved;
    awaiting_airgap.resolution_outcome = GameStatus::DefenderWins;
    awaiting_airgap.credit = U256::from(5);
    awaiting_airgap.finalized = false;
    let client = MockClient::new(vec![claimable, zero_credit, awaiting_airgap]);
    let owned_games = OwnedGames::default();
    owned_games.insert(GAME_1);
    owned_games.insert(GAME_2);
    owned_games.insert(GAME_3);
    let manager = BondManager::new(
        BondManagerConfig::default(),
        client.clone(),
        owned_games.clone(),
    );

    // Pass 1 unlocks the credit in DelayedWETH; the bond is not paid out yet.
    manager.withdraw_credits().await.unwrap();

    assert!(client.withdrawals().is_empty());
    assert!(
        owned_games.contains(GAME_1),
        "an unlocked bond must stay owned until it is withdrawn"
    );
    assert!(!owned_games.contains(GAME_2));
    assert!(
        owned_games.contains(GAME_3),
        "a game inside the finality airgap must stay owned"
    );

    // Pass 2 drains the pending withdrawal and drops the game.
    manager.withdraw_credits().await.unwrap();

    assert_eq!(client.withdrawals(), vec![GAME_1]);
    assert!(!owned_games.contains(GAME_1));
}

#[tokio::test]
async fn bond_manager_retries_failed_claim() {
    let mut game = MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK);
    game.proposal_status = ProposalStatus::Resolved;
    game.resolution_outcome = GameStatus::ChallengerWins;
    game.resolution_reason = REASON_PROOF_TIMEOUT;
    game.credit = U256::from(10);
    let client = MockClient::new(vec![game]);
    client
        .state
        .lock()
        .expect("not poisoned")
        .fail_claim_once
        .insert(GAME_1);
    let owned_games = OwnedGames::default();
    owned_games.insert(GAME_1);
    let manager = BondManager::new(
        BondManagerConfig::default(),
        client.clone(),
        owned_games.clone(),
    );

    // Injected failure on the unlock, then unlock, then withdraw.
    for _ in 0..2 {
        manager.withdraw_credits().await.unwrap();
        assert!(owned_games.contains(GAME_1));
    }

    manager.withdraw_credits().await.unwrap();
    assert!(!owned_games.contains(GAME_1));
    assert_eq!(client.withdrawals(), vec![GAME_1]);
}

#[tokio::test]
async fn bond_manager_uses_l1_timestamp_for_delayed_withdrawal() {
    let mut game = MockGame::proposed(GAME_1, B256::ZERO, L2_BLOCK);
    game.proposal_status = ProposalStatus::Resolved;
    game.resolution_outcome = GameStatus::DefenderWins;
    game.pending = PendingWithdrawal {
        amount: U256::from(10),
        unlock_at: 100,
    };
    let client = MockClient::new(vec![game]);
    client
        .state
        .lock()
        .expect("not poisoned")
        .latest_l1_timestamp = 99;
    let owned_games = OwnedGames::default();
    owned_games.insert(GAME_1);
    let manager = BondManager::new(
        BondManagerConfig::default(),
        client.clone(),
        owned_games.clone(),
    );

    manager.withdraw_credits().await.unwrap();
    assert!(owned_games.contains(GAME_1));
    assert!(client.withdrawals().is_empty());

    client
        .state
        .lock()
        .expect("not poisoned")
        .latest_l1_timestamp = 100;
    manager.withdraw_credits().await.unwrap();
    assert!(!owned_games.contains(GAME_1));
    assert_eq!(client.withdrawals(), vec![GAME_1]);
}

#[tokio::test]
async fn tick_skips_foreign_game_types() {
    let proposed_root = B256::repeat_byte(0x10);
    let canonical_root = B256::repeat_byte(0x20);
    let client = MockClient::new(vec![MockGame::proposed(GAME_1, proposed_root, L2_BLOCK)]);
    // Index 0 belongs to another game type; the WIP-1006 game sits behind it.
    {
        let mut state = client.state.lock().expect("not poisoned");
        state.order.insert(0, Address::ZERO);
        state.foreign_indices.insert(0);
    }
    let (output_roots, _) =
        mock_output_roots(HashMap::from([(L2_BLOCK, canonical_root)]), L2_BLOCK);
    let mut challenger = WorldChainChallenger::new(config(), client.clone(), output_roots);

    challenger.tick_at(1).await.unwrap();

    assert_eq!(client.challenges(), vec![GAME_1]);
}
