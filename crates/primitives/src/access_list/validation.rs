use super::FlashblockAccessList;
use alloy_eip7928::constants::{
    ITEM_COST, MAX_ACCOUNTS, MAX_CODE_SIZE, MAX_SLOTS, MAX_TXS_PER_BLOCK,
};
use alloy_primitives::{Address, U256};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize)]
pub enum FlashblockAccessListValidationError {
    #[error("invalid BAL index range {min}..={max}")]
    InvalidIndexRange { min: u64, max: u64 },
    #[error("BAL {resource} count {count} exceeds limit {limit}")]
    ResourceLimit {
        resource: &'static str,
        count: usize,
        limit: usize,
    },
    #[error("BAL item count {items} exceeds gas-derived limit {limit}")]
    ItemLimit { items: u64, limit: u64 },
    #[error("BAL {field} entries for {address} must be strictly increasing and unique")]
    InvalidOrder {
        address: Address,
        field: &'static str,
    },
    #[error("BAL storage slot {slot} for {address} has no changes")]
    EmptyStorageChanges { address: Address, slot: U256 },
    #[error("BAL storage slot {slot} for {address} appears in both reads and writes")]
    StorageReadWriteOverlap { address: Address, slot: U256 },
    #[error("BAL {field} index {index} for {address} is outside {min}..={max}")]
    ChangeIndexOutOfRange {
        address: Address,
        field: &'static str,
        index: u64,
        min: u64,
        max: u64,
    },
    #[error("BAL code at index {index} for {address} has {size} bytes, exceeding {MAX_CODE_SIZE}")]
    CodeTooLarge {
        address: Address,
        index: u64,
        size: usize,
    },
}

impl FlashblockAccessList {
    /// Checks canonical structure and pinned EIP-7928 resource limits without allocating.
    pub fn validate(
        &self,
        block_gas_limit: u64,
    ) -> Result<(), FlashblockAccessListValidationError> {
        use FlashblockAccessListValidationError as Error;

        if self.min_tx_index > self.max_tx_index || self.max_tx_index > MAX_TXS_PER_BLOCK as u64 + 1
        {
            return Err(Error::InvalidIndexRange {
                min: self.min_tx_index,
                max: self.max_tx_index,
            });
        }
        check_resource_limit("accounts", self.changes.len(), MAX_ACCOUNTS)?;

        // Equivalent to Alloy's validate_gas_limit, without cloning into its owned Bal wrapper.
        let item_limit = block_gas_limit / ITEM_COST as u64;
        let mut items = self.changes.len() as u64;
        let mut slots = 0usize;
        for account in &self.changes {
            slots = slots
                .saturating_add(account.storage_changes.len())
                .saturating_add(account.storage_reads.len());
            check_resource_limit("storage slots", slots, MAX_SLOTS)?;
        }
        items = items.saturating_add(slots as u64);
        if items > item_limit {
            return Err(Error::ItemLimit {
                items,
                limit: item_limit,
            });
        }

        let mut previous_address = None;
        for account in &self.changes {
            let address = account.address;
            if previous_address.is_some_and(|previous| previous >= address) {
                return Err(Error::InvalidOrder {
                    address,
                    field: "accounts",
                });
            }
            previous_address = Some(address);

            if account
                .storage_changes
                .windows(2)
                .any(|pair| pair[0].slot >= pair[1].slot)
            {
                return Err(Error::InvalidOrder {
                    address,
                    field: "storage writes",
                });
            }
            if account
                .storage_reads
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            {
                return Err(Error::InvalidOrder {
                    address,
                    field: "storage reads",
                });
            }

            let mut writes = account.storage_changes.iter().peekable();
            for slot in &account.storage_reads {
                while writes.peek().is_some_and(|write| write.slot < *slot) {
                    writes.next();
                }
                if writes.peek().is_some_and(|write| write.slot == *slot) {
                    return Err(Error::StorageReadWriteOverlap {
                        address,
                        slot: *slot,
                    });
                }
            }

            for slot in &account.storage_changes {
                if slot.changes.is_empty() {
                    return Err(Error::EmptyStorageChanges {
                        address,
                        slot: slot.slot,
                    });
                }
                self.validate_indices(address, "storage", &slot.changes, |change| {
                    change.block_access_index.0
                })?;
            }
            self.validate_indices(address, "balance", &account.balance_changes, |change| {
                change.block_access_index.0
            })?;
            self.validate_indices(address, "nonce", &account.nonce_changes, |change| {
                change.block_access_index.0
            })?;
            self.validate_indices(address, "code", &account.code_changes, |change| {
                change.block_access_index.0
            })?;
            for change in &account.code_changes {
                if change.new_code.len() > MAX_CODE_SIZE {
                    return Err(Error::CodeTooLarge {
                        address,
                        index: change.block_access_index.0,
                        size: change.new_code.len(),
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_indices<T>(
        &self,
        address: Address,
        field: &'static str,
        changes: &[T],
        index: impl Fn(&T) -> u64,
    ) -> Result<(), FlashblockAccessListValidationError> {
        let mut previous = None;
        for change in changes {
            let index = index(change);
            if index < self.min_tx_index || index > self.max_tx_index {
                return Err(FlashblockAccessListValidationError::ChangeIndexOutOfRange {
                    address,
                    field,
                    index,
                    min: self.min_tx_index,
                    max: self.max_tx_index,
                });
            }
            if previous.is_some_and(|previous| previous >= index) {
                return Err(FlashblockAccessListValidationError::InvalidOrder { address, field });
            }
            previous = Some(index);
        }
        Ok(())
    }
}

fn check_resource_limit(
    resource: &'static str,
    count: usize,
    limit: usize,
) -> Result<(), FlashblockAccessListValidationError> {
    if count > limit {
        return Err(FlashblockAccessListValidationError::ResourceLimit {
            resource,
            count,
            limit,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eip7928::{
        AccountChanges, BalanceChange, BlockAccessIndex, CodeChange, NonceChange, SlotChanges,
        StorageChange,
    };
    use alloy_primitives::Bytes;

    const GAS_LIMIT: u64 = 1_000_000;

    fn list(account: AccountChanges) -> FlashblockAccessList {
        FlashblockAccessList::from_block_access_list(vec![account], (0, 2))
    }

    fn account() -> AccountChanges {
        AccountChanges::new(Address::with_last_byte(1))
    }

    fn storage(slot: u64, index: u64) -> SlotChanges {
        SlotChanges::new(
            U256::from(slot),
            vec![StorageChange::new(
                BlockAccessIndex::new(index),
                U256::from(1),
            )],
        )
    }

    #[test]
    fn accepts_read_only_accounts_and_zeroing_writes() {
        let access_list = FlashblockAccessList::from_block_access_list(
            vec![
                account(),
                AccountChanges::new(Address::with_last_byte(2))
                    .with_storage_read(U256::from(1))
                    .with_storage_change(SlotChanges::new(
                        U256::from(2),
                        vec![StorageChange::new(BlockAccessIndex::new(1), U256::ZERO)],
                    ))
                    .with_balance_change(BalanceChange::new(BlockAccessIndex::new(2), U256::ZERO)),
            ],
            (0, 2),
        );
        assert_eq!(access_list.validate(GAS_LIMIT), Ok(()));
    }

    #[test]
    fn accepts_changes_at_both_flashblock_boundaries() {
        let mut access_list = list(
            account()
                .with_nonce_change(NonceChange::new(BlockAccessIndex::new(1), 1))
                .with_nonce_change(NonceChange::new(BlockAccessIndex::new(3), 2)),
        );
        access_list.min_tx_index = 1;
        access_list.max_tx_index = 3;
        assert_eq!(access_list.validate(GAS_LIMIT), Ok(()));
    }

    #[test]
    fn rejects_reversed_or_unbounded_flashblock_ranges() {
        for (min, max) in [(2, 1), (0, MAX_TXS_PER_BLOCK as u64 + 2), (0, u64::MAX)] {
            let access_list = FlashblockAccessList::from_block_access_list(vec![], (min, max));
            assert_eq!(
                access_list.validate(GAS_LIMIT),
                Err(FlashblockAccessListValidationError::InvalidIndexRange { min, max })
            );
        }
    }

    #[test]
    fn rejects_duplicate_and_unordered_accounts() {
        for next_address in [Address::with_last_byte(1), Address::ZERO] {
            let access_list = FlashblockAccessList::from_block_access_list(
                vec![account(), AccountChanges::new(next_address)],
                (0, 2),
            );
            assert_eq!(
                access_list.validate(GAS_LIMIT),
                Err(FlashblockAccessListValidationError::InvalidOrder {
                    address: next_address,
                    field: "accounts",
                })
            );
        }
    }

    #[test]
    fn rejects_duplicate_and_unordered_storage_writes() {
        for next_slot in [2, 1] {
            let access_list = list(
                account()
                    .with_storage_change(storage(2, 1))
                    .with_storage_change(storage(next_slot, 1)),
            );
            assert!(matches!(
                access_list.validate(GAS_LIMIT),
                Err(FlashblockAccessListValidationError::InvalidOrder {
                    field: "storage writes",
                    ..
                })
            ));
        }
    }

    #[test]
    fn rejects_duplicate_and_unordered_storage_reads() {
        for next_slot in [2, 1] {
            let access_list = list(
                account()
                    .with_storage_read(U256::from(2))
                    .with_storage_read(U256::from(next_slot)),
            );
            assert!(matches!(
                access_list.validate(GAS_LIMIT),
                Err(FlashblockAccessListValidationError::InvalidOrder {
                    field: "storage reads",
                    ..
                })
            ));
        }
    }

    #[test]
    fn rejects_storage_slots_in_both_reads_and_writes() {
        let access_list = list(
            account()
                .with_storage_change(storage(2, 1))
                .with_storage_change(storage(4, 1))
                .with_storage_read(U256::from(1))
                .with_storage_read(U256::from(4)),
        );
        assert_eq!(
            access_list.validate(GAS_LIMIT),
            Err(
                FlashblockAccessListValidationError::StorageReadWriteOverlap {
                    address: account().address,
                    slot: U256::from(4),
                }
            )
        );
    }

    #[test]
    fn rejects_empty_storage_change_lists() {
        let access_list =
            list(account().with_storage_change(SlotChanges::new(U256::from(1), vec![])));
        assert_eq!(
            access_list.validate(GAS_LIMIT),
            Err(FlashblockAccessListValidationError::EmptyStorageChanges {
                address: account().address,
                slot: U256::from(1),
            })
        );
    }

    #[test]
    fn rejects_duplicate_and_unordered_indices_in_every_change_kind() {
        for next_index in [1, 0] {
            let first = BlockAccessIndex::new(1);
            let next = BlockAccessIndex::new(next_index);
            let accounts = [
                (
                    "storage",
                    account().with_storage_change(SlotChanges::new(
                        U256::from(1),
                        vec![
                            StorageChange::new(first, U256::from(1)),
                            StorageChange::new(next, U256::from(2)),
                        ],
                    )),
                ),
                (
                    "balance",
                    account()
                        .with_balance_change(BalanceChange::new(first, U256::from(1)))
                        .with_balance_change(BalanceChange::new(next, U256::from(2))),
                ),
                (
                    "nonce",
                    account()
                        .with_nonce_change(NonceChange::new(first, 1))
                        .with_nonce_change(NonceChange::new(next, 2)),
                ),
                (
                    "code",
                    account()
                        .with_code_change(CodeChange::new(first, Bytes::from_static(&[1])))
                        .with_code_change(CodeChange::new(next, Bytes::from_static(&[2]))),
                ),
            ];
            for (field, account) in accounts {
                assert_eq!(
                    list(account).validate(GAS_LIMIT),
                    Err(FlashblockAccessListValidationError::InvalidOrder {
                        address: Address::with_last_byte(1),
                        field,
                    })
                );
            }
        }
    }

    #[test]
    fn rejects_indices_outside_the_flashblock_in_every_change_kind() {
        for index in [0, 3, u64::MAX] {
            let block_index = BlockAccessIndex::new(index);
            let accounts = [
                ("storage", account().with_storage_change(storage(1, index))),
                (
                    "balance",
                    account().with_balance_change(BalanceChange::new(block_index, U256::from(1))),
                ),
                (
                    "nonce",
                    account().with_nonce_change(NonceChange::new(block_index, 1)),
                ),
                (
                    "code",
                    account()
                        .with_code_change(CodeChange::new(block_index, Bytes::from_static(&[1]))),
                ),
            ];
            for (field, account) in accounts {
                let mut access_list = list(account);
                access_list.min_tx_index = 1;
                assert_eq!(
                    access_list.validate(GAS_LIMIT),
                    Err(FlashblockAccessListValidationError::ChangeIndexOutOfRange {
                        address: Address::with_last_byte(1),
                        field,
                        index,
                        min: 1,
                        max: 2,
                    })
                );
            }
        }
    }

    #[test]
    fn accepts_maximum_code_size_and_rejects_one_extra_byte() {
        let mut access_list = list(account().with_code_change(CodeChange::new(
            BlockAccessIndex::new(1),
            vec![0; MAX_CODE_SIZE].into(),
        )));
        assert_eq!(access_list.validate(GAS_LIMIT), Ok(()));
        access_list.changes[0].code_changes[0].new_code = vec![0; MAX_CODE_SIZE + 1].into();
        assert_eq!(
            access_list.validate(GAS_LIMIT),
            Err(FlashblockAccessListValidationError::CodeTooLarge {
                address: account().address,
                index: 1,
                size: MAX_CODE_SIZE + 1,
            })
        );
    }

    #[test]
    fn item_cost_matches_alloy_and_accepts_the_exact_limit() {
        let access_list = list(
            account()
                .with_storage_read(U256::from(1))
                .with_storage_change(storage(2, 1)),
        );
        let alloy_bal =
            alloy_eip7928::block_access_list::bal::Bal::new(access_list.as_block_access_list());
        let exact_gas_limit = 3 * ITEM_COST as u64;
        assert_eq!(access_list.validate(exact_gas_limit), Ok(()));
        assert_eq!(alloy_bal.validate_gas_limit(exact_gas_limit), Ok(()));
        assert_eq!(
            access_list.validate(exact_gas_limit - 1),
            Err(FlashblockAccessListValidationError::ItemLimit { items: 3, limit: 2 })
        );
        assert!(alloy_bal.validate_gas_limit(exact_gas_limit - 1).is_err());
    }

    #[test]
    fn rejects_excessive_accounts_before_walking_them() {
        let access_list =
            FlashblockAccessList::from_block_access_list(vec![account(); MAX_ACCOUNTS + 1], (0, 2));
        assert_eq!(
            access_list.validate(u64::MAX),
            Err(FlashblockAccessListValidationError::ResourceLimit {
                resource: "accounts",
                count: MAX_ACCOUNTS + 1,
                limit: MAX_ACCOUNTS,
            })
        );
    }

    #[test]
    fn rejects_excessive_slots_before_walking_them() {
        let mut account = account();
        account.storage_reads = vec![U256::ZERO; MAX_SLOTS + 1];
        assert_eq!(
            list(account).validate(u64::MAX),
            Err(FlashblockAccessListValidationError::ResourceLimit {
                resource: "storage slots",
                count: MAX_SLOTS + 1,
                limit: MAX_SLOTS,
            })
        );
    }
}
