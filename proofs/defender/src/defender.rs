use crate::{
    config::DefenderConfig,
    error::DefenderError,
    traits::DefenderClient,
    types::{ActiveDefense, DEFENDED_LANES, DefenseProgress, LaneState, WatchOutcome, WatchedGame},
};
use alloy_primitives::{Address, BlockNumber, Bytes};
use futures_util::{StreamExt, stream};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{error, info, warn};
use world_chain_proofs::{ConsensusProvider, GameCreated, ProofLane, RootState};
use world_chain_prover_service::{
    ProofBackend, ProofData, ProofRequest, ProofRequester, ProofStatus,
};

/// The number of L1 blocks published in 24h.
const ONE_DAY_OF_L1_BLOCKS: u64 = 7_200;
/// The safety margin of blocks.
const MARGIN: u64 = 500;

/// World Chain Defender.
///
/// Watches every created game until it leaves the `Proposed` state. When a
/// game is challenged and its root claim matches the canonical output root,
/// the defender requests one proof per defended lane from the
/// `prover-service` and submits each completed proof on-chain via
/// `submitProofLane`.
#[derive(Debug)]
pub struct WorldChainDefender<E, C, P> {
    config: DefenderConfig,
    execution_provider: E,
    consensus_provider: C,
    proof_requester: P,
    cursor: BlockNumber,
    watched_games: HashMap<Address, WatchedGame>,
    active_defenses: HashMap<Address, ActiveDefense>,
}

impl<E, C, P> WorldChainDefender<E, C, P> {
    /// Creates a defender from execution, consensus and prover-requester clients.
    pub fn new(
        config: DefenderConfig,
        execution_provider: E,
        consensus_provider: C,
        proof_requester: P,
    ) -> Self {
        Self {
            config,
            execution_provider,
            consensus_provider,
            proof_requester,
            cursor: 0,
            watched_games: HashMap::default(),
            active_defenses: HashMap::default(),
        }
    }

    /// Returns the defender configuration.
    #[must_use]
    pub const fn config(&self) -> &DefenderConfig {
        &self.config
    }

    #[cfg(test)]
    pub(crate) fn watched_games(&self) -> Vec<Address> {
        self.watched_games.keys().copied().collect()
    }

    #[cfg(test)]
    pub(crate) fn active_defenses(&self) -> Vec<Address> {
        self.active_defenses.keys().copied().collect()
    }
}

impl<E, C, P> WorldChainDefender<E, C, P>
where
    E: DefenderClient,
    C: ConsensusProvider,
    P: ProofRequester + Sync,
{
    async fn watch_game(
        &self,
        watched: &WatchedGame,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Result<WatchOutcome, DefenderError> {
        let game_created = &watched.game_created;
        let game = game_created.game;
        match self.execution_provider.root_state(game).await? {
            RootState::Proposed => {
                let challenge_deadline = match watched.challenge_deadline {
                    Some(challenge_deadline) => challenge_deadline,
                    None => self.execution_provider.challenge_deadline(game).await?,
                };
                if now >= challenge_deadline {
                    // the game can no longer be challenged
                    return Ok(WatchOutcome::Drop);
                }
                Ok(WatchOutcome::Keep {
                    challenge_deadline: Some(challenge_deadline),
                })
            }
            RootState::Challenged => {
                // only judge the root against finalized L2 state
                if game_created.l2_block_number > latest_finalized_l2_block {
                    return Ok(WatchOutcome::Keep {
                        challenge_deadline: watched.challenge_deadline,
                    });
                }
                let root = self
                    .consensus_provider
                    .output_root_at_block(game_created.l2_block_number)
                    .await?;
                if root == game_created.root_claim {
                    Ok(WatchOutcome::Defend)
                } else {
                    // invalid root, leave the game to the challenger
                    Ok(WatchOutcome::Drop)
                }
            }
            // the game already reached a terminal state
            RootState::None | RootState::Finalized | RootState::Invalidated => {
                Ok(WatchOutcome::Drop)
            }
        }
    }

    async fn scan_watched_games(
        &self,
        latest_finalized_l2_block: BlockNumber,
        now: u64,
    ) -> Vec<(WatchedGame, Result<WatchOutcome, DefenderError>)> {
        stream::iter(self.watched_games.values().copied().collect::<Vec<_>>())
            .map(|watched| async move {
                let result = self
                    .watch_game(&watched, latest_finalized_l2_block, now)
                    .await;
                (watched, result)
            })
            .buffer_unordered(self.config.max_game_concurrency)
            .collect()
            .await
    }

    fn handle_watch_outcomes(
        &mut self,
        results: Vec<(WatchedGame, Result<WatchOutcome, DefenderError>)>,
    ) {
        for (watched, result) in results {
            let game = watched.game_created.game;
            match result {
                Ok(WatchOutcome::Defend) => {
                    info!(%game, "challenged game has a valid root; starting defense");
                    self.watched_games.remove(&game);
                    self.active_defenses
                        .insert(game, ActiveDefense::new(watched.game_created));
                }
                Ok(WatchOutcome::Drop) => {
                    self.watched_games.remove(&game);
                }
                Ok(WatchOutcome::Keep { challenge_deadline }) => {
                    if let Some(watched) = self.watched_games.get_mut(&game) {
                        watched.challenge_deadline = challenge_deadline;
                    }
                }
                Err(err) => {
                    warn!(game = %game, error = %err, "game watch failed; retrying next tick");
                }
            }
        }
    }

    async fn advance_lane(
        &self,
        game_created: &GameCreated,
        lane: ProofLane,
        backend: ProofBackend,
        state: LaneState,
    ) -> LaneState {
        let game = game_created.game;
        match state {
            LaneState::Proven | LaneState::Abandoned => state,
            LaneState::Pending => {
                match self
                    .proof_requester
                    .request_proof(proof_request(game_created, backend))
                    .await
                {
                    Ok(id) => LaneState::Requested { id, attempts: 1 },
                    Err(error) => {
                        warn!(%game, ?lane, %error, "proof request failed; retrying next tick");
                        LaneState::Pending
                    }
                }
            }
            LaneState::Requested { id, attempts } => {
                let status = match self.proof_requester.proof_status(id).await {
                    Ok(status) => status,
                    Err(error) => {
                        warn!(%game, ?lane, %id, %error, "proof status check failed; retrying next tick");
                        return state;
                    }
                };
                match status {
                    ProofStatus::Queued | ProofStatus::InProgress => state,
                    ProofStatus::Completed => {
                        let response = match self.proof_requester.get_proof(id).await {
                            Ok(response) => response,
                            Err(error) => {
                                warn!(%game, ?lane, %id, %error, "proof retrieval failed; retrying next tick");
                                return state;
                            }
                        };
                        match self
                            .execution_provider
                            .submit_proof(game, lane as u8, encode_proof(&response.proof))
                            .await
                        {
                            Ok(submission) => {
                                info!(%game, ?lane, tx_hash = %submission.tx_hash, "proof lane submitted");
                                LaneState::Proven
                            }
                            Err(error) => {
                                // if the transaction actually landed, the
                                // proof bitmap check resolves the lane on the
                                // next tick
                                warn!(%game, ?lane, %error, "proof submission failed; retrying next tick");
                                state
                            }
                        }
                    }
                    ProofStatus::Failed => {
                        if attempts >= self.config.max_proof_attempts {
                            error!(%game, ?lane, attempts, "proving permanently failed; abandoning lane");
                            return LaneState::Abandoned;
                        }
                        // re-requesting a failed proof re-queues it
                        match self
                            .proof_requester
                            .request_proof(proof_request(game_created, backend))
                            .await
                        {
                            Ok(id) => LaneState::Requested {
                                id,
                                attempts: attempts + 1,
                            },
                            Err(error) => {
                                warn!(%game, ?lane, %error, "proof re-request failed; retrying next tick");
                                state
                            }
                        }
                    }
                }
            }
        }
    }

    async fn advance_defense(
        &self,
        defense: &ActiveDefense,
    ) -> Result<DefenseProgress, DefenderError> {
        let game_created = &defense.game_created;
        let game = game_created.game;
        if self.execution_provider.root_state(game).await? != RootState::Challenged {
            return Ok(DefenseProgress::Resolved);
        }
        let proof_bitmap = self.execution_provider.proof_bitmap(game).await?;

        let mut lanes = defense.lanes;
        for (slot, (lane, backend)) in DEFENDED_LANES.into_iter().enumerate() {
            // skip lanes already proven on-chain, by us or by anyone else
            if proof_bitmap & lane.mask() != 0 {
                lanes[slot] = LaneState::Proven;
                continue;
            }
            lanes[slot] = self
                .advance_lane(game_created, lane, backend, lanes[slot])
                .await;
        }
        Ok(DefenseProgress::Lanes(lanes))
    }

    async fn scan_active_defenses(
        &self,
    ) -> Vec<(ActiveDefense, Result<DefenseProgress, DefenderError>)> {
        stream::iter(self.active_defenses.values().copied().collect::<Vec<_>>())
            .map(|defense| async move {
                let result = self.advance_defense(&defense).await;
                (defense, result)
            })
            .buffer_unordered(self.config.max_game_concurrency)
            .collect()
            .await
    }

    fn handle_defense_progress(
        &mut self,
        results: Vec<(ActiveDefense, Result<DefenseProgress, DefenderError>)>,
    ) {
        for (defense, result) in results {
            let game = defense.game_created.game;
            match result {
                Ok(DefenseProgress::Resolved) => {
                    info!(%game, "game left the challenged state; defense closed");
                    self.active_defenses.remove(&game);
                }
                Ok(DefenseProgress::Lanes(lanes)) => {
                    if lanes.iter().all(|lane| *lane == LaneState::Proven) {
                        info!(%game, "all proof lanes submitted; defense completed");
                        self.active_defenses.remove(&game);
                    } else if lanes.iter().all(|lane| lane.is_terminal()) {
                        error!(%game, "defense abandoned without proving all lanes");
                        self.active_defenses.remove(&game);
                    } else if let Some(defense) = self.active_defenses.get_mut(&game) {
                        defense.lanes = lanes;
                    }
                }
                Err(err) => {
                    warn!(game = %game, error = %err, "defense scan failed; retrying next tick");
                }
            }
        }
    }

    pub async fn scan_once(&mut self) -> Result<(), DefenderError> {
        let target = self.execution_provider.finalized_l1_block_num().await?;
        let from = if self.cursor == 0 {
            target.saturating_sub(ONE_DAY_OF_L1_BLOCKS + MARGIN)
        } else {
            self.cursor
        };
        if from <= target {
            let games_created = self.execution_provider.games_created(from, target).await?;
            for game_created in games_created {
                let game = game_created.game;
                if self.watched_games.contains_key(&game)
                    || self.active_defenses.contains_key(&game)
                {
                    continue;
                }
                self.watched_games.insert(
                    game,
                    WatchedGame {
                        game_created,
                        challenge_deadline: None,
                    },
                );
            }
            // games are tracked in memory from here on, so the cursor can advance
            self.cursor = target + 1;
        }

        if self.watched_games.is_empty() && self.active_defenses.is_empty() {
            return Ok(());
        }

        let latest_finalized_l2_block = self.consensus_provider.latest_l2_finalized_block().await?;
        let now = unix_now();

        let watch_results = self
            .scan_watched_games(latest_finalized_l2_block, now)
            .await;
        self.handle_watch_outcomes(watch_results);

        let defense_results = self.scan_active_defenses().await;
        self.handle_defense_progress(defense_results);
        Ok(())
    }

    /// Runs the defender forever, logging transient failures and retrying on each tick.
    pub async fn run_forever(&mut self) -> Result<(), DefenderError> {
        self.config.validate()?;

        let mut interval = tokio::time::interval(self.config.poll_interval);
        loop {
            interval.tick().await;
            if let Err(e) = self.scan_once().await {
                warn!(%e, "scan attempt failed");
            }
        }
    }
}

/// Builds the proof request for one lane of a defended game.
fn proof_request(game_created: &GameCreated, backend: ProofBackend) -> ProofRequest {
    ProofRequest {
        backend,
        game: game_created.game,
        root_claim: game_created.root_claim,
        l2_block_number: game_created.l2_block_number,
        // pin the witness to the L1 origin committed at proposal time, so
        // the request id stays stable across defender restarts
        l1_head: game_created.l1_origin_hash,
    }
}

/// Encode a proof payload into the `bytes` argument of `submitProofLane`.
///
/// TODO: the on-chain proof calldata format is not defined yet. Replace this
/// placeholder encoding once the game contract specifies it.
fn encode_proof(proof: &ProofData) -> Bytes {
    match proof {
        ProofData::Sp1 {
            proof,
            public_values,
        } => [public_values.as_ref(), proof.as_ref()].concat().into(),
        ProofData::Nitro {
            attestation,
            signature,
        } => [attestation.as_ref(), signature.as_ref()].concat().into(),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs()
}
