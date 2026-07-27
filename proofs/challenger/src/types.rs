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

/// Result of a withdraw transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawSubmission {
    /// Transaction hash for the withdrawal.
    pub tx_hash: TxHash,
    /// Amount withdrawn for the challenger.
    pub amount: U256,
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
