//! Deterministic e2e harness for the World Chain proof services.
//!
//! The harness keeps the services under test real and replaces the external world with small,
//! stateful fakes: execution/contracts, consensus roots, and proof backends. This lets tests
//! exercise proposer -> challenger -> defender -> prover-service -> worker orchestration without
//! depending on a live chain or expensive proof generation.

use alloy_primitives::{Address, B256, BlockNumber, Bytes, U256, address};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use world_chain_challenger::{ChallengeSubmission, ChallengerClient, ChallengerError};
use world_chain_defender::{DefenderClient, DefenderError, DefenderSubmission};
use world_chain_proof_worker::{ClaimedProofJobHandler, ProofJob};
use world_chain_proofs::{
    ConsensusError, ConsensusProvider, GameCreated, PROOF_SYSTEM_VERSION, ProofDomain, ProofLane,
    RootCommitment, RootState, has_threshold,
};
use world_chain_proposer::{
    ParentRef, Proposal, ProposalSubmission, ProposerClient, ProposerError,
};
use world_chain_prover_service::{
    BackendSession, BackendSessionStatus, LockId, LockedProofRequest, ProofBackend, ProofData,
    ProofJobQueue, ProofJobQueueError, ProofRequest, ProofRequestError, ProofRequestId,
    ProofRequester, ProofResponse, ProofStatus, ProverService, ProverServiceConfig, SessionType,
    SucceededProofResponse,
};

pub const BLOCK_INTERVAL: u64 = 10;
pub const INTERMEDIATE_BLOCK_INTERVAL: u64 = 5;
pub const CHAIN_ID: u64 = 4801;
pub const ANCHOR: Address = address!("0000000000000000000000000000000000001006");

const STATE_NONE: u8 = 0;
const STATE_PROPOSED: u8 = 1;
const STATE_CHALLENGED: u8 = 2;
const STATE_FINALIZED: u8 = 3;

/// Domain used by the fake proof-system factory.
#[must_use]
pub fn test_domain() -> ProofDomain {
    ProofDomain {
        chain_id: CHAIN_ID,
        proof_system_version: PROOF_SYSTEM_VERSION,
        rollup_config_hash: B256::repeat_byte(0x99),
        block_interval: BLOCK_INTERVAL,
        intermediate_block_interval: INTERMEDIATE_BLOCK_INTERVAL,
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
    anchor: ParentRef,
    finalized_l1_block: BlockNumber,
    next_game_nonce: u8,
    games_by_key: HashMap<B256, Address>,
    games_by_address: HashMap<Address, GameRecord>,
    game_order: Vec<Address>,
}

#[derive(Debug, Clone)]
struct GameRecord {
    event: GameCreated,
    state: u8,
    challenge_deadline: u64,
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
                anchor: ParentRef {
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
    pub fn latest_game(&self) -> Option<GameCreated> {
        let state = self.state.lock().expect("fake execution mutex poisoned");
        state
            .game_order
            .last()
            .and_then(|game| state.games_by_address.get(game))
            .map(|record| record.event)
    }

    #[must_use]
    pub fn game_state(&self, game: Address) -> RootState {
        let raw = self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map_or(STATE_NONE, |record| record.state);
        RootState::try_from(raw).expect("fake execution stores valid root states")
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

    fn create_game(state: &mut FakeExecutionState, proposal: &Proposal) -> GameCreated {
        let game = Address::with_last_byte(state.next_game_nonce);
        state.next_game_nonce = state.next_game_nonce.saturating_add(1);

        let l1_origin_number = state.finalized_l1_block.saturating_sub(1);
        let l1_origin_hash = B256::with_last_byte(l1_origin_number as u8);
        let root = RootCommitment {
            proposal: proposal.commitment(),
            l1_origin_hash,
            l1_origin_number,
        };
        let event = GameCreated {
            proposal_key: proposal.proposal_key,
            root_id: root.root_id(state.domain_hash),
            game,
            proposer: Address::repeat_byte(0xa1),
            root_claim: proposal.root_claim,
            l2_block_number: proposal.l2_block_number,
            parent_ref: proposal.parent_ref,
            l1_origin_hash,
            l1_origin_number,
        };

        state.games_by_key.insert(proposal.proposal_key, game);
        state.game_order.push(game);
        state.games_by_address.insert(
            game,
            GameRecord {
                event,
                state: STATE_PROPOSED,
                challenge_deadline: u64::MAX,
                proof_bitmap: 0,
                challenge_count: 0,
                submitted_lanes: Vec::new(),
            },
        );
        event
    }
}

fn challenge_record(record: &mut GameRecord) {
    record.challenge_count = record.challenge_count.saturating_add(1);
    if record.state == STATE_PROPOSED {
        record.state = STATE_CHALLENGED;
    }
}

fn proof_lane(lane: u8) -> Option<ProofLane> {
    match lane {
        0 => Some(ProofLane::ValidityProof),
        1 => Some(ProofLane::TeeAttestation),
        2 => Some(ProofLane::SecurityCouncil),
        _ => None,
    }
}

#[async_trait]
impl ProposerClient for FakeExecution {
    async fn anchor_parent(&self) -> Result<ParentRef, ProposerError> {
        Ok(self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .anchor)
    }

    async fn proposal_key(
        &self,
        commitment: world_chain_proofs::ProposalCommitment,
    ) -> Result<B256, ProposerError> {
        Ok(commitment.proposal_key(
            self.state
                .lock()
                .expect("fake execution mutex poisoned")
                .domain_hash,
        ))
    }

    async fn game_for_proposal_key(
        &self,
        proposal_key: B256,
    ) -> Result<Option<Address>, ProposerError> {
        Ok(self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_key
            .get(&proposal_key)
            .copied())
    }

    async fn submit_proposal(
        &self,
        proposal: &Proposal,
        _proposer_bond: U256,
    ) -> Result<ProposalSubmission, ProposerError> {
        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        if let Some(existing) = state.games_by_key.get(&proposal.proposal_key) {
            return Err(ProposerError::Contract(format!(
                "game already exists for proposal key {} at {existing}",
                proposal.proposal_key
            )));
        }
        let event = Self::create_game(&mut state, proposal);
        Ok(ProposalSubmission {
            tx_hash: B256::with_last_byte(event.game.as_slice()[19]),
        })
    }
}

#[async_trait]
impl ChallengerClient for FakeExecution {
    async fn root_state(&self, game: Address) -> Result<RootState, ChallengerError> {
        let raw = self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map_or(STATE_NONE, |record| record.state);
        RootState::try_from(raw).map_err(Into::into)
    }

    async fn finalized_l1_block_num(&self) -> Result<BlockNumber, ChallengerError> {
        Ok(self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .finalized_l1_block)
    }

    async fn games_created(
        &self,
        _from: BlockNumber,
        _to: BlockNumber,
    ) -> Result<Vec<GameCreated>, ChallengerError> {
        let state = self.state.lock().expect("fake execution mutex poisoned");
        Ok(state
            .game_order
            .iter()
            .filter_map(|game| state.games_by_address.get(game))
            .map(|record| record.event)
            .collect())
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, ChallengerError> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map(|record| record.challenge_deadline)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))
    }

    async fn submit_challenge(
        &self,
        game: Address,
        _challenger_bond: U256,
    ) -> Result<ChallengeSubmission, ChallengerError> {
        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        let record = state
            .games_by_address
            .get_mut(&game)
            .ok_or_else(|| ChallengerError::Contract(format!("unknown game {game}")))?;
        challenge_record(record);
        Ok(ChallengeSubmission {
            tx_hash: B256::with_last_byte(record.challenge_count as u8),
        })
    }
}

#[async_trait]
impl DefenderClient for FakeExecution {
    async fn root_state(&self, game: Address) -> Result<RootState, DefenderError> {
        let raw = self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map_or(STATE_NONE, |record| record.state);
        RootState::try_from(raw).map_err(Into::into)
    }

    async fn finalized_l1_block_num(&self) -> Result<BlockNumber, DefenderError> {
        Ok(self
            .state
            .lock()
            .expect("fake execution mutex poisoned")
            .finalized_l1_block)
    }

    async fn games_created(
        &self,
        _from: BlockNumber,
        _to: BlockNumber,
    ) -> Result<Vec<GameCreated>, DefenderError> {
        let state = self.state.lock().expect("fake execution mutex poisoned");
        Ok(state
            .game_order
            .iter()
            .filter_map(|game| state.games_by_address.get(game))
            .map(|record| record.event)
            .collect())
    }

    async fn challenge_deadline(&self, game: Address) -> Result<u64, DefenderError> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map(|record| record.challenge_deadline)
            .ok_or_else(|| DefenderError::Contract(format!("unknown game {game}")))
    }

    async fn proof_bitmap(&self, game: Address) -> Result<u8, DefenderError> {
        self.state
            .lock()
            .expect("fake execution mutex poisoned")
            .games_by_address
            .get(&game)
            .map(|record| record.proof_bitmap)
            .ok_or_else(|| DefenderError::Contract(format!("unknown game {game}")))
    }

    async fn submit_proof(
        &self,
        game: Address,
        lane: u8,
        proof: Bytes,
    ) -> Result<DefenderSubmission, DefenderError> {
        let lane =
            proof_lane(lane).ok_or_else(|| DefenderError::Contract("invalid lane".into()))?;
        if proof.is_empty() {
            return Err(DefenderError::Contract("empty proof".into()));
        }

        let mut state = self.state.lock().expect("fake execution mutex poisoned");
        let record = state
            .games_by_address
            .get_mut(&game)
            .ok_or_else(|| DefenderError::Contract(format!("unknown game {game}")))?;
        if record.state != STATE_CHALLENGED {
            return Err(DefenderError::Contract(format!(
                "game {game} is not challenged"
            )));
        }

        let mask = lane.mask();
        if record.proof_bitmap & mask == 0 {
            record.proof_bitmap |= mask;
            record.submitted_lanes.push(lane);
            if has_threshold(record.proof_bitmap) {
                record.state = STATE_FINALIZED;
            }
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
    ) -> Result<ProofRequestId, ProofRequestError> {
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
        backend: ProofBackend,
        worker_id: String,
    ) -> Result<Option<LockedProofRequest>, ProofJobQueueError> {
        self.service.get_next_proof(backend, worker_id).await
    }

    async fn submit_proof(
        &self,
        proof: SucceededProofResponse,
        worker_id: String,
        lock: LockId,
    ) -> Result<(), ProofJobQueueError> {
        self.service.submit_proof(proof, worker_id, lock).await
    }

    async fn get_proof_session(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
    ) -> Result<Option<BackendSession>, ProofJobQueueError> {
        self.service.get_proof_session(proof_id, session_type).await
    }

    async fn record_proof_session(
        &self,
        proof_id: ProofRequestId,
        session_type: SessionType,
        worker_id: String,
        lock_id: LockId,
        backend_session_id: String,
        state: BackendSessionStatus,
    ) -> Result<(), ProofJobQueueError> {
        self.service
            .record_proof_session(
                proof_id,
                session_type,
                worker_id,
                lock_id,
                backend_session_id,
                state,
            )
            .await
    }
}
