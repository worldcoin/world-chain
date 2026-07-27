use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
};

use alloy_primitives::{Address, B256, TxHash, U256};

/// Minimal immutable game data needed to validate an output-root claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameMetadata {
    pub address: Address,
    pub root_claim: B256,
    pub l2_block_number: u64,
}

/// Result of a submitted challenge transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChallengeSubmission {
    /// Transaction hash for the challenge submission.
    pub tx_hash: TxHash,
}

/// Result of a resolve transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveSubmission {
    /// Transaction hash for the resolution.
    pub tx_hash: TxHash,
}

/// Outcome of one bond-claim step against a game (two-phase DelayedWETH flow).
///
/// `MultiProofGame.claimCredit` closes the game and unlocks the credit on its first call, then
/// finalizes the DelayedWETH withdrawal on a later one; a single call is never enough to move
/// the funds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The game is not yet claimable (unresolved, inside the finality airgap, or inside the
    /// DelayedWETH withdrawal delay).
    NotReady,
    /// Phase 1 executed: credit unlocked in DelayedWETH.
    Unlocked {
        /// Transaction hash for the unlock submission.
        tx_hash: TxHash,
        /// Amount unlocked.
        amount: U256,
    },
    /// Phase 2 executed: funds withdrawn and transferred to the challenger.
    Claimed {
        /// Transaction hash for the withdrawal submission.
        tx_hash: TxHash,
        /// Amount transferred.
        amount: U256,
    },
    /// The game holds no credit for the challenger; nothing to claim now or later.
    NoCredit,
}

/// Challenger-owned games shared by scanning, resolution, and bond-management loops.
#[derive(Debug, Clone, Default)]
pub struct OwnedGames {
    games: Arc<RwLock<HashSet<Address>>>,
}

impl OwnedGames {
    /// Adds a game challenged by the managed challenger.
    pub fn insert(&self, game: Address) {
        self.games
            .write()
            .expect("owned-games lock poisoned")
            .insert(game);
    }

    /// Removes a game that no longer needs lifecycle management.
    pub fn remove(&self, game: Address) {
        self.games
            .write()
            .expect("owned-games lock poisoned")
            .remove(&game);
    }

    /// Returns whether a game is currently tracked.
    #[must_use]
    pub fn contains(&self, game: Address) -> bool {
        self.games
            .read()
            .expect("owned-games lock poisoned")
            .contains(&game)
    }

    /// Returns a snapshot suitable for asynchronous processing without holding the lock.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Address> {
        self.games
            .read()
            .expect("owned-games lock poisoned")
            .iter()
            .copied()
            .collect()
    }
}

/// A game queued for retry after a transient scan failure.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryGame {
    pub game: GameMetadata,
    pub challenge_deadline: Option<u64>,
    pub attempts: u32,
}

/// Result of processing a single game.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GameScanOutcome {
    Valid,
    NeedsChallenge { challenge_deadline: u64 },
    Skip,
}
