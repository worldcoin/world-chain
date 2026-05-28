//! Sync actions: compute what the batcher should do this cycle from the local
//! L2 heads, with no rollup-node RPC.
//!
//! Mirrors `op-batcher/batcher/sync_actions.go::computeSyncActions`, re-sourced
//! from [`LocalHeads`] (see `BATCHER_SPEC.md` §6). The restart-recovery property
//! is preserved: with no persisted cursor, the resume point is derived purely
//! from `(safe_l2, unsafe_l2)`.

use std::ops::RangeInclusive;

use crate::source::LocalHeads;

/// What the driver should do this cycle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncActions {
    /// Prune buffered blocks at or below this safe block number.
    pub prune_safe: Option<u64>,
    /// Inclusive L2 block range to load into the channel manager.
    pub blocks_to_load: Option<RangeInclusive<u64>>,
    /// Whether the local heads were inconsistent (skip this cycle).
    pub out_of_sync: bool,
}

/// Compute the actions for this cycle.
///
/// * `heads` — the local unsafe/safe heads.
/// * `newest_in_state` — the highest L2 block currently buffered in the channel
///   manager (`None` if empty — e.g. after a restart or a `clear`).
pub fn compute_sync_actions(heads: &LocalHeads, newest_in_state: Option<u64>) -> SyncActions {
    let safe = heads.safe_l2;
    let unsafe_head = heads.unsafe_l2;

    // Guard: unsafe head behind safe head is nonsensical — skip.
    if unsafe_head < safe {
        return SyncActions {
            out_of_sync: true,
            ..Default::default()
        };
    }

    let prune_safe = Some(safe);

    // Nothing unsafe to batch.
    if unsafe_head == safe {
        return SyncActions {
            prune_safe,
            blocks_to_load: None,
            out_of_sync: false,
        };
    }

    // Resume point: continue after the newest block already in state, but never
    // below the safe head (those blocks are already derived). With no blocks in
    // state (restart / clear), this loads the full unsafe range — the
    // no-persisted-cursor recovery path.
    let start = match newest_in_state {
        Some(newest) => newest.saturating_add(1).max(safe.saturating_add(1)),
        None => safe.saturating_add(1),
    };

    let blocks_to_load = if start <= unsafe_head {
        Some(start..=unsafe_head)
    } else {
        None
    };

    SyncActions {
        prune_safe,
        blocks_to_load,
        out_of_sync: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heads(safe: u64, unsafe_head: u64) -> LocalHeads {
        LocalHeads {
            unsafe_l2: unsafe_head,
            safe_l2: safe,
            ..Default::default()
        }
    }

    #[test]
    fn no_blocks_in_state_loads_full_unsafe_range() {
        let a = compute_sync_actions(&heads(100, 110), None);
        assert_eq!(a.blocks_to_load, Some(101..=110));
        assert_eq!(a.prune_safe, Some(100));
        assert!(!a.out_of_sync);
    }

    #[test]
    fn happy_path_loads_after_newest() {
        let a = compute_sync_actions(&heads(100, 110), Some(105));
        assert_eq!(a.blocks_to_load, Some(106..=110));
    }

    #[test]
    fn caught_up_loads_nothing() {
        let a = compute_sync_actions(&heads(100, 110), Some(110));
        assert_eq!(a.blocks_to_load, None);
        assert_eq!(a.prune_safe, Some(100));
    }

    #[test]
    fn nothing_unsafe() {
        let a = compute_sync_actions(&heads(110, 110), None);
        assert_eq!(a.blocks_to_load, None);
    }

    #[test]
    fn unsafe_behind_safe_is_out_of_sync() {
        let a = compute_sync_actions(&heads(110, 100), None);
        assert!(a.out_of_sync);
    }

    #[test]
    fn safe_ahead_of_state_skips_gap() {
        // State had block 105 but safe jumped to 108 (those are derived).
        let a = compute_sync_actions(&heads(108, 112), Some(105));
        assert_eq!(a.blocks_to_load, Some(109..=112));
    }
}
