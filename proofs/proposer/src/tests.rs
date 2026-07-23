use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{Address, B256, BlockNumber, U256, address};
use async_trait::async_trait;
use world_chain_proofs::{
    ANCHOR_PARENT_INDEX, ConsensusError, ConsensusProvider, InvalidationReason, ResolutionStatus,
    RootState, extra_data,
};

use crate::{
    BondManager, BondManagerClient, BondManagerConfig, CanonicalLine, ClaimOutcome,
    CloseGameSubmission, FinalizedGames, NextProposalAction, ParentRef, Proposal,
    ProposalSubmission, ProposerClient, ProposerConfig, ProposerError, ResolveSubmission,
    WorldChainProposer,
};

const ANCHOR: Address = address!("0000000000000000000000000000000000001006");
const GAME_1: Address = address!("0000000000000000000000000000000000000001");

/// Programmable fake for [`ProposerClient`].
///
/// Games are keyed by their on-chain factory identity `(root_claim, extraData)` so that
/// `find_game` distinguishes parent index and retry attempt exactly like the real factory does.
#[derive(Debug, Default)]
struct MockContracts {
    anchor_parents: Vec<ParentRef>,
    games: HashMap<(B256, Vec<u8>), Address>,
    game_indices: HashMap<Address, U256>,
    resolution_statuses: Arc<Mutex<HashMap<Address, ResolutionStatus>>>,
    finalized: HashSet<Address>,
    submissions: Arc<Mutex<Vec<Proposal>>>,
    resolutions: Arc<Mutex<Vec<Address>>>,
    closures: Arc<Mutex<Vec<Address>>>,
    find_game_probes: Arc<Mutex<Vec<Proposal>>>,
}

#[async_trait]
impl ProposerClient for MockContracts {
    async fn anchor_parents(&self) -> Result<Vec<ParentRef>, ProposerError> {
        Ok(self.anchor_parents.clone())
    }

    async fn find_game(&self, proposal: &Proposal) -> Result<Option<Address>, ProposerError> {
        self.find_game_probes
            .lock()
            .expect("not poisoned")
            .push(*proposal);
        Ok(self
            .games
            .get(&(proposal.root_claim, proposal.extra_data()))
            .copied())
    }

    async fn game_index(&self, game: Address) -> Result<U256, ProposerError> {
        self.game_indices
            .get(&game)
            .copied()
            .ok_or_else(|| ProposerError::Contract(format!("no factory index for game {game}")))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        Ok(self
            .resolution_statuses
            .lock()
            .expect("not poisoned")
            .remove(&game)
            .unwrap_or(ResolutionStatus {
                resolvable: false,
                root_state: RootState::Proposed,
                invalidation_reason: InvalidationReason::None,
            }))
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        Ok(self.finalized.contains(&game))
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        self.resolutions.lock().expect("not poisoned").push(game);
        Ok(ResolveSubmission {
            tx_hash: B256::repeat_byte(0xbb),
        })
    }

    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        self.closures.lock().expect("not poisoned").push(game);
        Ok(CloseGameSubmission {
            tx_hash: B256::repeat_byte(0xcc),
        })
    }

    async fn proposer_bond(&self) -> Result<U256, ProposerError> {
        Ok(U256::from(1))
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        _proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError> {
        self.submissions
            .lock()
            .expect("not poisoned")
            .push(*proposal);
        Ok(ProposalSubmission {
            tx_hash: B256::repeat_byte(0xaa),
            game_address: Address::repeat_byte(0xaa),
        })
    }
}

/// One programmed outcome of a `claim_credits` call, including an injectable failure.
#[derive(Debug, Clone, Copy)]
enum ClaimStep {
    Fail,
    Outcome(ClaimOutcome),
}

/// Programmable fake for [`BondManagerClient`].
#[derive(Debug, Clone)]
struct MockBondClient {
    proposer: Address,
    // `None` entries model games of a different game type, which the scanner must skip.
    games: Arc<Mutex<Vec<Option<(Address, Address)>>>>,
    requested_indices: Arc<Mutex<Vec<u64>>>,
    resolved_games: Arc<Mutex<HashSet<Address>>>,
    claim_steps: Arc<Mutex<HashMap<Address, VecDeque<ClaimStep>>>>,
    claim_calls: Arc<Mutex<Vec<Address>>>,
    fail_game_at_once: Arc<Mutex<Option<u64>>>,
}

impl MockBondClient {
    fn new(proposer: Address, games: Vec<Option<(Address, Address)>>) -> Self {
        Self {
            proposer,
            games: Arc::new(Mutex::new(games)),
            requested_indices: Arc::default(),
            resolved_games: Arc::default(),
            claim_steps: Arc::default(),
            claim_calls: Arc::default(),
            fail_game_at_once: Arc::default(),
        }
    }

    fn set_resolved(&self, games: impl IntoIterator<Item = Address>) {
        self.resolved_games
            .lock()
            .expect("not poisoned")
            .extend(games);
    }

    fn program_claims(&self, game: Address, steps: impl IntoIterator<Item = ClaimStep>) {
        self.claim_steps
            .lock()
            .expect("not poisoned")
            .insert(game, VecDeque::from_iter(steps));
    }
}

#[async_trait]
impl BondManagerClient for MockBondClient {
    fn proposer_address(&self) -> Address {
        self.proposer
    }

    async fn game_count(&self) -> Result<u64, ProposerError> {
        Ok(self.games.lock().expect("not poisoned").len() as u64)
    }

    async fn game_at(&self, index: u64) -> Result<Option<Address>, ProposerError> {
        self.requested_indices
            .lock()
            .expect("not poisoned")
            .push(index);
        {
            let mut fail_index = self.fail_game_at_once.lock().expect("not poisoned");
            if *fail_index == Some(index) {
                *fail_index = None;
                return Err(ProposerError::Contract("injected gameAt failure".into()));
            }
        }
        let games = self.games.lock().expect("not poisoned");
        let entry = games
            .get(index as usize)
            .copied()
            .ok_or_else(|| ProposerError::Contract(format!("missing game at index {index}")))?;
        Ok(entry.map(|(game, _)| game))
    }

    async fn game_proposer(&self, game: Address) -> Result<Address, ProposerError> {
        self.games
            .lock()
            .expect("not poisoned")
            .iter()
            .flatten()
            .find_map(|(candidate, proposer)| (*candidate == game).then_some(*proposer))
            .ok_or_else(|| ProposerError::Contract(format!("unknown game {game}")))
    }

    async fn resolution_status(&self, game: Address) -> Result<ResolutionStatus, ProposerError> {
        let resolved = self
            .resolved_games
            .lock()
            .expect("not poisoned")
            .contains(&game);
        Ok(ResolutionStatus {
            resolvable: false,
            root_state: if resolved {
                RootState::Finalized
            } else {
                RootState::Proposed
            },
            invalidation_reason: InvalidationReason::None,
        })
    }

    async fn claim_credits(&self, game: Address) -> Result<ClaimOutcome, ProposerError> {
        self.claim_calls.lock().expect("not poisoned").push(game);
        let step = self
            .claim_steps
            .lock()
            .expect("not poisoned")
            .get_mut(&game)
            .and_then(VecDeque::pop_front);
        match step {
            Some(ClaimStep::Fail) => Err(ProposerError::Contract("injected claim failure".into())),
            Some(ClaimStep::Outcome(outcome)) => Ok(outcome),
            None => Ok(ClaimOutcome::NotReady),
        }
    }
}

#[derive(Debug, Clone)]
struct MockOutputRoots {
    roots: HashMap<u64, B256>,
    finalized_l2_block: BlockNumber,
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
        Ok(self.finalized_l2_block)
    }
}

fn config() -> ProposerConfig {
    ProposerConfig {
        block_interval: 10,
        proposer_bond: U256::from(1),
        poll_interval: Duration::from_secs(1),
        max_resolutions_per_tick: 1,
    }
}

fn positive_ready_status() -> ResolutionStatus {
    ResolutionStatus {
        resolvable: true,
        root_state: RootState::Finalized,
        invalidation_reason: InvalidationReason::None,
    }
}

fn finalized_status() -> ResolutionStatus {
    ResolutionStatus {
        resolvable: false,
        root_state: RootState::Finalized,
        invalidation_reason: InvalidationReason::None,
    }
}

fn timed_out_status() -> ResolutionStatus {
    ResolutionStatus {
        resolvable: false,
        root_state: RootState::Invalidated,
        invalidation_reason: InvalidationReason::ProofTimeout,
    }
}

fn invalidated_status(reason: InvalidationReason) -> ResolutionStatus {
    ResolutionStatus {
        resolvable: false,
        root_state: RootState::Invalidated,
        invalidation_reason: reason,
    }
}

/// The anchor sentinel: a parent addressed by [`ANCHOR_PARENT_INDEX`].
fn sentinel(address: Address, l2_block_number: u64) -> ParentRef {
    ParentRef {
        address,
        l2_block_number,
        parent_index: ANCHOR_PARENT_INDEX,
    }
}

/// A parent addressed by its factory creation index.
fn indexed(address: Address, l2_block_number: u64, parent_index: u64) -> ParentRef {
    ParentRef {
        address,
        l2_block_number,
        parent_index: U256::from(parent_index),
    }
}

fn proposal(
    parent_index: U256,
    parent_ref: Address,
    root_claim: B256,
    l2_block_number: u64,
    attempt: u64,
) -> Proposal {
    Proposal {
        parent_index,
        parent_ref,
        root_claim,
        l2_block_number,
        attempt: U256::from(attempt),
    }
}

/// Builds the on-chain factory identity key used by [`MockContracts::find_game`].
fn game_key(
    root_claim: B256,
    l2_block_number: u64,
    parent_index: U256,
    attempt: u64,
) -> (B256, Vec<u8>) {
    (
        root_claim,
        extra_data(l2_block_number, parent_index, U256::from(attempt)),
    )
}

fn game_address(index: u64) -> Address {
    let mut bytes = [0_u8; 20];
    bytes[12..].copy_from_slice(&index.to_be_bytes());
    Address::from(bytes)
}

fn bond_manager_config(initial_scan_limit: u64) -> BondManagerConfig {
    BondManagerConfig {
        poll_interval: Duration::from_secs(1),
        initial_scan_limit,
    }
}

#[tokio::test]
async fn anchor_and_canonical_line_walks_existing_games_until_gap() {
    let root_10 = B256::repeat_byte(0x10);
    let root_20 = B256::repeat_byte(0x20);
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        games: HashMap::from([(game_key(root_10, 10, ANCHOR_PARENT_INDEX, 0), GAME_1)]),
        game_indices: HashMap::from([(GAME_1, U256::ZERO)]),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10), (20, root_20)]),
        finalized_l2_block: 20,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.canonical_line().games(),
        &[indexed(GAME_1, 10, 0)]
    );
}

#[tokio::test]
async fn anchor_and_canonical_line_finds_child_under_anchor_game_index() {
    // The anchor advanced to block 10 (anchor game `AG` at factory index 5). A child game `C`
    // was created before the advance and therefore references its parent by index, not by the
    // sentinel. The dual-candidate lookup must still find it.
    let anchor_game = game_address(50);
    let child = game_address(51);
    let root_20 = B256::repeat_byte(0x20);
    let contracts = MockContracts {
        anchor_parents: vec![indexed(anchor_game, 10, 5), sentinel(ANCHOR, 10)],
        games: HashMap::from([(game_key(root_20, 20, U256::from(5), 0), child)]),
        game_indices: HashMap::from([(child, U256::from(7))]),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(20, root_20)]),
        finalized_l2_block: 20,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.canonical_line().games(),
        &[indexed(child, 20, 7)]
    );
    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::CaughtUp {
            target_block: 30,
            finalized_block: 20,
        }
    );
}

#[tokio::test]
async fn propose_at_anchor_uses_sentinel_parent_index() {
    let root_10 = B256::repeat_byte(0x10);
    let submissions = Arc::default();
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        submissions: Arc::clone(&submissions),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10)]),
        finalized_l2_block: 10,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);
    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::Propose(proposal(ANCHOR_PARENT_INDEX, ANCHOR, root_10, 10, 0))
    );

    proposer.propose(&canonical_scan).await.unwrap();

    let submitted = submissions.lock().expect("not poisoned")[0];
    assert_eq!(submitted.parent_index, ANCHOR_PARENT_INDEX);
    assert_eq!(submitted.parent_ref, ANCHOR);
    assert_eq!(submitted.root_claim, root_10);
    assert_eq!(submitted.l2_block_number, 10);
    assert_eq!(submitted.attempt, U256::ZERO);
}

#[tokio::test]
async fn zero_block_interval_is_rejected() {
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        ..Default::default()
    };
    let proposer = WorldChainProposer::new(
        ProposerConfig {
            block_interval: 0,
            ..config()
        },
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
    );

    assert!(matches!(
        proposer.anchor_and_canonical_line().await,
        Err(ProposerError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn anchor_and_canonical_line_stops_at_finalized_l2_block() {
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::new(),
        finalized_l2_block: 9,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert!(canonical_scan.canonical_line().games().is_empty());
    assert_eq!(
        canonical_scan.canonical_line().anchor(),
        sentinel(ANCHOR, 0)
    );
    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::CaughtUp {
            target_block: 10,
            finalized_block: 9,
        }
    );
}

#[tokio::test]
async fn retry_timed_out_extends_lineage_above_anchor() {
    let root_10 = B256::repeat_byte(0x10);
    let root_20 = B256::repeat_byte(0x20);
    let timed_out = game_address(2);
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        games: HashMap::from([
            (game_key(root_10, 10, ANCHOR_PARENT_INDEX, 0), GAME_1),
            (game_key(root_20, 20, U256::from(3), 0), timed_out),
        ]),
        game_indices: HashMap::from([(GAME_1, U256::from(3))]),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([(timed_out, timed_out_status())]))),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10), (20, root_20)]),
        finalized_l2_block: 20,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.canonical_line().games(),
        &[indexed(GAME_1, 10, 3)]
    );
    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::RetryTimedOut {
            proposal: proposal(U256::from(3), GAME_1, root_20, 20, 1),
            invalidated_game: timed_out,
        }
    );
}

#[tokio::test]
async fn retry_uses_highest_attempt_after_probing_gaps() {
    let root_10 = B256::repeat_byte(0x10);
    let attempt_0 = game_address(101);
    let attempt_1 = game_address(102);
    let attempt_2 = game_address(103);
    let probes = Arc::default();
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        games: HashMap::from([
            (game_key(root_10, 10, ANCHOR_PARENT_INDEX, 0), attempt_0),
            (game_key(root_10, 10, ANCHOR_PARENT_INDEX, 1), attempt_1),
            (game_key(root_10, 10, ANCHOR_PARENT_INDEX, 2), attempt_2),
        ]),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([(attempt_2, timed_out_status())]))),
        find_game_probes: Arc::clone(&probes),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10)]),
        finalized_l2_block: 10,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    // The probe walks attempts 0, 1, 2 and stops at the first gap (attempt 3).
    let probed_attempts: Vec<U256> = probes
        .lock()
        .expect("not poisoned")
        .iter()
        .map(|probe| probe.attempt)
        .collect();
    assert_eq!(
        probed_attempts,
        vec![U256::ZERO, U256::from(1), U256::from(2), U256::from(3)]
    );
    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::RetryTimedOut {
            proposal: proposal(ANCHOR_PARENT_INDEX, ANCHOR, root_10, 10, 3),
            invalidated_game: attempt_2,
        }
    );
}

#[tokio::test]
async fn rebase_onto_sentinel_when_anchor_game_timed_out() {
    // The timed-out game references its parent by index, but that parent has since become the
    // anchor, so its lineage can no longer be extended; the retry rebases onto the sentinel.
    let anchor_game = game_address(50);
    let timed_out = game_address(51);
    let root_20 = B256::repeat_byte(0x20);
    let contracts = MockContracts {
        anchor_parents: vec![indexed(anchor_game, 10, 5), sentinel(ANCHOR, 10)],
        games: HashMap::from([(game_key(root_20, 20, U256::from(5), 0), timed_out)]),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([(timed_out, timed_out_status())]))),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(20, root_20)]),
        finalized_l2_block: 20,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert!(canonical_scan.canonical_line().games().is_empty());
    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::Propose(proposal(ANCHOR_PARENT_INDEX, ANCHOR, root_20, 20, 0))
    );
}

#[tokio::test]
async fn rebase_continues_sentinel_attempt_chain() {
    // Same rebase, but a sentinel-lineage game already exists for this transition and is also
    // timed out; the retry continues that lineage's attempt chain instead of starting over.
    let anchor_game = game_address(50);
    let indexed_timed_out = game_address(51);
    let sentinel_timed_out = game_address(52);
    let root_20 = B256::repeat_byte(0x20);
    let contracts = MockContracts {
        anchor_parents: vec![indexed(anchor_game, 10, 5), sentinel(ANCHOR, 10)],
        games: HashMap::from([
            (game_key(root_20, 20, U256::from(5), 0), indexed_timed_out),
            (
                game_key(root_20, 20, ANCHOR_PARENT_INDEX, 0),
                sentinel_timed_out,
            ),
        ]),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([
            (indexed_timed_out, timed_out_status()),
            (sentinel_timed_out, timed_out_status()),
        ]))),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(20, root_20)]),
        finalized_l2_block: 20,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::RetryTimedOut {
            proposal: proposal(ANCHOR_PARENT_INDEX, ANCHOR, root_20, 20, 1),
            invalidated_game: indexed_timed_out,
        }
    );
}

#[tokio::test]
async fn awaits_negative_resolution_for_resolvable_invalidated_game() {
    let root_10 = B256::repeat_byte(0x10);
    let game = game_address(9);
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        games: HashMap::from([(game_key(root_10, 10, ANCHOR_PARENT_INDEX, 0), game)]),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([(
            game,
            ResolutionStatus {
                resolvable: true,
                root_state: RootState::Invalidated,
                invalidation_reason: InvalidationReason::InvalidParent,
            },
        )]))),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10)]),
        finalized_l2_block: 10,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::AwaitNegativeResolution {
            game,
            reason: InvalidationReason::InvalidParent,
        }
    );
}

#[tokio::test]
async fn blocks_on_non_timeout_invalidation() {
    let root_10 = B256::repeat_byte(0x10);
    let game = game_address(9);
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        games: HashMap::from([(game_key(root_10, 10, ANCHOR_PARENT_INDEX, 0), game)]),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([(
            game,
            invalidated_status(InvalidationReason::InvalidParent),
        )]))),
        ..Default::default()
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10)]),
        finalized_l2_block: 10,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::BlockedByInvalidation {
            game,
            reason: InvalidationReason::InvalidParent,
        }
    );
}

#[tokio::test]
async fn resolve_games_caps_submissions_and_keeps_scanning_finalized_games() {
    let game_2 = game_address(2);
    let game_3 = game_address(3);
    let resolutions = Arc::default();
    let closures = Arc::default();
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([
            (GAME_1, positive_ready_status()),
            (game_2, positive_ready_status()),
            (game_3, finalized_status()),
        ]))),
        finalized: HashSet::from([game_3]),
        resolutions: Arc::clone(&resolutions),
        closures: Arc::clone(&closures),
        ..Default::default()
    };
    let proposer = WorldChainProposer::new(
        ProposerConfig {
            max_resolutions_per_tick: 2,
            ..config()
        },
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
    );
    let mut canonical_line = CanonicalLine::new(sentinel(ANCHOR, 0));
    canonical_line.push_game(indexed(GAME_1, 10, 0));
    canonical_line.push_game(indexed(game_2, 20, 0));
    canonical_line.push_game(indexed(game_3, 30, 0));

    let finalized_games = proposer.resolve_games(&canonical_line).await.unwrap();
    assert_eq!(
        *resolutions.lock().expect("not poisoned"),
        vec![GAME_1, game_2]
    );
    assert_eq!(finalized_games.last(), Some(indexed(game_3, 30, 0)));

    proposer.advance_anchor(finalized_games).await.unwrap();
    assert_eq!(*closures.lock().expect("not poisoned"), vec![game_3]);
}

#[tokio::test]
async fn finalized_games_do_not_consume_resolution_budget() {
    let game_2 = game_address(2);
    let game_3 = game_address(3);
    let resolutions = Arc::default();
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([
            (GAME_1, finalized_status()),
            (game_2, positive_ready_status()),
            (game_3, positive_ready_status()),
        ]))),
        resolutions: Arc::clone(&resolutions),
        ..Default::default()
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
    );
    let mut canonical_line = CanonicalLine::new(sentinel(ANCHOR, 0));
    for (address, l2_block_number) in [(GAME_1, 10), (game_2, 20), (game_3, 30)] {
        canonical_line.push_game(indexed(address, l2_block_number, 0));
    }

    let finalized_games = proposer.resolve_games(&canonical_line).await.unwrap();

    assert_eq!(*resolutions.lock().expect("not poisoned"), vec![game_2]);
    assert_eq!(finalized_games.last(), Some(indexed(game_2, 20, 0)));
}

#[tokio::test]
async fn advance_anchor_defers_close_inside_finality_airgap() {
    let game = game_address(3);
    let closures = Arc::default();
    let contracts = MockContracts {
        // `finalized` intentionally left empty: the game is resolved but still inside the airgap.
        closures: Arc::clone(&closures),
        ..Default::default()
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
    );
    let mut finalized_games = FinalizedGames::default();
    finalized_games.push(indexed(game, 30, 0));

    proposer.advance_anchor(finalized_games).await.unwrap();

    assert!(closures.lock().expect("not poisoned").is_empty());
}

#[tokio::test]
async fn zero_resolution_budget_is_rejected() {
    let contracts = MockContracts {
        anchor_parents: vec![sentinel(ANCHOR, 0)],
        ..Default::default()
    };
    let proposer = WorldChainProposer::new(
        ProposerConfig {
            max_resolutions_per_tick: 0,
            ..config()
        },
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
    );

    assert!(matches!(
        proposer.anchor_and_canonical_line().await,
        Err(ProposerError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn bond_manager_scans_bounded_initial_window_then_only_new_games() {
    let proposer = Address::repeat_byte(0xa1);
    let other_proposer = Address::repeat_byte(0xb2);
    let games: Vec<_> = (0..1_005)
        .map(|index| {
            Some((
                game_address(index + 1),
                if index == 1_003 {
                    other_proposer
                } else {
                    proposer
                },
            ))
        })
        .collect();
    let client = MockBondClient::new(proposer, games);
    let mut manager = BondManager::new(bond_manager_config(1_000), client.clone());

    manager.scan_games().await.unwrap();

    {
        let requested = client.requested_indices.lock().expect("not poisoned");
        assert_eq!(requested.len(), 1_000);
        assert_eq!(requested.first(), Some(&5));
        assert_eq!(requested.last(), Some(&1_004));
    }
    assert_eq!(manager.next_game_index(), Some(1_005));
    assert!(manager.tracks_game(game_address(6)));
    assert!(!manager.tracks_game(game_address(1_004)));

    client
        .games
        .lock()
        .expect("not poisoned")
        .push(Some((game_address(1_006), proposer)));
    client
        .requested_indices
        .lock()
        .expect("not poisoned")
        .clear();

    manager.scan_games().await.unwrap();

    assert_eq!(
        *client.requested_indices.lock().expect("not poisoned"),
        vec![1_005]
    );
    assert_eq!(manager.next_game_index(), Some(1_006));
    assert!(manager.tracks_game(game_address(1_006)));
}

#[tokio::test]
async fn bond_manager_retries_complete_range_after_partial_scan_failure() {
    let proposer = Address::repeat_byte(0xa1);
    let games: Vec<_> = (1..=3)
        .map(|index| Some((game_address(index), proposer)))
        .collect();
    let client = MockBondClient::new(proposer, games);
    *client.fail_game_at_once.lock().expect("not poisoned") = Some(1);
    let mut manager = BondManager::new(bond_manager_config(100), client.clone());

    assert!(manager.scan_games().await.is_err());
    assert_eq!(manager.next_game_index(), None);
    assert!(manager.tracks_game(game_address(1)));

    manager.scan_games().await.unwrap();

    assert_eq!(manager.next_game_index(), Some(3));
    assert!(manager.tracks_game(game_address(1)));
    assert!(manager.tracks_game(game_address(2)));
    assert!(manager.tracks_game(game_address(3)));
    assert_eq!(
        *client.requested_indices.lock().expect("not poisoned"),
        vec![0, 1, 0, 1, 2]
    );
}

#[tokio::test]
async fn bond_manager_skips_other_game_types() {
    let proposer = Address::repeat_byte(0xa1);
    let ours_low = game_address(1);
    let other_type = game_address(2);
    let ours_high = game_address(3);
    let client = MockBondClient::new(
        proposer,
        vec![
            Some((ours_low, proposer)),
            None,
            Some((ours_high, proposer)),
        ],
    );
    let mut manager = BondManager::new(bond_manager_config(100), client.clone());

    manager.scan_games().await.unwrap();

    assert_eq!(
        *client.requested_indices.lock().expect("not poisoned"),
        vec![0, 1, 2]
    );
    assert_eq!(manager.next_game_index(), Some(3));
    assert!(manager.tracks_game(ours_low));
    assert!(manager.tracks_game(ours_high));
    assert!(!manager.tracks_game(other_type));
}

#[tokio::test]
async fn bond_manager_two_phase_claim_progression() {
    let proposer = Address::repeat_byte(0xa1);
    let game = game_address(1);
    let client = MockBondClient::new(proposer, vec![Some((game, proposer))]);
    client.set_resolved([game]);
    client.program_claims(
        game,
        [
            ClaimStep::Outcome(ClaimOutcome::NotReady),
            ClaimStep::Outcome(ClaimOutcome::Unlocked {
                tx_hash: B256::repeat_byte(0x01),
                amount: U256::from(10),
            }),
            ClaimStep::Outcome(ClaimOutcome::Claimed {
                tx_hash: B256::repeat_byte(0x02),
                amount: U256::from(10),
            }),
        ],
    );
    let mut manager = BondManager::new(bond_manager_config(100), client.clone());
    manager.scan_games().await.unwrap();

    // Phase 0: not yet claimable.
    manager.withdraw_credits().await.unwrap();
    assert!(manager.tracks_game(game));

    // Phase 1: credit unlocked, still tracked until the withdrawal delay elapses.
    manager.withdraw_credits().await.unwrap();
    assert!(manager.tracks_game(game));

    // Phase 2: funds withdrawn, game pruned.
    manager.withdraw_credits().await.unwrap();
    assert!(!manager.tracks_game(game));

    assert_eq!(
        *client.claim_calls.lock().expect("not poisoned"),
        vec![game, game, game]
    );
}

#[tokio::test]
async fn bond_manager_prunes_settled_games_and_retries_failed_claims() {
    let proposer = Address::repeat_byte(0xa1);
    let unresolved = game_address(1);
    let no_credit = game_address(2);
    let claimed = game_address(3);
    let retry = game_address(4);
    let client = MockBondClient::new(
        proposer,
        vec![
            Some((unresolved, proposer)),
            Some((no_credit, proposer)),
            Some((claimed, proposer)),
            Some((retry, proposer)),
        ],
    );
    client.set_resolved([no_credit, claimed, retry]);
    client.program_claims(no_credit, [ClaimStep::Outcome(ClaimOutcome::NoCredit)]);
    client.program_claims(
        claimed,
        [ClaimStep::Outcome(ClaimOutcome::Claimed {
            tx_hash: B256::repeat_byte(0x03),
            amount: U256::from(10),
        })],
    );
    client.program_claims(
        retry,
        [
            ClaimStep::Fail,
            ClaimStep::Outcome(ClaimOutcome::Claimed {
                tx_hash: B256::repeat_byte(0x04),
                amount: U256::from(20),
            }),
        ],
    );
    let mut manager = BondManager::new(bond_manager_config(100), client.clone());
    manager.scan_games().await.unwrap();

    manager.withdraw_credits().await.unwrap();

    // Unresolved games are never claimed; settled games are pruned; the failed claim is kept.
    assert!(manager.tracks_game(unresolved));
    assert!(!manager.tracks_game(no_credit));
    assert!(!manager.tracks_game(claimed));
    assert!(manager.tracks_game(retry));
    {
        let calls = client.claim_calls.lock().expect("not poisoned");
        assert!(!calls.contains(&unresolved));
        assert!(calls.contains(&no_credit));
        assert!(calls.contains(&claimed));
        assert!(calls.contains(&retry));
    }

    manager.withdraw_credits().await.unwrap();

    assert!(!manager.tracks_game(retry));
    assert!(manager.tracks_game(unresolved));
}
