//! EVM Inspector to aggregate a Flashblock Access List

use std::collections::HashMap;

use alloy_eip7928::{AccountChanges, BalanceChange, SlotChanges, StorageChange};
use alloy_primitives::{Address, StorageKey, B256, U256};
use reth_primitives::Bytecode;
use revm::{
    bytecode::opcode::{SLOAD, SSTORE},
    context::ContextTr,
    interpreter::{
        interpreter::EthInterpreter,
        interpreter_types::{InputsTr, Jumps},
        Interpreter,
    },
    state::EvmState,
    Inspector,
};

pub struct BalInspector {
    storage_writes: HashMap<Address, HashMap<B256, U256>>,
    storage_reads: HashMap<Address, Vec<B256>>,
    index: u64,
}

impl BalInspector {
    pub fn new() -> Self {
        Self {
            storage_writes: HashMap::new(),
            storage_reads: HashMap::new(),
            index: 0,
        }
    }

    pub fn set_index(&mut self, index: u64) {
        self.index = index;
    }

    pub fn merge_state(&self, state: EvmState) -> Vec<AccountChanges> {
        let mut changes = HashMap::new();
        for (account, state) in state.into_iter() {
            let entry = changes
                .entry(account)
                .or_insert_with(|| AccountChanges::new(account));

            let balance_change = BalanceChange {
                block_access_index: self.index,
                post_balance: state.info.balance,
            };

            entry.balance_changes.push(balance_change);
        }

        self.storage_writes
            .keys()
            .chain(self.storage_reads.keys())
            .cloned()
            .for_each(|address| {
                let writes = self.storage_writes.get(&address);
                let reads = self.storage_reads.get(&address);
                let account_changes = changes
                    .entry(address)
                    .or_insert_with(|| AccountChanges::new(address));

                if let Some(writes) = writes {
                    for (slot, value) in writes.into_iter() {
                        let slot_change = SlotChanges::new(
                            *slot,
                            vec![StorageChange {
                                block_access_index: self.index,
                                new_value: value.clone().into(),
                            }],
                        );

                        account_changes.storage_changes.push(slot_change);
                    }
                }

                if let Some(reads) = reads {
                    for &slot in reads {
                        if writes.map_or(true, |w| !w.contains_key(&slot)) {
                            account_changes.storage_reads.push(slot);
                            account_changes.storage_reads.sort_unstable();
                        }
                    }
                }
            });

        changes.into_values().collect()
    }
}

impl<CTX: ContextTr> Inspector<CTX> for BalInspector {
    fn step(&mut self, interp: &mut Interpreter<EthInterpreter>, _context: &mut CTX) {
        let op_code = interp.bytecode.opcode();
        let contract = interp.input.target_address();
        match op_code {
            SLOAD => {
                if let Ok(val) = interp.stack.peek(0) {
                    let writes = self.storage_writes.get(&contract);
                    // only record the read if we haven't written to it in this tx
                    if !writes.map_or(false, |w| w.contains_key(&B256::from(val))) {
                        self.storage_reads
                            .entry(contract)
                            .or_insert_with(|| vec![])
                            .push(val.into());
                    }
                }
            }
            SSTORE => {
                if let (Ok(slot), Ok(value)) = (interp.stack.peek(0), interp.stack.peek(1)) {
                    // always keep the last write to a slot
                    self.storage_writes
                        .entry(contract)
                        .or_insert_with(|| HashMap::new())
                        .insert(slot.into(), value);
                }
            }
            _ => {}
        }
    }
}
