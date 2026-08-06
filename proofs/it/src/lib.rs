//! Deterministic e2e harness for the World Chain proof services.
//!
//! The harness keeps the services under test real and replaces the external world with small,
//! stateful fakes: execution/contracts, consensus roots, and proof backends. This lets tests
//! exercise proposer -> challenger -> defender -> prover-service -> worker orchestration without
//! depending on a live chain or expensive proof generation.

use alloy_primitives::{Address, B256, BlockNumber, Bytes, U256, address};
use alloy_sol_types::SolValue;
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use world_chain_challenger::{
    ChallengeSubmission, ChallengerClient, ChallengerError, GameMetadata as ChallengerGameMetadata,
};
use world_chain_defender::{
    DefenderClient, DefenderError, DefenderSubmission, GameMetadata as DefenderGameMetadata,
};
use world_chain_proof_core::boot::TransitionPublicValues;
use world_chain_proof_protocol::{
    ClaimData, ConsensusError, ConsensusProvider, GameCreation, GameStatus, InvalidationReason,
    LineageAnchor, LineageError, LineageGame, LineageProvider, LineageTransition, MAX_ATTEMPT_SCAN,
    PROOF_SYSTEM_VERSION, PROOF_THRESHOLD, ProofDomain, ProofLane, ProposalCommitment,
    ProposalStatus, ResolutionStatus, RootCommitment, has_threshold,
};
use world_chain_proof_worker::{ClaimedProofJobHandler, ProofJob};
use world_chain_proposer::{
    CloseGameSubmission, Proposal, ProposalSubmission, ProposerClient, ProposerError,
    ResolveSubmission,
};
use world_chain_prover_service::{
    GetNextProofRequest, GetNextProofResponse, GetProofSessionRequest, GetProofSessionResponse,
    HeartbeatRequest, HeartbeatResponse, ProofBackend, ProofData, ProofJobQueue,
    ProofJobQueueError, ProofRequest, ProofRequestError, ProofRequestId, ProofRequester,
    ProofResponse, ProofStatus, ProverService, ProverServiceConfig, RecordProofSessionRequest,
    RecordProofSessionResponse, RequestProofResponse, SubmitProofRequest, SubmitProofResponse,
};

pub const BLOCK_INTERVAL: u64 = 10;
pub const CHAIN_ID: u64 = 4801;
pub const ANCHOR: Address = address!("0000000000000000000000000000000000001006");
pub const FAKE_PROPOSER: Address = address!("a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1");

/// Fake game lifecycle. The contract splits this across `ProposalStatus` (challenged or not)
/// and `GameStatus` (the terminal outcome); the fake keeps one field and derives both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameLifecycle {
    Proposed,
    Challenged,
    Finalized,
    Invalidated,
}

impl GameLifecycle {
    const fn outcome(self) -> GameStatus {
        match self {
            Self::Proposed | Self::Challenged => GameStatus::InProgress,
            Self::Finalized => GameStatus::DefenderWins,
            Self::Invalidated => GameStatus::ChallengerWins,
        }
    }

    const fn proposal_status(self, proven: bool) -> ProposalStatus {
        match self {
            Self::Proposed if proven => ProposalStatus::UnchallengedAndValidProofProvided,
            Self::Proposed => ProposalStatus::Unchallenged,
            Self::Challenged if proven => ProposalStatus::ChallengedAndValidProofProvided,
            Self::Challenged => ProposalStatus::Challenged,
            Self::Finalized | Self::Invalidated => ProposalStatus::Resolved,
        }
    }
}

/// Domain used by the fake proof-system factory.
#[must_use]
pub fn test_domain() -> ProofDomain {
    ProofDomain {
        chain_id: CHAIN_ID,
        proof_system_version: PROOF_SYSTEM_VERSION,
        rollup_config_hash: B256::repeat_byte(0x99),
        block_interval: BLOCK_INTERVAL,
    }
}

#[derive(Debug, Clone)]
pub struct FakeConsensus {
    state: Arc<Mutex<FakeConsensusState>>,
}

#[derive(Debug)]
struct FakeConsensusState {
    finalized_l2_block: BlockNumber,
    roots: HashMap<BlockNumber, B256>,
}

impl FakeConsensus {
    #[must_use]
    pub fn new(finalized_l2_block: BlockNumber) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeConsensusState {
                finalized_l2_block,
                roots: HashMap::new(),
            })),
        }
    }

    #[must_use]
    pub fn with_root(self, l2_block_number: BlockNumber, root: B256) -> Self {
        self.set_root(l2_block_number, root);
        self
    }

    pub fn set_root(&self, l2_block_number: BlockNumber, root: B256) {
        self.state
            .lock()
            .expect("fake consensus mutex poisoned")
            .roots
            .insert(l2_block_number, root);
    }

    pub fn set_finalized_l2_block(&self, finalized_l2_block: BlockNumber) {
        self.state
            .lock()
            .expect("fake consensus mutex poisoned")
            .finalized_l2_block = finalized_l2_block;
    }
}

#[async_trait]
impl ConsensusProvider for FakeConsensus {
    async fn output_root_at_block(&self, l2_block_number: u64) -> Result<B256, ConsensusError> {
        self.state
            .lock()
            .expect("fake consensus mutex poisoned")
            .roots
            .get(&l2_block_number)
            .copied()
            .ok_or(ConsensusError::MissingOutputRoot)
    }

    async fn latest_l2_finalized_block(&self) -> Result<BlockNumber, ConsensusError> {
        Ok(self
            .state
            .lock()
            .expect("fake consensus mutex poisoned")
            .finalized_l2_block)
    }
}

#[derive(Debug, Clone)]
pub struct FakeExecution {
    state: Arc<Mutex<FakeExecutionState>>,
}

#[derive(Debug)]
struct FakeExecutionState {
    domain_hash: B256,
    anchor: LineageAnchor,
    finalized_l1_block: BlockNumber,
    next_game_nonce: u8,
    games_by_key: HashMap<B256, Address>,
    games_by_address: HashMap<Address, GameRecord>,
    game_order: Vec<Address>,
}

#[derive(Debug, Clone)]
struct GameRecord {
    creation: GameCreation,
    state: GameLifecycle,
    challenge_deadline: u64,
    proof_deadline: u64,
    proof_bitmap: u8,
    challenge_count: u32,
    submitted_lanes: Vec<ProofLane>,
}

impl Default for FakeExecution {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeExecution {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeExecutionState {
                domain_hash: test_domain().hash(),
                anchor: LineageAnchor {
                    address: ANCHOR,
                    l2_block_number: 0,
                },
                finalized_l1_block: 10_000,
                next_game_nonce: 1,
                games_by_key: HashMap::new(),
                games_by_address: HashMap::new(),
                game_order: Vec::new(),
            })),
        }
    }

    #[must_use]
    pub fn latest_game(&self) -> Option<GameCreation> {
        let state = self.state.lock().expect("fake execution mutex poisoned");
        state
            .game_order
            .last()
            .and_then(|game| state.games_by_address.get(game))
            .map(|record| record.creation)
    }

    #[must_use]
    pub fn game_state(&self, game: Address) -> Option<GameLifecycle> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map(|record| record.state)
    }

    #[must_use]
    pub fn proof_bitmap(&self, game: Address) -> u8 {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map_or(0, |record| record.proof_bitmap)
    }

    #[must_use]
    pub fn submitted_lanes(&self, game: Address) -> Vec<ProofLane> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map_or_else(Vec::new, |record| record.submitted_lanes.clone())
    }

    #[must_use]
    pub fn challenge_count(&self, game: Address) -> u32 {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map_or(0, |record| record.challenge_count)
    }

    pub fn challenge_game(&self, game: Address) {
        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        challenge_record(state.games_by_address.get_mut(&game).expect("game exists"));
    }

    fn create_game(state: &mut FakeExecutionState, proposal: &Proposal) -> GameCreation {
        let game = Address::with_last_byte(state.next_game_nonce);
        state.next_game_nonce = state.next_game_nonce.saturating_add(1);

        let l1_origin_number = state.finalized_l1_block.saturating_sub(1);
        let l1_origin_hash = B256::with_last_byte(l1_origin_number as u8);
        let root = RootCommitment {
            proposal: proposal.commitment(),
            l1_origin_hash,
            l1_origin_number,
        };
        let creation = GameCreation {
            root_id: root.root_id(state.domain_hash),
            game,
            game_creator: FAKE_PROPOSER,
            root_claim: proposal.root_claim,
            l2_block_number: proposal.l2_block_number,
            parent_ref: proposal.parent_ref,
            l1_origin_hash,
            l1_origin_number,
            attempt: proposal.attempt,
        };

        state
            .games_by_key
            .insert(proposal.commitment().game_uuid(state.domain_hash), game);
        state.game_order.push(game);
        state.games_by_address.insert(
            game,
            GameRecord {
                creation,
                state: GameLifecycle::Proposed,
                challenge_deadline: u64::MAX,
                proof_deadline: u64::MAX,
                proof_bitmap: 0,
                challenge_count: 0,
                submitted_lanes: Vec::new(),
            },
        );
        creation
    }
}

fn challenge_record(record: &mut GameRecord) {
    record.challenge_count = record.challenge_count.saturating_add(1);
    if record.state == GameLifecycle::Proposed {
        record.state = GameLifecycle::Challenged;
    }
}

fn parent_is_unresolved(state: &FakeExecutionState, record: &GameRecord) -> bool {
    if record.creation.parent_ref == ANCHOR {
        return false;
    }
    state
        .games_by_address
        .get(&record.creation.parent_ref)
        .is_some_and(|parent| {
            matches!(
                parent.state,
                GameLifecycle::Proposed | GameLifecycle::Challenged
            )
        })
}

#[async_trait]
impl LineageProvider for FakeExecution {
    fn lineage_block_interval(&self) -> u64 {
        BLOCK_INTERVAL
    }

    async fn lineage_anchor(&self) -> Result<LineageAnchor, LineageError> {
        Ok(self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .anchor)
    }

    async fn game_for_transition(
        &self,
        transition: LineageTransition,
    ) -> Result<Option<LineageGame>, LineageError> {
        let state = self.state.lock().expect("fake execution mutex poisoned");
        let mut latest = None;
        for attempt in 0..MAX_ATTEMPT_SCAN {
            let uuid = ProposalCommitment {
                parent_ref: transition.parent_ref,
                root_claim: transition.root_claim,
                l2_block_number: transition.l2_block_number,
                attempt,
            }
            .game_uuid(state.domain_hash);
            let Some(address) = state.games_by_key.get(&uuid).copied() else {
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
        let state = self.state.lock().expect("fake execution mutex poisoned");
        let record = state
            .games_by_address
            .get(&game)
            .ok_or_else(|| LineageError::Contract(format!("unknown game {game}")))?;

        let outcome = record.state.outcome();
        if record.state == GameLifecycle::Finalized {
            return Ok(ResolutionStatus {
                resolvable: false,
                outcome,
                invalidation_reason: InvalidationReason::None,
            });
        }
        if parent_is_unresolved(&state, record) {
            return Ok(ResolutionStatus {
                resolvable: false,
                outcome,
                invalidation_reason: InvalidationReason::None,
            });
        }
        if matches!(
            record.state,
            GameLifecycle::Proposed | GameLifecycle::Challenged
        ) && has_threshold(record.proof_bitmap)
        {
            return Ok(ResolutionStatus {
                resolvable: true,
                outcome: GameStatus::DefenderWins,
                invalidation_reason: InvalidationReason::None,
            });
        }
        if record.state == GameLifecycle::Challenged && record.proof_deadline == 0 {
            return Ok(ResolutionStatus {
                resolvable: true,
                outcome: GameStatus::ChallengerWins,
                invalidation_reason: InvalidationReason::ProofTimeout,
            });
        }
        if record.state == GameLifecycle::Proposed && record.challenge_deadline == 0 {
            return Ok(ResolutionStatus {
                resolvable: true,
                outcome: if record.proof_bitmap == 0 {
                    GameStatus::ChallengerWins
                } else {
                    GameStatus::DefenderWins
                },
                invalidation_reason: if record.proof_bitmap == 0 {
                    InvalidationReason::ProofTimeout
                } else {
                    InvalidationReason::None
                },
            });
        }

        Ok(ResolutionStatus {
            resolvable: false,
            outcome,
            invalidation_reason: InvalidationReason::None,
        })
    }
}

#[async_trait]
impl ProposerClient for FakeExecution {
    /// The fake has no registry finality airgap: a resolved game is immediately closeable.
    async fn is_game_finalized(&self, _game: Address) -> Result<bool, ProposerError> {
        Ok(true)
    }

    async fn resolve_game(&self, game: Address) -> Result<ResolveSubmission, ProposerError> {
        let status = self.lineage_resolution_status(game).await?;
        if !status.resolvable {
            return Err(ProposerError::message(format!(
                "game {game} is not resolvable"
            )));
        }
        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        let record = state
            .games_by_address
            .get_mut(&game)
            .ok_or_else(|| ProposerError::message(format!("unknown game {game}")))?;
        record.state = match status.outcome {
            GameStatus::DefenderWins => GameLifecycle::Finalized,
            GameStatus::ChallengerWins => GameLifecycle::Invalidated,
            GameStatus::InProgress => {
                return Err(ProposerError::message(format!(
                    "game {game} has no terminal outcome"
                )));
            }
        };

        Ok(ResolveSubmission {
            tx_hash: B256::with_last_byte(game.as_slice()[19]),
        })
    }

    async fn close_game(&self, game: Address) -> Result<CloseGameSubmission, ProposerError> {
        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        let record = state
            .games_by_address
            .get(&game)
            .ok_or_else(|| ProposerError::message(format!("unknown game {game}")))?;
        if record.state != GameLifecycle::Finalized {
            return Err(ProposerError::message(format!(
                "game {game} is not finalized"
            )));
        }
        let l2_block_number = record.creation.l2_block_number;
        state.anchor = LineageAnchor {
            address: game,
            l2_block_number,
        };
        Ok(CloseGameSubmission {
            tx_hash: B256::with_last_byte(game.as_slice()[19]),
        })
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
    ) -> Result<ProposalSubmission, ProposerError> {
        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        let uuid = proposal.commitment().game_uuid(state.domain_hash);
        if let Some(existing) = state.games_by_key.get(&uuid) {
            return Err(ProposerError::message(format!(
                "game already exists for factory uuid {uuid} at {existing}"
            )));
        }
        let event = Self::create_game(&mut state, proposal);
        Ok(ProposalSubmission {
            tx_hash: B256::with_last_byte(event.game.as_slice()[19]),
            game_address: event.game,
        })
    }
}

#[async_trait]
impl ChallengerClient for FakeExecution {
    async fn game_count(&self) -> Result<u64, ChallengerError> {
        Ok(self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .game_order
            .len() as u64)
    }

    async fn game_address_at(&self, index: u64) -> Result<Option<Address>, ChallengerError> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .game_order
            .get(index as usize)
            .copied()
            .map(Some)
            .ok_or_else(|| ChallengerError::message(format!("unknown game index {index}")))
    }

    async fn game_metadata(
        &self,
        game: Address,
    ) -> Result<ChallengerGameMetadata, ChallengerError> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map(|record| ChallengerGameMetadata {
                address: game,
                root_claim: record.creation.root_claim,
                l2_block_number: record.creation.l2_block_number,
            })
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))
    }

    async fn proposal_status(&self, game: Address) -> Result<ProposalStatus, ChallengerError> {
        Ok(self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map_or(ProposalStatus::Resolved, |record| {
                record.state.proposal_status(record.proof_bitmap != 0)
            }))
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map(|record| record.challenge_deadline)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))
    }

    async fn submit_challenge(
        &self,
        game: Address,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        let record = state
            .games_by_address
            .get_mut(&game)
            .ok_or_else(|| ChallengerError::message(format!("unknown game {game}")))?;
        challenge_record(record);
        Ok(ChallengeSubmission {
            tx_hash: B256::with_last_byte(record.challenge_count as u8),
            bond: U256::from(1),
        })
    }
}

#[async_trait]
impl DefenderClient for FakeExecution {
    async fn game_metadata(&self, game: Address) -> Result<DefenderGameMetadata, DefenderError> {
        let state = self.state.lock().expect("fake execution mutex poisoned");
        state
            .games_by_address
            .get(&game)
            .map(|record| DefenderGameMetadata {
                address: game,
                domain_hash: state.domain_hash,
                parent_ref: record.creation.parent_ref,
                root_claim: record.creation.root_claim,
                l2_block_number: record.creation.l2_block_number,
                l1_origin_hash: record.creation.l1_origin_hash,
                l1_origin_number: record.creation.l1_origin_number,
                challenge_deadline: record.challenge_deadline,
                proof_deadline: record.proof_deadline,
                proof_threshold: PROOF_THRESHOLD,
            })
            .ok_or_else(|| DefenderError::message(format!("unknown game {game}")))
    }

    async fn claim_data(&self, game: Address) -> Result<ClaimData, DefenderError> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map(|record| ClaimData {
                status: record.state.proposal_status(record.proof_bitmap != 0),
                challenger: Address::ZERO,
                deadline: record.proof_deadline,
                proof_bitmap: record.proof_bitmap,
                invalidation_reason: InvalidationReason::None,
            })
            .ok_or_else(|| DefenderError::message(format!("unknown game {game}")))
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: ProofLane,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
        if proof.is_empty() {
            return Err(DefenderError::message("empty proof"));
        }

        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        let record = state
            .games_by_address
            .get_mut(&game)
            .ok_or_else(|| DefenderError::message(format!("unknown game {game}")))?;
        if !matches!(
            record.state,
            GameLifecycle::Proposed | GameLifecycle::Challenged
        ) {
            return Err(DefenderError::message(format!(
                "game {game} is not open for proofs"
            )));
        }

        let mask = lane.mask();
        if record.proof_bitmap & mask == 0 {
            record.proof_bitmap |= mask;
            record.submitted_lanes.push(lane);
        }

        Ok(DefenderSubmission {
            tx_hash: B256::with_last_byte(record.proof_bitmap),
        })
    }
}

#[derive(Debug, Clone)]
pub struct FakeProofBackend {
    lane: ProofBackend,
    failures_before_success: u32,
    attempts: Arc<Mutex<HashMap<ProofRequestId, u32>>>,
}

impl FakeProofBackend {
    #[must_use]
    pub fn new(lane: ProofBackend) -> Self {
        Self {
            lane,
            failures_before_success: 0,
            attempts: Arc::default(),
        }
    }

    #[must_use]
    pub fn flaky(lane: ProofBackend, failures_before_success: u32) -> Self {
        Self {
            lane,
            failures_before_success,
            attempts: Arc::default(),
        }
    }
}

#[async_trait]
impl ClaimedProofJobHandler for FakeProofBackend {
    fn lane(&self) -> ProofBackend {
        self.lane
    }

    async fn handle_claimed_job(&self, job: ProofJob) -> anyhow::Result<ProofData> {
        let request = &job.request;
        let id = request.id();
        {
            let mut attempts = self.attempts.lock().expect("fake backend mutex poisoned");
            let count = attempts.entry(id).or_default();
            if *count < self.failures_before_success {
                *count += 1;
                anyhow::bail!("configured fake proof failure for {id}");
            }
            *count += 1;
        }

        Ok(match self.lane {
            ProofBackend::Sp1 => ProofData::Sp1 {
                public_values: request.root_claim.as_slice().to_vec().into(),
                proof: vec![0x51, request.l2_block_number as u8].into(), // mock proof
            },
            ProofBackend::Nitro => ProofData::Nitro {
                attestation: request.l1_head.as_slice().to_vec().into(),
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
                signature: vec![0x7e, request.l2_block_number as u8].into(), // mock signature
            },
        })
    }
}

#[derive(Debug, Clone)]
pub struct SharedProverService {
    service: Arc<ProverService>,
}

impl SharedProverService {
    pub async fn connect(
        database_url: &str,
        config: ProverServiceConfig,
    ) -> Result<Self, world_chain_prover_service::ProverServiceInitError> {
        Ok(Self {
            service: Arc::new(ProverService::connect(database_url, config).await?),
        })
    }
}

#[async_trait]
impl ProofRequester for SharedProverService {
    async fn request_proof(
        &self,
        proof_request: ProofRequest,
    ) -> Result<RequestProofResponse, ProofRequestError> {
        self.service.request_proof(proof_request).await
    }

    async fn proof_status(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofStatus, ProofRequestError> {
        self.service.proof_status(proof_id).await
    }

    async fn get_proof(
        &self,
        proof_id: ProofRequestId,
    ) -> Result<ProofResponse, ProofRequestError> {
        self.service.get_proof(proof_id).await
    }
}

#[async_trait]
impl ProofJobQueue for SharedProverService {
    async fn get_next_proof(
        &self,
        request: GetNextProofRequest,
    ) -> Result<GetNextProofResponse, ProofJobQueueError> {
        self.service.get_next_proof(request).await
    }

    async fn submit_proof(
        &self,
        request: SubmitProofRequest,
    ) -> Result<SubmitProofResponse, ProofJobQueueError> {
        self.service.submit_proof(request).await
    }

    async fn get_proof_session(
        &self,
        request: GetProofSessionRequest,
    ) -> Result<GetProofSessionResponse, ProofJobQueueError> {
        self.service.get_proof_session(request).await
    }

    async fn record_proof_session(
        &self,
        request: RecordProofSessionRequest,
    ) -> Result<RecordProofSessionResponse, ProofJobQueueError> {
        self.service.record_proof_session(request).await
    }

    async fn heartbeat(
        &self,
        request: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, ProofJobQueueError> {
        self.service.heartbeat(request).await
    }
}
