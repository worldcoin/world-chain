use alloy_primitives::{Address, B256, TxHash};
use world_chain_proofs::ProofLane;
use world_chain_prover_service::{ProofBackend, ProofRequestId};

/// Immutable game data needed to monitor and defend an output-root claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameMetadata {
    pub address: Address,
    pub root_claim: B256,
    pub l2_block_number: u64,
    pub l1_origin_hash: B256,
    pub challenge_deadline: u64,
    pub proof_deadline: u64,
    pub proof_threshold: u8,
}

/// Result of a submitted `submitProofLane` transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefenderSubmission {
    /// Transaction hash for the challenge submission.
    pub tx_hash: TxHash,
}

/// Number of proof lanes the defender drives.
pub(crate) const DEFENDED_LANE_COUNT: usize = 2;

/// The proof lanes the defender drives, paired with the prover-service
/// backend that generates each proof.
pub(crate) const DEFENDED_LANES: [(ProofLane, ProofBackend); DEFENDED_LANE_COUNT] = [
    (ProofLane::ValidityProof, ProofBackend::Sp1),
    (ProofLane::TeeAttestation, ProofBackend::Nitro),
];

/// Result of watching a single game for one tick.
#[derive(Debug, Clone, Copy)]
pub(crate) enum WatchOutcome {
    /// Keep watching the game.
    Keep,
    /// The game was challenged and its root is valid: start a defense.
    Defend,
    /// The game no longer needs watching.
    Drop,
}

/// Progress of a single proof lane within an active defense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneState {
    /// The proof has not been requested yet.
    Pending,
    /// The proof was requested from the prover-service.
    Requested { id: ProofRequestId, attempts: u32 },
    /// The lane is proven on-chain.
    Proven,
    /// Proving permanently failed after exhausting all attempts.
    Abandoned,
}

impl LaneState {
    /// Whether the lane needs no further work.
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Proven | Self::Abandoned)
    }
}

/// An active defense of a challenged game with a valid root.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActiveDefense {
    pub game: GameMetadata,
    /// Lane progress, indexed like [`DEFENDED_LANES`].
    pub lanes: [LaneState; DEFENDED_LANE_COUNT],
}

impl ActiveDefense {
    pub(crate) const fn new(game: GameMetadata) -> Self {
        Self {
            game,
            lanes: [LaneState::Pending; DEFENDED_LANE_COUNT],
        }
    }
}

/// Result of advancing a single defense for one tick.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DefenseProgress {
    /// The game left the `Challenged` state on-chain.
    Closed,
    /// The game already has enough proof support to complete the defense.
    Complete,
    /// The proof deadline elapsed before the defense completed.
    DeadlineElapsed,
    /// Lane progress after this tick.
    Lanes([LaneState; DEFENDED_LANE_COUNT]),
}

/// Result of scanning a newly discovered allowlisted game.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GameScanOutcome {
    /// Retain the game for monitoring.
    Track,
    /// The game does not need defense monitoring.
    Skip,
}
