//! Local [`StateDB`] trait abstracting over `revm::database::State<DB>`.
//!
//! Upstream alloy-evm defines `StateDB` as a marker trait (`Database + DatabaseCommit`).
//! World-chain needs the richer version below so that custom database wrappers
//! ([`BalBuilderDb`], [`AsyncBalBuilderDb`], [`NoOpCommitDB`]) can be used
//! transparently wherever block execution accesses bundle state.
//!
//! [`BalBuilderDb`]: crate::database::bal_builder_db::BalBuilderDb
//! [`AsyncBalBuilderDb`]: crate::database::bal_builder_db::AsyncBalBuilderDb
//! [`NoOpCommitDB`]: crate::database::bal_builder_db::NoOpCommitDB

use revm::{
    Database, DatabaseCommit,
    database::{BundleState, states::bundle_state::BundleRetention},
};
use revm_database::State;

/// Abstraction over [`State<DB>`] that provides access to the accumulated
/// bundle state, transition merging, and state-clear configuration.
///
/// Any database wrapper that sits on top of a `State<DB>` can implement this
/// trait by delegating to the inner state, allowing the payload builder to
/// remain agnostic about the concrete database stack.
pub trait StateDB: Database + DatabaseCommit {
    /// Returns a shared reference to the current bundle state.
    fn bundle_state(&self) -> &BundleState;

    /// Returns a mutable reference to the current bundle state.
    fn bundle_state_mut(&mut self) -> &mut BundleState;

    /// Merges all recorded transitions into the bundle state with the given
    /// retention policy.
    fn merge_transitions(&mut self, retention: BundleRetention);

    /// Takes the bundle state out, replacing it with a default empty one.
    fn take_bundle(&mut self) -> BundleState;

    /// Sets the EIP-161 state-clear flag that determines whether empty accounts
    /// are removed during state transitions.
    fn set_state_clear_flag(&mut self, has_state_clear: bool);
}

// ---------------------------------------------------------------------------
// Blanket implementation for `revm::database::State<DB>`
// ---------------------------------------------------------------------------

impl<DB: Database> StateDB for State<DB> {
    fn bundle_state(&self) -> &BundleState {
        &self.bundle_state
    }

    fn bundle_state_mut(&mut self) -> &mut BundleState {
        &mut self.bundle_state
    }

    fn merge_transitions(&mut self, retention: BundleRetention) {
        self.merge_transitions(retention);
    }

    fn take_bundle(&mut self) -> BundleState {
        self.take_bundle()
    }

    fn set_state_clear_flag(&mut self, _has_state_clear: bool) {
        // revm-database v12 handles state clearing in the EVM journal.
    }
}

// ---------------------------------------------------------------------------
// Implementation for `&mut T` where `T: StateDB` (used when the EVM holds a borrow)
// ---------------------------------------------------------------------------

impl<T: StateDB> StateDB for &mut T {
    fn bundle_state(&self) -> &BundleState {
        T::bundle_state(self)
    }

    fn bundle_state_mut(&mut self) -> &mut BundleState {
        T::bundle_state_mut(self)
    }

    fn merge_transitions(&mut self, retention: BundleRetention) {
        T::merge_transitions(self, retention);
    }

    fn take_bundle(&mut self) -> BundleState {
        T::take_bundle(self)
    }

    fn set_state_clear_flag(&mut self, has_state_clear: bool) {
        T::set_state_clear_flag(self, has_state_clear);
    }
}
