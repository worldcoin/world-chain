use std::collections::{HashMap, hash_map::Entry};

use revm_database::states::reverts::{AccountInfoRevert, Reverts};

/// Flattens a multi-transition [`Reverts`] into a single transition, merging per-account data.
///
/// Merge rules (iterate earliest -> latest):
/// - For each account, keep the **earliest** `previous_status`.
/// - For each account, keep the **earliest non-`DoNothing`** account-info revert.
/// - For each account+slot, keep the **earliest** `RevertToSlot`.
/// - For each account, OR `wipe_storage`.
pub(crate) fn flatten_reverts(reverts: &Reverts) -> Reverts {
    let mut per_account = HashMap::new();

    for (addr, acc_revert) in reverts.iter().flatten() {
        match per_account.entry(*addr) {
            Entry::Vacant(v) => {
                v.insert(acc_revert.clone());
            }
            Entry::Occupied(mut o) => {
                let entry = o.get_mut();

                // Always OR wipe_storage (if any transition wiped storage, the block-level revert
                // must reflect it).
                entry.wipe_storage |= acc_revert.wipe_storage;

                // Merge storage: keep earliest revert-to value per slot.
                for (slot, revert_to) in &acc_revert.storage {
                    entry.storage.entry(*slot).or_insert(*revert_to);
                }

                // Merge account-info revert: keep earliest non-DoNothing.
                if matches!(entry.account, AccountInfoRevert::DoNothing)
                    && !matches!(acc_revert.account, AccountInfoRevert::DoNothing)
                {
                    entry.account = acc_revert.account.clone();
                }

                // Keep earliest previous_status: do not overwrite.
            }
        }
    }

    // Transform the map into a vec
    let flattened = per_account.into_iter().collect();
    Reverts::new(vec![flattened])
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};
    use revm::{
        database::{
            AccountRevert, AccountStatus, RevertToSlot,
            states::reverts::{AccountInfoRevert, Reverts},
        },
        state::AccountInfo,
    };

    use crate::utils::flatten_reverts;

    #[bon::builder]
    fn revert(
        #[builder(start_fn)] status: AccountStatus,
        account: Option<AccountInfoRevert>,
        #[builder(into, default)] slots: Vec<(U256, U256)>,
        #[builder(default)] wipe_storage: bool,
    ) -> AccountRevert {
        AccountRevert {
            account: account.unwrap_or(AccountInfoRevert::DoNothing),
            storage: slots
                .iter()
                .map(|(k, v)| (*k, RevertToSlot::Some(*v)))
                .collect(),
            previous_status: status,
            wipe_storage,
        }
    }

    #[test]
    fn test_flatten_reverts_different_storage_slots() {
        let addr = Address::with_last_byte(1);
        let slot_0 = U256::ZERO;
        let slot_1 = U256::from(1);

        // Frame 1: slot_0 reverts to 1
        let first = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .slots([(slot_0, U256::from(1))])
                .call(),
        )];

        // Frame 2: slot_0 reverts to 2 (should be ignored), slot_1 reverts to 1 (new, kept)
        let second = vec![(
            addr,
            revert(AccountStatus::InMemoryChange)
                .slots([(slot_0, U256::from(2)), (slot_1, U256::from(1))])
                .call(),
        )];

        let reverts = Reverts::new(vec![first, second]);
        let flattened = flatten_reverts(&reverts);

        assert_eq!(flattened.len(), 1);
        let (actual_addr, actual_revert) = flattened[0][0].clone();

        assert_eq!(actual_addr, addr);
        // slot_0 keeps first frame's value (1), slot_1 added from second frame
        let expected = revert(AccountStatus::Loaded)
            .slots([(slot_0, U256::from(1)), (slot_1, U256::from(1))])
            .call();
        assert_eq!(actual_revert, expected);
    }

    #[test]
    fn test_flatten_reverts_different_account_info() {
        let addr = Address::with_last_byte(1);
        let prev_acc_info = AccountInfo::default();

        // Frame 1: DoNothing
        let first = vec![(addr, revert(AccountStatus::Loaded).call())];

        // Frame 2: RevertTo (first non-DoNothing, should be kept)
        let second = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .account(AccountInfoRevert::RevertTo(prev_acc_info.clone()))
                .call(),
        )];

        // Frame 3: DeleteIt (should be ignored, already have non-DoNothing)
        let third = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .account(AccountInfoRevert::DeleteIt)
                .call(),
        )];

        let reverts = Reverts::new(vec![first, second, third]);
        let flattened = flatten_reverts(&reverts);

        assert_eq!(flattened.len(), 1);
        let (actual_addr, actual_revert) = flattened[0][0].clone();

        assert_eq!(actual_addr, addr);
        // Should keep RevertTo from frame 2 (first non-DoNothing)
        let expected = revert(AccountStatus::Loaded)
            .account(AccountInfoRevert::RevertTo(prev_acc_info))
            .call();
        assert_eq!(actual_revert, expected);
    }

    #[test]
    fn test_flatten_reverts_wipe_storage() {
        let addr = Address::with_last_byte(1);
        let prev_acc_info = AccountInfo::default();

        // Frame 1: wipe_storage = false (default)
        let first = vec![(addr, revert(AccountStatus::Loaded).call())];

        // Frame 2: wipe_storage = true (should be kept - sticky)
        let second = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .account(AccountInfoRevert::RevertTo(prev_acc_info.clone()))
                .wipe_storage(true)
                .call(),
        )];

        // Frame 3: wipe_storage = false (should be ignored, already true)
        let third = vec![(
            addr,
            revert(AccountStatus::Loaded)
                .account(AccountInfoRevert::DeleteIt)
                .call(),
        )];

        let reverts = Reverts::new(vec![first, second, third]);
        let flattened = flatten_reverts(&reverts);

        assert_eq!(flattened.len(), 1);
        let (actual_addr, actual_revert) = flattened[0][0].clone();

        assert_eq!(actual_addr, addr);
        // wipe_storage should remain true (sticky once set)
        let expected = revert(AccountStatus::Loaded)
            .account(AccountInfoRevert::RevertTo(prev_acc_info))
            .wipe_storage(true)
            .call();
        assert_eq!(actual_revert, expected);
    }
}
