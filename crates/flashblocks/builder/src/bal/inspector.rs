//! EVM Inspector to aggregate a Block Access List per EIP-7928

use std::collections::{HashMap, HashSet};

use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{Address, Bytes, B256, U256};

use revm::{
    bytecode::opcode::{BALANCE, EXTCODECOPY, EXTCODEHASH, EXTCODESIZE, SLOAD, SSTORE},
    context::{ContextTr, JournalTr},
    interpreter::{
        interpreter::EthInterpreter,
        interpreter_types::{InputsTr, Jumps},
        CallInputs, CallOutcome, CreateInputs, CreateOutcome, Interpreter,
    },
    state::EvmState,
    Inspector,
};

/// Inspector that tracks all state accesses and changes according to EIP-7928.
///
/// This inspector records:
/// - All accessed addresses (even if unchanged)
/// - Storage reads (SLOAD without write, or SSTORE with unchanged value)
/// - Storage writes (actual value changes including zeroing)
/// - Balance changes (when balance actually changes)
/// - Nonce changes (for senders, deployers, deployed contracts, EIP-7702 authorities)
/// - Code changes (for deployed/modified contracts)
pub struct BalInspector {
    /// Storage slots written to by address and slot.
    /// Maps: Address -> Slot -> (pre_value, new_value)
    storage_writes: HashMap<Address, HashMap<B256, (U256, U256)>>,

    /// Storage slots read from (SLOAD or no-op SSTORE).
    /// Maps: Address -> Set of slots
    storage_reads: HashMap<Address, HashSet<B256>>,

    /// All addresses accessed during execution (even if no state changes).
    accessed_addresses: HashSet<Address>,

    /// Code changes
    /// Maps: Address -> Code
    code_changes: HashMap<Address, Bytes>,

    /// Maps: Address -> Balance
    // Post-execution balance changes
    balance_changes: HashMap<Address, U256>,

    /// Pre-execution state for comparison to detect actual changes.
    /// Maps: Address -> (pre_balance, pre_nonce, pre_code)
    pre_state: HashMap<Address, (U256, u64, Option<Bytes>)>,

    /// Current block access index (0 for pre-execution, 1..n for transactions, n+1 for post-execution).
    index: u64,
}

impl BalInspector {
    /// Creates a new BAL inspector.
    pub fn new() -> Self {
        Self {
            storage_writes: HashMap::new(),
            storage_reads: HashMap::new(),
            accessed_addresses: HashSet::new(),
            code_changes: HashMap::new(),
            balance_changes: HashMap::new(),
            pre_state: HashMap::new(),
            index: 0,
        }
    }

    /// Sets the current block access index.
    pub fn set_index(&mut self, index: u64) {
        self.index = index;
    }

    /// Returns the current block access index.
    pub fn index(&self) -> u64 {
        self.index
    }

    /// Records an address access (even if no state changes).
    fn record_address_access(&mut self, address: Address) {
        self.accessed_addresses.insert(address);
    }

    /// Records a storage read (SLOAD).
    fn record_storage_read(&mut self, address: Address, slot: B256) {
        self.record_address_access(address);

        // Only record as read if it hasn't been written in this transaction
        let written = self
            .storage_writes
            .get(&address)
            .is_some_and(|writes| writes.contains_key(&slot));

        if !written {
            self.storage_reads.entry(address).or_default().insert(slot);
        }
    }

    /// Records a storage write (SSTORE).
    /// The `pre_value` is the value before this SSTORE, and `new_value` is what's being written.
    fn record_storage_write(
        &mut self,
        address: Address,
        slot: B256,
        pre_value: U256,
        new_value: U256,
    ) {
        self.record_address_access(address);

        if pre_value == new_value {
            // No-op write: value unchanged, treat as read
            self.record_storage_read(address, slot);
        } else {
            // Actual write: value changed (includes zeroing)
            self.storage_writes
                .entry(address)
                .or_default()
                .insert(slot, (pre_value, new_value));

            // Remove from reads if previously recorded
            if let Some(reads) = self.storage_reads.get_mut(&address) {
                reads.remove(&slot);
            }
        }
    }

    pub fn record_code_change(&mut self, address: Address, code: Bytes) {
        self.record_address_access(address);

        self.code_changes.insert(address, code);
    }

    /// Merges post-execution state to generate AccountChanges.
    ///
    /// This should be called after each transaction with the post-execution state.
    /// It compares against pre-state to determine actual changes.
    pub fn merge_state(&mut self, state: EvmState) -> Vec<AccountChanges> {
        let mut changes: HashMap<Address, AccountChanges> = HashMap::new();

        // First, ensure all accessed addresses are represented
        for &address in &self.accessed_addresses {
            changes
                .entry(address)
                .or_insert_with(|| AccountChanges::new(address));
        }

        // Process all accounts in the post-execution state
        for (address, account_state) in state.iter() {
            self.record_address_access(*address);

            let account_changes = changes
                .entry(*address)
                .or_insert_with(|| AccountChanges::new(*address));

            // Get pre-state for comparison
            let (pre_balance, pre_nonce, pre_code) = self
                .pre_state
                .get(address)
                .map(|(pre_balance, pre_nonce, pre_code)| {
                    (pre_balance, pre_nonce, pre_code.as_ref().map(|c| c))
                })
                .unwrap_or((&U256::ZERO, &0, None));

            let post_balance = account_state.info.balance;
            let post_nonce = account_state.info.nonce;
            let post_code = account_state
                .info
                .code
                .as_ref()
                .map(|c| c.bytecode().clone());

            // Record balance change only if it actually changed
            if post_balance != *pre_balance {
                account_changes
                    .balance_changes
                    .push(BalanceChange::new(self.index, post_balance));
            }

            // Record nonce change only if it actually changed
            if post_nonce != *pre_nonce {
                account_changes
                    .nonce_changes
                    .push(NonceChange::new(self.index, post_nonce));
            }

            // Record code change only if it actually changed
            if post_code != pre_code.cloned() {
                account_changes
                    .code_changes
                    .push(CodeChange::new(self.index, post_code.unwrap_or_default()));
            }

            // Process storage writes
            if let Some(writes) = self.storage_writes.get(address) {
                for (slot, &(_pre_val, new_val)) in writes {
                    let slot_change = SlotChanges::new(
                        *slot,
                        vec![StorageChange::new(self.index, new_val.into())],
                    );
                    account_changes.storage_changes.push(slot_change);
                }
            }

            // Process storage reads
            if let Some(reads) = self.storage_reads.get(address) {
                for &slot in reads {
                    // Only include if not in writes
                    if !self
                        .storage_writes
                        .get(address)
                        .is_some_and(|w| w.contains_key(&slot))
                    {
                        account_changes.storage_reads.push(slot);
                    }
                }
            }
        }

        // Sort and prepare final output
        let mut result: Vec<_> = changes.into_values().collect();

        // Sort by address (lexicographic)
        result.sort_by_key(|c| c.address);

        // Sort storage changes and reads within each account
        for account in &mut result {
            account.storage_changes.sort_by_key(|c| c.slot);
            account.storage_reads.sort();
            account.storage_reads.dedup();
        }

        result
    }

    /// Clears accumulated state for the next transaction.
    pub fn clear_transaction_state(&mut self) {
        self.storage_writes.clear();
        self.storage_reads.clear();
        // Keep accessed_addresses to track all addresses in the block
    }
}

impl Default for BalInspector {
    fn default() -> Self {
        Self::new()
    }
}

impl<CTX> Inspector<CTX> for BalInspector
where
    CTX: ContextTr<Journal: JournalTr>,
{
    /// Called for each opcode step during EVM execution.
    fn step(&mut self, interp: &mut Interpreter<EthInterpreter>, context: &mut CTX) {
        let op_code = interp.bytecode.opcode();
        let contract = interp.input.target_address();

        match op_code {
            SLOAD => {
                if let Ok(slot) = interp.stack.peek(0) {
                    self.record_storage_read(contract, B256::from(slot));
                }
            }
            SSTORE => {
                // Storage write - need to check pre-value to distinguish actual writes from no-op writes
                if let (Ok(slot), Ok(new_value)) = (interp.stack.peek(0), interp.stack.peek(1)) {
                    // Get pre-value from storage
                    let slot_b256 = B256::from(slot);
                    let pre_value = context
                        .journal_mut()
                        .sload(contract, slot_b256.into())
                        .unwrap_or_default();

                    self.record_storage_write(contract, slot_b256, pre_value.data, new_value);
                }
            }
            BALANCE | EXTCODESIZE | EXTCODECOPY | EXTCODEHASH => {
                // Address accessed (read operations)
                if let Ok(addr) = interp.stack.peek(0) {
                    let address = Address::from_word(addr.into());
                    self.record_address_access(address);
                }
            }
            _ => {}
        }
    }

    /// Called before a CALL-like opcode.
    fn call(&mut self, _context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        // Record target address access for CALL, CALLCODE, DELEGATECALL, STATICCALL
        self.record_address_access(inputs.target_address);
        if inputs.transfer_value().is_some() {
            self.record_address_access(inputs.caller);
        }

        None
    }

    fn create(&mut self, _context: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        self.record_address_access(inputs.caller);
        None
    }

    fn create_end(
        &mut self,
        context: &mut CTX,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        if outcome.result.is_ok() && outcome.address.is_some() {
            let created_address = outcome.address.unwrap();
            let code_load: revm::interpreter::StateLoad<Bytes> =
                context.journal_mut().code(created_address).unwrap();
            let bytes = code_load.data;

            self.record_code_change(created_address, bytes);
        }
    }

    /// Called when SELFDESTRUCT is executed.
    /// Note: Balance changes will be captured in post-execution state comparison.
    fn selfdestruct(&mut self, _contract: Address, target: Address, _value: U256) {
        // Record both the self-destructing contract and the beneficiary
        self.record_address_access(target);
    }
}
