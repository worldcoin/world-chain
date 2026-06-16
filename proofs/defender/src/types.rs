use alloy_primitives::TxHash;
use world_chain_proofs::{GameCreated, ProofLane};
use world_chain_prover_service::{ProofBackend, ProofRequestId};

/// Result of a submitted `submitProofLane`` transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefenderSubmission {
    /// Transaction hash for the challenge submission.
    pub tx_hash: TxHash,
}

/// Number of proof lanes the defender drives.
///
/// Currently SP1-only: the defender defends a challenged game with a single
/// validity proof. This matches the SP1-only devnet, where the proof system is
/// deployed with `proofThreshold = 1`. Re-add the TEE lane here once a Nitro
/// worker exists to service the `Nitro` backend queue.
pub(crate) const DEFENDED_LANE_COUNT: usize = 1;

/// The proof lanes the defender drives, paired with the prover-service
/// backend that generates each proof.
pub(crate) const DEFENDED_LANES: [(ProofLane, ProofBackend); DEFENDED_LANE_COUNT] =
    [(ProofLane::ValidityProof, ProofBackend::Sp1)];

/// A game watched until it leaves the `Proposed` state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WatchedGame {
    pub game_created: GameCreated,
    /// Cached challenge deadline, fetched lazily on the first watch tick.
    pub challenge_deadline: Option<u64>,
}

/// Result of watching a single game for one tick.
#[derive(Debug, Clone, Copy)]
pub(crate) enum WatchOutcome {
    /// Keep watching the game.
    Keep { challenge_deadline: Option<u64> },
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
    pub game_created: GameCreated,
    /// Lane progress, indexed like [`DEFENDED_LANES`].
    pub lanes: [LaneState; DEFENDED_LANE_COUNT],
}

impl ActiveDefense {
    pub(crate) const fn new(game_created: GameCreated) -> Self {
        Self {
            game_created,
            lanes: [LaneState::Pending; DEFENDED_LANE_COUNT],
        }
    }
}

/// Result of advancing a single defense for one tick.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DefenseProgress {
    /// The game left the `Challenged` state on-chain.
    Resolved,
    /// Lane progress after this tick.
    Lanes([LaneState; DEFENDED_LANE_COUNT]),
}
