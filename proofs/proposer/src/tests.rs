use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use alloy_primitives::{Address, B256, BlockNumber, Bytes, U256, address, b256};
use async_trait::async_trait;
use world_chain_proofs::{
    ConsensusError, ConsensusProvider, InvalidationReason, ProposalCommitment, ResolutionStatus,
    RootState,
};
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequestError, ProofRequestId, ProofRequester,
    ProofResponse, ProofStatus, RequestProofResponse, SucceededProofResponse,
};

use crate::{
    AnchorRef, BondManager, BondManagerClient, BondManagerConfig, CanonicalLine, ParentRef,
    Proposal, ProposalSubmission, ProposerClient, ProposerConfig, ProposerError,
    WorldChainProposer,
    types::{
        CanonicalScan, ClaimSubmission, CloseGameSubmission, NextProposalAction, PendingWithdrawal,
        ResolveSubmission, TransitionGame,
    },
};

const DOMAIN_HASH: B256 = b256!("1111111111111111111111111111111111111111111111111111111111111111");
const ANCHOR: Address = address!("0000000000000000000000000000000000001006");
const GAME_1: Address = address!("0000000000000000000000000000000000000001");

/// Mirrors [`crate::alloy`]'s attempt walk so tests exercise the same lookup shape.
const MAX_TEST_ATTEMPT: u64 = 4;

#[derive(Debug, Clone)]
struct MockContracts {
    anchor: AnchorRef,
    /// Games keyed by their `DisputeGameFactory` UUID.
    games: HashMap<B256, Address>,
    submissions: Arc<Mutex<Vec<(Proposal, ProofData)>>>,
    resolution_statuses: Arc<Mutex<HashMap<Address, ResolutionStatus>>>,
    resolutions: Arc<Mutex<Vec<Address>>>,
    closures: Arc<Mutex<Vec<Address>>>,
    submission_failures: Arc<Mutex<usize>>,
    /// Games whose registry finality airgap has not elapsed yet.
    unfinalized_games: Arc<Mutex<HashSet<Address>>>,
}

#[async_trait]
impl ProposerClient for MockContracts {
    async fn anchor_parent(&self) -> Result<AnchorRef, ProposerError> {
        Ok(self.anchor)
    }

    async fn games_for_transition(
        &self,
        parent_candidates: &[Address],
        root_claim: B256,
        l2_block_number: u64,
    ) -> Result<Vec<TransitionGame>, ProposerError> {
        let mut found = Vec::with_capacity(parent_candidates.len());
        for parent_ref in parent_candidates {
            let mut latest = None;
            for attempt in 0..MAX_TEST_ATTEMPT {
                let uuid = game_uuid(*parent_ref, root_claim, l2_block_number, attempt);
                let Some(address) = self.games.get(&uuid).copied() else {
                    break;
                };
                latest = Some(TransitionGame {
                    address,
                    parent_ref: *parent_ref,
                    attempt,
                });
            }
            found.extend(latest);
        }
        Ok(found)
    }

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        Ok(!self
            .unfinalized_games
            .lock()
            .expect("not poisoned")
            .contains(&game))
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

    async fn latest_finalized_l1_block(&self) -> Result<B256, ProposerError> {
        Ok(B256::repeat_byte(0xf1))
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        proof: ProofData,
    ) -> Result<ProposalSubmission, ProposerError> {
        let mut failures = self.submission_failures.lock().expect("not poisoned");
        if *failures > 0 {
            *failures -= 1;
            return Err(ProposerError::Contract(
                "injected proposal submission failure".into(),
            ));
        }
        drop(failures);
        self.submissions
            .lock()
            .expect("not poisoned")
            .push((*proposal, proof));
        Ok(ProposalSubmission {
            tx_hash: B256::repeat_byte(0xaa),
            game_address: Address::repeat_byte(0xaa),
        })
    }
}

#[derive(Debug, Clone)]
struct MockProofRequester {
    requests: Arc<Mutex<Vec<ProofRequest>>>,
    status: Arc<Mutex<ProofStatus>>,
    fail_status_once: Arc<Mutex<bool>>,
}

impl Default for MockProofRequester {
    fn default() -> Self {
        Self {
            requests: Arc::default(),
            status: Arc::new(Mutex::new(ProofStatus::Succeeded)),
            fail_status_once: Arc::default(),
        }
    }
}

impl MockProofRequester {
    fn with_status(status: ProofStatus) -> Self {
        Self {
            status: Arc::new(Mutex::new(status)),
            ..Self::default()
        }
    }

    fn set_status(&self, status: ProofStatus) {
        *self.status.lock().expect("not poisoned") = status;
    }

    fn fail_next_status(&self) {
        *self.fail_status_once.lock().expect("not poisoned") = true;
    }
}

#[async_trait]
impl ProofRequester for MockProofRequester {
    async fn request_proof(
        &self,
        request: ProofRequest,
    ) -> Result<RequestProofResponse, ProofRequestError> {
        let id = request.id();
        let l1_head = request.l1_head;
        self.requests.lock().expect("not poisoned").push(request);
        Ok(RequestProofResponse {
            proof_id: id,
            l1_head,
        })
    }

    async fn proof_status(
        &self,
        _proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        let mut fail_once = self.fail_status_once.lock().expect("not poisoned");
        if *fail_once {
            *fail_once = false;
            return Err(ProofRequestError::RemoteInternal);
        }
        Ok(*self.status.lock().expect("not poisoned"))
    }

    async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        Ok(ProofResponse::Succeeded(SucceededProofResponse {
            id: proof_id,
            proof: ProofData::Nitro {
                attestation: Bytes::from_static(&[1]),
                public_values: Bytes::from_static(&[2]),
                signature: Bytes::from_static(&[3]),
            },
        }))
    }
}

/// Two-phase bond client mirroring `MultiProofGame.claimCredit`: the first claim moves credit
/// into a pending `DelayedWETH` withdrawal, the second drains it.
#[derive(Debug, Clone)]
struct MockBondClient {
    proposer: Address,
    /// `(game, creator)` pairs in factory index order. `None` marks a foreign game type.
    games: Arc<Mutex<Vec<(Option<Address>, Address)>>>,
    requested_indices: Arc<Mutex<Vec<u64>>>,
    resolved_games: Arc<Mutex<HashSet<Address>>>,
    unfinalized_games: Arc<Mutex<HashSet<Address>>>,
    credit: Arc<Mutex<HashMap<Address, U256>>>,
    pending: Arc<Mutex<HashMap<Address, PendingWithdrawal>>>,
    latest_l1_timestamp: Arc<AtomicU64>,
    unlocks: Arc<Mutex<Vec<Address>>>,
    withdrawals: Arc<Mutex<Vec<Address>>>,
    fail_game_at_once: Arc<Mutex<Option<u64>>>,
    fail_claim_once: Arc<Mutex<HashSet<Address>>>,
}

impl MockBondClient {
    fn new(proposer: Address, games: Vec<(Address, Address)>) -> Self {
        Self::with_indexed_games(
            proposer,
            games
                .into_iter()
                .map(|(game, creator)| (Some(game), creator))
                .collect(),
        )
    }

    fn with_indexed_games(proposer: Address, games: Vec<(Option<Address>, Address)>) -> Self {
        Self {
            proposer,
            games: Arc::new(Mutex::new(games)),
            requested_indices: Arc::default(),
            resolved_games: Arc::default(),
            unfinalized_games: Arc::default(),
            credit: Arc::default(),
            pending: Arc::default(),
            latest_l1_timestamp: Arc::default(),
            unlocks: Arc::default(),
            withdrawals: Arc::default(),
            fail_game_at_once: Arc::default(),
            fail_claim_once: Arc::default(),
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

    async fn game_at(&self, index: u64) -> Result<Option<Address>, ProposerError> {
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

    async fn game_creator(&self, game: Address) -> Result<Address, ProposerError> {
        self.games
            .lock()
            .expect("not poisoned")
            .iter()
            .find_map(|(candidate, creator)| (*candidate == Some(game)).then_some(*creator))
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

    async fn is_game_finalized(&self, game: Address) -> Result<bool, ProposerError> {
        Ok(!self
            .unfinalized_games
            .lock()
            .expect("not poisoned")
            .contains(&game))
    }

    async fn credit(&self, game: Address) -> Result<U256, ProposerError> {
        Ok(self
            .credit
            .lock()
            .expect("not poisoned")
            .get(&game)
            .copied()
            .unwrap_or_default())
    }

    async fn pending_withdrawal(&self, game: Address) -> Result<PendingWithdrawal, ProposerError> {
        Ok(self
            .pending
            .lock()
            .expect("not poisoned")
            .get(&game)
            .copied()
            .unwrap_or_default())
    }

    async fn latest_l1_timestamp(&self) -> Result<u64, ProposerError> {
        Ok(self.latest_l1_timestamp.load(Ordering::SeqCst))
    }

    async fn claim_credit(&self, game: Address) -> Result<ClaimSubmission, ProposerError> {
        if self
            .fail_claim_once
            .lock()
            .expect("not poisoned")
            .remove(&game)
        {
            return Err(ProposerError::Contract("injected claim failure".into()));
        }

        let credit = self
            .credit
            .lock()
            .expect("not poisoned")
            .remove(&game)
            .unwrap_or_default();
        if credit > U256::ZERO {
            // Phase 1: unlock in DelayedWETH, immediately withdrawable in tests.
            self.pending.lock().expect("not poisoned").insert(
                game,
                PendingWithdrawal {
                    amount: credit,
                    unlock_at: 0,
                },
            );
            self.unlocks.lock().expect("not poisoned").push(game);
            return Ok(ClaimSubmission {
                tx_hash: B256::repeat_byte(0xdd),
                amount: credit,
                withdrawn: false,
            });
        }

        // Phase 2: drain the pending withdrawal.
        let pending = self
            .pending
            .lock()
            .expect("not poisoned")
            .remove(&game)
            .unwrap_or_default();
        self.withdrawals.lock().expect("not poisoned").push(game);
        Ok(ClaimSubmission {
            tx_hash: B256::repeat_byte(0xde),
            amount: pending.amount,
            withdrawn: true,
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
        poll_interval: Duration::from_secs(1),
        max_resolutions_per_tick: 1,
    }
}

async fn advance_proposal(
    proposer: &mut WorldChainProposer<MockContracts, MockOutputRoots, MockProofRequester>,
    scan: &CanonicalScan,
) -> Result<(), ProposerError> {
    proposer.request_proof(scan).await?;
    proposer.poll_and_submit().await
}

fn positive_ready_status() -> ResolutionStatus {
    ResolutionStatus {
        resolvable: true,
        root_state: RootState::Finalized,
        invalidation_reason: InvalidationReason::None,
    }
}

fn anchor_at(l2_block_number: u64) -> AnchorRef {
    AnchorRef {
        registry: ANCHOR,
        anchor_game: None,
        l2_block_number,
    }
}

/// The anchor after it has been advanced onto `anchor_game`, which is therefore no longer an
/// acceptable parent for new proposals.
fn anchor_advanced_onto(anchor_game: Address, l2_block_number: u64) -> AnchorRef {
    AnchorRef {
        registry: ANCHOR,
        anchor_game: Some(anchor_game),
        l2_block_number,
    }
}

fn timed_out_status() -> ResolutionStatus {
    ResolutionStatus {
        resolvable: false,
        root_state: RootState::Invalidated,
        invalidation_reason: InvalidationReason::ProofTimeout,
    }
}

fn game_uuid(parent_ref: Address, root_claim: B256, l2_block_number: u64, attempt: u64) -> B256 {
    ProposalCommitment {
        parent_ref,
        root_claim,
        l2_block_number,
        attempt,
    }
    .game_uuid(DOMAIN_HASH)
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
    games.insert(game_uuid(ANCHOR, root_10, 10, 0), GAME_1);

    let contracts = MockContracts {
        anchor: anchor_at(0),
        games,
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, root_10), (20, root_20)]),
        finalized_l2_block: 20,
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        output_roots,
        MockProofRequester::default(),
    );

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
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::clone(&submissions),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 10,
    };
    let mut proposer = WorldChainProposer::new(
        config(),
        contracts,
        output_roots,
        MockProofRequester::default(),
    );
    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    advance_proposal(&mut proposer, &canonical_scan)
        .await
        .unwrap();

    let proposal = submissions.lock().expect("not poisoned")[0].0;
    assert_eq!(proposal.parent_ref, ANCHOR);
    assert_eq!(proposal.root_claim, B256::repeat_byte(0x10));
    assert_eq!(proposal.l2_block_number, 10);
    assert_eq!(proposal.attempt, 0);
}

#[tokio::test]
async fn propose_waits_for_pending_proof_and_reuses_exact_request() {
    let submissions = Arc::default();
    let contracts = MockContracts {
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::clone(&submissions),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 10,
    };
    let proof_requester = MockProofRequester::with_status(ProofStatus::Running);
    let mut proposer =
        WorldChainProposer::new(config(), contracts, output_roots, proof_requester.clone());
    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    advance_proposal(&mut proposer, &canonical_scan)
        .await
        .unwrap();

    assert!(submissions.lock().expect("not poisoned").is_empty());
    let first_request = proof_requester.requests.lock().expect("not poisoned")[0].clone();
    assert_eq!(first_request.backend, ProofBackend::Nitro);
    assert_eq!(first_request.game, Address::ZERO);
    assert_eq!(first_request.l1_head, B256::repeat_byte(0xf1));

    proof_requester.set_status(ProofStatus::Succeeded);
    advance_proposal(&mut proposer, &canonical_scan)
        .await
        .unwrap();

    let requests = proof_requester.requests.lock().expect("not poisoned");
    assert_eq!(requests.as_slice(), &[first_request.clone(), first_request]);
    assert_eq!(submissions.lock().expect("not poisoned").len(), 1);
}

#[tokio::test]
async fn propose_rerequests_failed_proof_without_submitting() {
    let submissions = Arc::default();
    let contracts = MockContracts {
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::clone(&submissions),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 10,
    };
    let proof_requester = MockProofRequester::with_status(ProofStatus::Failed);
    let mut proposer =
        WorldChainProposer::new(config(), contracts, output_roots, proof_requester.clone());
    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    advance_proposal(&mut proposer, &canonical_scan)
        .await
        .unwrap();
    advance_proposal(&mut proposer, &canonical_scan)
        .await
        .unwrap();

    let requests = proof_requester.requests.lock().expect("not poisoned");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    assert!(submissions.lock().expect("not poisoned").is_empty());
}

#[tokio::test]
async fn propose_retains_request_after_transient_status_error() {
    let submissions = Arc::default();
    let contracts = MockContracts {
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::clone(&submissions),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 10,
    };
    let proof_requester = MockProofRequester::default();
    proof_requester.fail_next_status();
    let mut proposer =
        WorldChainProposer::new(config(), contracts, output_roots, proof_requester.clone());
    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert!(matches!(
        advance_proposal(&mut proposer, &canonical_scan).await,
        Err(ProposerError::ProofRequest(
            ProofRequestError::RemoteInternal
        ))
    ));
    assert!(submissions.lock().expect("not poisoned").is_empty());

    advance_proposal(&mut proposer, &canonical_scan)
        .await
        .unwrap();

    let requests = proof_requester.requests.lock().expect("not poisoned");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    assert_eq!(submissions.lock().expect("not poisoned").len(), 1);
}

#[tokio::test]
async fn propose_retries_submission_with_same_completed_proof() {
    let submissions = Arc::default();
    let submission_failures = Arc::new(Mutex::new(1));
    let contracts = MockContracts {
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::clone(&submissions),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures,
        unfinalized_games: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 10,
    };
    let proof_requester = MockProofRequester::default();
    let mut proposer =
        WorldChainProposer::new(config(), contracts, output_roots, proof_requester.clone());
    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert!(matches!(
        advance_proposal(&mut proposer, &canonical_scan).await,
        Err(ProposerError::Contract(_))
    ));
    assert!(submissions.lock().expect("not poisoned").is_empty());

    advance_proposal(&mut proposer, &canonical_scan)
        .await
        .unwrap();

    let requests = proof_requester.requests.lock().expect("not poisoned");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    assert_eq!(submissions.lock().expect("not poisoned").len(), 1);
}

#[tokio::test]
async fn propose_replaces_pending_request_when_canonical_candidate_changes() {
    let contracts = MockContracts {
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let proof_requester = MockProofRequester::with_status(ProofStatus::Running);
    let mut proposer = WorldChainProposer::new(
        config(),
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
        proof_requester.clone(),
    );
    let canonical_line = CanonicalLine::new(ParentRef {
        address: ANCHOR,
        l2_block_number: 0,
    });
    let first = CanonicalScan::new(
        canonical_line,
        NextProposalAction::Propose(Proposal {
            parent_ref: ANCHOR,
            root_claim: B256::repeat_byte(0x10),
            l2_block_number: 10,
            attempt: 0,
        }),
    );
    advance_proposal(&mut proposer, &first).await.unwrap();

    let second = CanonicalScan::new(
        CanonicalLine::new(ParentRef {
            address: ANCHOR,
            l2_block_number: 0,
        }),
        NextProposalAction::Propose(Proposal {
            parent_ref: ANCHOR,
            root_claim: B256::repeat_byte(0x20),
            l2_block_number: 10,
            attempt: 0,
        }),
    );
    advance_proposal(&mut proposer, &second).await.unwrap();

    {
        let requests = proof_requester.requests.lock().expect("not poisoned");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].root_claim, B256::repeat_byte(0x10));
        assert_eq!(requests[1].root_claim, B256::repeat_byte(0x20));
    }

    let caught_up = CanonicalScan::new(
        CanonicalLine::new(ParentRef {
            address: ANCHOR,
            l2_block_number: 10,
        }),
        NextProposalAction::CaughtUp {
            target_block: 20,
            finalized_block: 10,
        },
    );
    advance_proposal(&mut proposer, &caught_up).await.unwrap();
    assert!(!proposer.has_pending_proposal());
}

#[tokio::test]
async fn zero_block_interval_is_rejected() {
    let contracts = MockContracts {
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
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
        MockProofRequester::default(),
    );

    assert!(matches!(
        proposer.anchor_and_canonical_line().await,
        Err(ProposerError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn anchor_and_canonical_line_stops_at_finalized_l2_block() {
    let contracts = MockContracts {
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let output_roots = MockOutputRoots {
        roots: HashMap::from([(10, B256::repeat_byte(0x10))]),
        finalized_l2_block: 9,
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        output_roots,
        MockProofRequester::default(),
    );

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
        anchor: anchor_at(0),
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
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
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
        MockProofRequester::default(),
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
        anchor: anchor_at(0),
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
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        MockOutputRoots {
            roots: HashMap::new(),
            finalized_l2_block: 0,
        },
        MockProofRequester::default(),
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
        anchor: anchor_at(0),
        games: HashMap::new(),
        submissions: Arc::default(),
        resolution_statuses: Arc::default(),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
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
        MockProofRequester::default(),
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
        .push((Some(game_address(1_006)), proposer));
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
async fn bond_manager_prunes_resolved_games_and_retries_failed_claims() {
    let proposer = Address::repeat_byte(0xa1);
    let unresolved = game_address(1);
    let zero_credit = game_address(2);
    let claimable = game_address(3);
    let retry_claim = game_address(4);
    let awaiting_airgap = game_address(5);
    let games = vec![
        (unresolved, proposer),
        (zero_credit, proposer),
        (claimable, proposer),
        (retry_claim, proposer),
        (awaiting_airgap, proposer),
    ];
    let client = MockBondClient::new(proposer, games);
    client.resolved_games.lock().expect("not poisoned").extend([
        zero_credit,
        claimable,
        retry_claim,
        awaiting_airgap,
    ]);
    client
        .unfinalized_games
        .lock()
        .expect("not poisoned")
        .insert(awaiting_airgap);
    client.credit.lock().expect("not poisoned").extend([
        (claimable, U256::from(10)),
        (retry_claim, U256::from(20)),
        (awaiting_airgap, U256::from(30)),
    ]);
    client
        .fail_claim_once
        .lock()
        .expect("not poisoned")
        .insert(retry_claim);
    let mut manager = BondManager::new(bond_manager_config(100), client.clone());
    manager.scan_games().await.unwrap();

    // Pass 1: only `claimable` advances, and only through the DelayedWETH unlock.
    manager.withdraw_credits().await.unwrap();

    assert!(manager.tracks_game(unresolved));
    assert!(!manager.tracks_game(zero_credit));
    assert!(
        manager.tracks_game(claimable),
        "unlocked bond must stay tracked"
    );
    assert!(manager.tracks_game(retry_claim));
    assert!(
        manager.tracks_game(awaiting_airgap),
        "a game inside the finality airgap must stay tracked"
    );
    assert_eq!(
        *client.unlocks.lock().expect("not poisoned"),
        vec![claimable]
    );
    assert!(client.withdrawals.lock().expect("not poisoned").is_empty());

    // Pass 2: `claimable` withdraws and is dropped; the failed claim is retried and unlocks.
    manager.withdraw_credits().await.unwrap();

    assert!(!manager.tracks_game(claimable));
    assert!(manager.tracks_game(retry_claim));
    assert_eq!(
        *client.withdrawals.lock().expect("not poisoned"),
        vec![claimable]
    );

    // Pass 3: the retried claim withdraws and is dropped.
    manager.withdraw_credits().await.unwrap();

    assert!(!manager.tracks_game(retry_claim));
    let withdrawals = client.withdrawals.lock().expect("not poisoned");
    assert!(withdrawals.contains(&claimable));
    assert!(withdrawals.contains(&retry_claim));
}

#[tokio::test]
async fn bond_manager_skips_foreign_game_types() {
    let proposer = Address::repeat_byte(0xa1);
    let ours = game_address(1);
    let client = MockBondClient::with_indexed_games(
        proposer,
        vec![(None, proposer), (Some(ours), proposer), (None, proposer)],
    );
    let mut manager = BondManager::new(bond_manager_config(100), client);

    manager.scan_games().await.unwrap();

    assert!(manager.tracks_game(ours));
    assert_eq!(manager.next_game_index(), Some(3));
}

#[tokio::test]
async fn bond_manager_uses_l1_timestamp_for_delayed_withdrawal() {
    let proposer = Address::repeat_byte(0xa1);
    let game = game_address(1);
    let client = MockBondClient::new(proposer, vec![(game, proposer)]);
    client
        .resolved_games
        .lock()
        .expect("not poisoned")
        .insert(game);
    client.pending.lock().expect("not poisoned").insert(
        game,
        PendingWithdrawal {
            amount: U256::from(10),
            unlock_at: 100,
        },
    );
    client.latest_l1_timestamp.store(99, Ordering::SeqCst);

    let mut manager = BondManager::new(bond_manager_config(100), client.clone());
    manager.scan_games().await.unwrap();
    manager.withdraw_credits().await.unwrap();
    assert!(manager.tracks_game(game));
    assert!(client.withdrawals.lock().expect("not poisoned").is_empty());

    client.latest_l1_timestamp.store(100, Ordering::SeqCst);
    manager.withdraw_credits().await.unwrap();
    assert!(!manager.tracks_game(game));
    assert_eq!(
        *client.withdrawals.lock().expect("not poisoned"),
        vec![game]
    );
}

#[tokio::test]
async fn timed_out_game_rebases_onto_registry_after_its_parent_became_the_anchor() {
    // Anchor advanced onto P, so P is no longer an acceptable parent: `initialize` rejects any
    // parent at or below the anchor. The timed-out game under P therefore cannot be retried in
    // place; the transition must start over under the registry sentinel at attempt zero.
    let parent = game_address(1);
    let timed_out = game_address(2);
    let root_20 = B256::repeat_byte(0x20);
    let contracts = MockContracts {
        anchor: anchor_advanced_onto(parent, 10),
        games: HashMap::from([(game_uuid(parent, root_20, 20, 0), timed_out)]),
        submissions: Arc::default(),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([(timed_out, timed_out_status())]))),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        MockOutputRoots {
            roots: HashMap::from([(20, root_20)]),
            finalized_l2_block: 20,
        },
        MockProofRequester::default(),
    );

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert!(canonical_scan.canonical_line().games().is_empty());
    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::Propose(Proposal {
            parent_ref: ANCHOR,
            root_claim: root_20,
            l2_block_number: 20,
            attempt: 0,
        })
    );
}

#[tokio::test]
async fn invalidated_legacy_game_does_not_hide_a_live_registry_parented_rebase() {
    let parent = game_address(1);
    let timed_out = game_address(2);
    let rebased = game_address(3);
    let root_20 = B256::repeat_byte(0x20);
    let contracts = MockContracts {
        anchor: anchor_advanced_onto(parent, 10),
        games: HashMap::from([
            (game_uuid(parent, root_20, 20, 0), timed_out),
            (game_uuid(ANCHOR, root_20, 20, 0), rebased),
        ]),
        submissions: Arc::default(),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([(timed_out, timed_out_status())]))),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        MockOutputRoots {
            roots: HashMap::from([(20, root_20)]),
            finalized_l2_block: 20,
        },
        MockProofRequester::default(),
    );

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    // The live rebase continues the line even though the dead legacy game is scanned first.
    assert_eq!(
        canonical_scan.canonical_line().games(),
        &[ParentRef {
            address: rebased,
            l2_block_number: 20,
        }]
    );
}

#[tokio::test]
async fn timed_out_game_bumps_attempt_while_its_parent_is_still_acceptable() {
    let timed_out = game_address(2);
    let root_20 = B256::repeat_byte(0x20);
    let contracts = MockContracts {
        anchor: anchor_at(10),
        games: HashMap::from([(game_uuid(ANCHOR, root_20, 20, 0), timed_out)]),
        submissions: Arc::default(),
        resolution_statuses: Arc::new(Mutex::new(HashMap::from([(timed_out, timed_out_status())]))),
        resolutions: Arc::default(),
        closures: Arc::default(),
        submission_failures: Arc::default(),
        unfinalized_games: Arc::default(),
    };
    let proposer = WorldChainProposer::new(
        config(),
        contracts,
        MockOutputRoots {
            roots: HashMap::from([(20, root_20)]),
            finalized_l2_block: 20,
        },
        MockProofRequester::default(),
    );

    let canonical_scan = proposer.anchor_and_canonical_line().await.unwrap();

    assert_eq!(
        canonical_scan.next_action(),
        &NextProposalAction::RetryTimedOut {
            proposal: Proposal {
                parent_ref: ANCHOR,
                root_claim: root_20,
                l2_block_number: 20,
                attempt: 1,
            },
            invalidated_game: timed_out,
        }
    );
}
