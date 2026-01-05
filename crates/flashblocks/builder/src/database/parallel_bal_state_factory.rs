use std::sync::Arc;

use revm::DatabaseRef;
use revm_database::{BundleState, State, WrapDatabaseRef};

use super::temporal_db::{TemporalDb, TemporalDbFactory};

/// Factory for creating [`State`] instances with BAL construction for parallel validation.
///
/// This factory creates isolated [`State`] instances that can be used in parallel
/// to execute transactions independently, each with their own BAL (Block Access List)
/// builder enabled.
#[derive(Clone, Debug)]
pub struct ParallelBalStateFactory<DB: DatabaseRef + Clone> {
    /// The underlying temporal database factory for creating time-indexed views.
    temporal_factory: TemporalDbFactory<DB>,
    /// The bundle state to preload (pre-state from prior flashblocks).
    bundle_state: Arc<BundleState>,
}

impl<DB: DatabaseRef + Clone> ParallelBalStateFactory<DB> {
    /// Creates a new factory with the given temporal database factory and bundle state.
    pub fn new(temporal_factory: TemporalDbFactory<DB>, bundle_state: Arc<BundleState>) -> Self {
        Self {
            temporal_factory,
            bundle_state,
        }
    }

    /// Creates a [`State`] at a specific transaction index with BAL construction enabled.
    ///
    /// Each [`State`] instance:
    /// - Has a time-indexed view of the database via [`TemporalDb`] wrapped in [`WrapDatabaseRef`]
    /// - Does NOT use bundle prestate (to ensure TemporalDb is used for correct per-index values)
    /// - Has BAL construction enabled via `with_bal_builder()`
    ///
    /// IMPORTANT: We don't use bundle_prestate here because the bundle contains FINAL values
    /// from extend_bundle, not the per-transaction values. The TemporalDb provides the correct
    /// state at each transaction index, so we let the State read from it directly.
    ///
    /// The returned [`State`] is ready for independent parallel execution.
    pub fn state_at(&self, index: u64) -> State<WrapDatabaseRef<TemporalDb<DB>>> {
        let temporal_db = self.temporal_factory.db(index);
        // Wrap TemporalDb (DatabaseRef) in WrapDatabaseRef to provide Database trait
        let db = WrapDatabaseRef(temporal_db);

        State::builder()
            .with_database(db)
            // Note: We intentionally don't use with_bundle_prestate here because
            // the bundle has final values, not per-index values. The TemporalDb
            // provides the correct state at each transaction index.
            .with_bundle_update()
            .with_bal_builder()
            .build()
    }

    /// Returns a reference to the bundle state.
    pub fn bundle_state(&self) -> &Arc<BundleState> {
        &self.bundle_state
    }
}
