use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{Address, B256, BlockNumber, U256, address, b256};
use async_trait::async_trait;
use world_chain_proofs::{
    ConsensusError, ConsensusProvider, InvalidationReason, ProposalCommitment, ResolutionStatus,
    RootState,
};

use crate::{
    BondManager, BondManagerClient, BondManagerConfig, CanonicalLine, ParentRef, Proposal,
    ProposalSubmission, ProposerClient, ProposerConfig, ProposerError, WorldChainProposer,
    types::{CloseGameSubmission, ResolveSubmission, WithdrawSubmission},
};

const DOMAIN_HASH: B256 = b256!("1111111111111111111111111111111111111111111111111111111111111111");
const ANCHOR: Address = address!("0000000000000000000000000000000000001006");
const GAME_1: Address = address!("0000000000000000000000000000000000000001");

#[derive(Debug, Clone)]
struct MockContracts {
    anchor: ParentRef,
    games: HashMap<B256, Address>,
    submissions: Arc<Mutex<Vec<Proposal>>>,
    resolution_statuses: Arc<Mutex<HashMap<Address, ResolutionStatus>>>,
    resolutions: Arc<Mutex<Vec<Address>>>,
    closures: Arc<Mutex<Vec<Address>>>,
}

#[async_trait]
impl ProposerClient for MockContracts {
    async fn anchor_parent(&self) -> Result<ParentRef, ProposerError> {
        Ok(self.anchor)
    }

    async fn proposal_key(&self, commitment: ProposalCommitment) -> Result<B256, ProposerError> {
        Ok(commitment.proposal_key(DOMAIN_HASH))
    }

    async fn game_for_proposal(
        &self,
        commitment: ProposalCommitment,
    ) -> Result<Option<Address>, ProposerError> {
        Ok(self
            .games
            .get(&commitment.proposal_key(DOMAIN_HASH))
            .copied())
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

    async fn claimable(&self, _game: Address) -> Result<U256, ProposerError> {
        Ok(U256::ZERO)
    }

    async fn withdraw(&self, _game: Address) -> Result<WithdrawSubmission, ProposerError> {
        Ok(WithdrawSubmission {
            tx_hash: B256::repeat_byte(0xdd),
            amount: U256::ZERO,
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

#[derive(Debug, Clone)]
struct MockBondClient {
    proposer: Address,
    games: Arc<Mutex<Vec<(Address, Address)>>>,
    requested_indices: Arc<Mutex<Vec<u64>>>,
    resolved_games: Arc<Mutex<HashSet<Address>>>,
    claimable: Arc<Mutex<HashMap<Address, U256>>>,
    withdrawals: Arc<Mutex<Vec<Address>>>,
    fail_game_at_once: Arc<Mutex<Option<u64>>>,
    fail_withdraw_once: Arc<Mutex<HashSet<Address>>>,
}

impl MockBondClient {
    fn new(proposer: Address, games: Vec<(Address, Address)>) -> Self {
        Self {
            proposer,
            games: Arc::new(Mutex::new(games)),
            requested_indices: Arc::default(),
            resolved_games: Arc::default(),
            claimable: Arc::default(),
            withdrawals: Arc::default(),
            fail_game_at_once: Arc::default(),
            fail_withdraw_once: Arc::default(),
        }
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

    async fn game_at(&self, index: u64) -> Result<Address, ProposerError> {
        self.requested_indices
            .lock()
            .expect("not poisoned")
            .push(index);
        let mut fail_index = self.fail_game_at_once.lock().expect("not poisoned");
        if *fail_index == Some(index) {
            *fail_index = None;
            return Err(ProposerError::Contract("injected gameAt failure".into()));
        }
        self.games
            .lock()
            .expect("not poisoned")
            .get(index as usize)
            .map(|(game, _)| *game)
            .ok_or_else(|| ProposerError::Contract(format!("missing game at index {index}")))
    }

    async fn game_proposer(&self, game: Address) -> Result<Address, ProposerError> {
        self.games
            .lock()
            .expect("not poisoned")
            .iter()
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

    async fn claimable(&self, game: Address) -> Result<U256, ProposerError> {
        Ok(self
            .claimable
            .lock()
            .expect("not poisoned")
            .get(&game)
            .copied()
            .unwrap_or_default())
    }

    async fn withdraw(&self, game: Address) -> Result<WithdrawSubmission, ProposerError> {
        if self
            .fail_withdraw_once
            .lock()
            .expect("not poisoned")
            .remove(&game)
        {
            return Err(ProposerError::Contract(
                "injected withdrawal failure".into(),
            ));
        }
        self.withdrawals.lock().expect("not poisoned").push(game);
        Ok(WithdrawSubmission {
            tx_hash: B256::repeat_byte(0xdd),
            amount: self
                .claimable
                .lock()
                .expect("not poisoned")
                .get(&game)
                .copied()
                .unwrap_or_default(),
        })
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

fn proposal_key(parent_ref: Address, root_claim: B256, l2_block_number: u64) -> B256 {
    ProposalCommitment {
        parent_ref,
        root_claim,
        l2_block_number,
    }
    .proposal_key(DOMAIN_HASH)
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
    let mut games = HashMap::new();
    games.insert(proposal_key(ANCHOR, root_10, 10), GAME_1);

    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games,
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10), (20, root_20)]),
        finalized_l2_block: 20,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.canonical_line().games(),
        &[ParentRef {
            address: GAME_1,
            l2_block_number: 10,
        }]
    );
}

#[tokio::test]
async fn propose_submits_proposal_after_last_canonical_game() {
    let submissions = Arc::default();
    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::clone(&submissions),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 10,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);
    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    proposer.propose(&canonical_scan).await.unwrap();

    let proposal = submissions.lock().expect("not poisoned")[0];
    assert_eq!(proposal.parent_ref, ANCHOR);
    assert_eq!(proposal.root_claim, B256::repeat_byte(0x10));
    assert_eq!(proposal.l2_block_number, 10);
    assert_eq!(
        proposal.proposal_key,
        proposal_key(ANCHOR, B256::repeat_byte(0x10), 10)
    );
}

#[tokio::test]
async fn zero_block_interval_is_rejected() {
    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
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
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 9,
    };
    let proposer = WorldChainProposer::new(config(), contracts, output_roots);

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert!(canonical_scan.canonical_line().games().is_empty());
    assert_eq!(
        canonical_scan.canonical_line().anchor(),
        ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
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
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([
            (GAME_1, positive_ready_status()),
            (game_2, positive_ready_status()),
            (
                game_3,
                ResolutionStatus {
                    resolvable: false,
                    root_state: RootState::Finalized,
                    invalidation_reason: InvalidationReason::None,
                },
            ),
        ]))),
        resolutions: Arc::clone(&resolutions),
        closures: Arc::clone(&closures),
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
    let mut canonical_line = CanonicalLine::new(ParentRef {
        address: ANCHOR,
        l2_block_number: 0,
    });
    canonical_line.push_game(ParentRef {
        address: GAME_1,
        l2_block_number: 10,
    });
    canonical_line.push_game(ParentRef {
        address: game_2,
        l2_block_number: 20,
    });
    canonical_line.push_game(ParentRef {
        address: game_3,
        l2_block_number: 30,
    });

    let finalized_games = proposer.resolve_games(&canonical_line).await.unwrap();
    assert_eq!(
        *resolutions.lock().expect("not poisoned"),
        vec![GAME_1, game_2]
    );
    assert_eq!(
        finalized_games.last(),
        Some(ParentRef {
            address: game_3,
            l2_block_number: 30,
        })
    );

    proposer.advance_anchor(finalized_games).await.unwrap();
    assert_eq!(*closures.lock().expect("not poisoned"), vec![game_3]);
}

#[tokio::test]
async fn finalized_games_do_not_consume_resolution_budget() {
    let game_2 = game_address(2);
    let game_3 = game_address(3);
    let resolutions = Arc::default();
    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([
            (
                GAME_1,
                ResolutionStatus {
                    resolvable: false,
                    root_state: RootState::Finalized,
                    invalidation_reason: InvalidationReason::None,
                },
            ),
            (game_2, positive_ready_status()),
            (game_3, positive_ready_status()),
        ]))),
        resolutions: Arc::clone(&resolutions),
        closures: Arc::default(),
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
    );
    let mut canonical_line = CanonicalLine::new(ParentRef {
        address: ANCHOR,
        l2_block_number: 0,
    });
    for (address, l2_block_number) in [(GAME_1, 10), (game_2, 20), (game_3, 30)] {
        canonical_line.push_game(ParentRef {
            address,
            l2_block_number,
        });
    }

    let finalized_games = proposer.resolve_games(&canonical_line).await.unwrap();

    assert_eq!(*resolutions.lock().expect("not poisoned"), vec![game_2]);
    assert_eq!(
        finalized_games.last(),
        Some(ParentRef {
            address: game_2,
            l2_block_number: 20,
        })
    );
}

#[tokio::test]
async fn zero_resolution_budget_is_rejected() {
    let contracts = MockContracts {
        anchor: ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        },
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
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
            (
                game_address(index + 1),
                if index == 1_003 {
                    other_proposer
                } else {
                    proposer
                },
            )
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
        .push((game_address(1_006), proposer));
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
        .map(|index| (game_address(index), proposer))
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
async fn bond_manager_prunes_resolved_games_and_retries_failed_withdrawals() {
    let proposer = Address::repeat_byte(0xa1);
    let unresolved = game_address(1);
    let zero_credit = game_address(2);
    let withdrawable = game_address(3);
    let retry_withdrawal = game_address(4);
    let games = vec![
        (unresolved, proposer),
        (zero_credit, proposer),
        (withdrawable, proposer),
        (retry_withdrawal, proposer),
    ];
    let client = MockBondClient::new(proposer, games);
    client.resolved_games.lock().expect("not poisoned").extend([
        zero_credit,
        withdrawable,
        retry_withdrawal,
    ]);
    client.claimable.lock().expect("not poisoned").extend([
        (withdrawable, U256::from(10)),
        (retry_withdrawal, U256::from(20)),
    ]);
    client
        .fail_withdraw_once
        .lock()
        .expect("not poisoned")
        .insert(retry_withdrawal);
    let mut manager = BondManager::new(bond_manager_config(100), client.clone());
    manager.scan_games().await.unwrap();

    manager.withdraw_credits().await.unwrap();

    assert!(manager.tracks_game(unresolved));
    assert!(!manager.tracks_game(zero_credit));
    assert!(!manager.tracks_game(withdrawable));
    assert!(manager.tracks_game(retry_withdrawal));
    assert_eq!(
        *client.withdrawals.lock().expect("not poisoned"),
        vec![withdrawable]
    );

    manager.withdraw_credits().await.unwrap();

    assert!(!manager.tracks_game(retry_withdrawal));
    let withdrawals = client.withdrawals.lock().expect("not poisoned");
    assert!(withdrawals.contains(&withdrawable));
    assert!(withdrawals.contains(&retry_withdrawal));
}
