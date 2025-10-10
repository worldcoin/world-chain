//! Block Access List (BAL) utilities for reconstructing state at any transaction index.
//!
//! This module provides functionality to reconstruct the pre-state `BundleState` for any
//! transaction index from the EIP-7928 Block Access List, enabling executionless state updates
//! and parallel transaction validation.
//!
//! This implementation follows the go-ethereum BAL specification.
use alloy_consensus::transaction::SignerRecoverable;
use alloy_eips::Decodable2718;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Encodable;
use eyre::{eyre::eyre, Result};
use flashblocks_primitives::bal::FlashblockBlockAccessList;
use flashblocks_primitives::flashblocks::{Flashblock, Flashblocks};
use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use op_alloy_consensus::OpTxEnvelope;
use parking_lot::Mutex;
use rayon::prelude::*;
use reth::api::PayloadBuilderError;
use reth::revm::database::StateProviderDatabase;
use reth::revm::State;
use reth::rpc::api::eth::helpers::pending_block::BuildPendingEnv;
use reth::{
    providers::StateProvider,
    revm::{
        db::{
            states::{bundle_account::BundleAccount, StorageSlot},
            AccountStatus, BundleState,
        },
        state::{AccountInfo, Bytecode},
    },
};
use reth_evm::execute::BlockExecutor;
use reth_evm::Evm;

use alloy_consensus::{Block, BlockHeader, Header};
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates, ExecutedTrieUpdates};
use reth_evm::precompiles::PrecompilesMap;
use reth_evm::ConfigureEvm;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpEvm, OpNextBlockEnvAttributes};
use reth_optimism_node::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_primitives::{OpPrimitives, OpReceipt, OpTransactionSigned};
use reth_primitives::{Recovered, RecoveredBlock, SealedHeader};
use reth_provider::{ExecutionOutcome, HeaderProvider, StateProviderFactory};
use reth_trie::TrieInput;
use std::fmt::format;
use std::sync::OnceLock;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

mod inspector;
pub use inspector::BalInspector;

use crate::executor::FlashblocksBlockExecutor;

/// Represents state changes for a single account.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountState {
    pub nonce: Option<u64>,
    pub balance: Option<U256>,
    pub code: Option<Bytes>,
    pub storage_writes: Option<HashMap<B256, B256>>,
}

impl AccountState {
    /// Returns true if this account state has no changes.
    pub fn is_empty(&self) -> bool {
        self.nonce.is_none()
            && self.balance.is_none()
            && self.code.is_none()
            && self.storage_writes.is_none()
    }

    /// Checks equality with another AccountState.
    pub fn eq(&self, other: &AccountState) -> bool {
        self.nonce == other.nonce
            && self.balance == other.balance
            && self.code == other.code
            && self.storage_writes == other.storage_writes
    }
}

/// BALReader provides methods for reading account state from a block access list.
/// State values returned from the Reader methods must not be modified.
///
/// This implementation follows the go-ethereum specification for BAL handling.
pub struct BalReader<'a> {
    /// The block access list containing all state changes
    bal: &'a FlashblockBlockAccessList,
    /// State provider for fetching base state (parent block state)
    state_provider: Box<dyn StateProvider>,
}

impl<'a> BalReader<'a> {
    pub fn new(
        parent_block: B256,
        bal: &'a FlashblockBlockAccessList,
        state_provider: &'a impl StateProviderFactory,
    ) -> Self {
        let state_provider = state_provider.state_by_block_hash(parent_block).unwrap();
        Self {
            bal,
            state_provider,
        }
    }

    pub fn provider(&self) -> &Box<dyn StateProvider> {
        &self.state_provider
    }

    pub fn modified_accounts(&self) -> Vec<Address> {
        let mut res = Vec::new();
        for (addr, access) in &self.bal.accounts {
            if !access.nonce_changes.is_empty()
                || !access.code_changes.is_empty()
                || !access.storage_writes.is_empty()
                || !access.balance_changes.is_empty()
            {
                res.push(*addr);
            }
        }
        res
    }

    pub fn accessed_state(&self) -> HashMap<Address, HashSet<B256>> {
        let mut res = HashMap::new();
        for (addr, accesses) in &self.bal.accounts {
            if !accesses.storage_reads.is_empty() {
                res.insert(*addr, accesses.storage_reads.clone());
            } else if accesses.balance_changes.is_empty()
                && accesses.nonce_changes.is_empty()
                && accesses.storage_writes.is_empty()
                && accesses.code_changes.is_empty()
            {
                // Account was accessed but had no changes
                res.insert(*addr, HashSet::default());
            }
        }
        res
    }

    pub fn read_account_diff(&self, addr: Address, idx: u64) -> Option<AccountState> {
        let diff = self.bal.accounts.get(&addr)?;
        let mut res = AccountState::default();

        let mut balance_changes: Vec<_> = diff.balance_changes.iter().collect();
        balance_changes.sort_by_key(|(tx_idx, _)| *tx_idx);
        for (tx_idx, balance) in balance_changes {
            if (*tx_idx as u64) <= idx {
                res.balance = Some(*balance);
            } else {
                break;
            }
        }

        // Collect code changes up to and including idx
        let mut code_changes: Vec<_> = diff.code_changes.iter().collect();
        code_changes.sort_by_key(|(tx_idx, _)| *tx_idx);
        for (tx_idx, code) in code_changes {
            if (*tx_idx as u64) <= idx {
                res.code = Some(code.clone());
            } else {
                break;
            }
        }

        // Collect nonce changes up to and including idx
        let mut nonce_changes: Vec<_> = diff.nonce_changes.iter().collect();
        nonce_changes.sort_by_key(|(tx_idx, _)| *tx_idx);
        for (tx_idx, nonce) in nonce_changes {
            if (*tx_idx as u64) <= idx {
                res.nonce = Some(*nonce);
            } else {
                break;
            }
        }

        // Collect storage changes up to and including idx
        if !diff.storage_writes.is_empty() {
            res.storage_writes = Some(HashMap::new());
            for (slot, slot_writes) in &diff.storage_writes {
                let mut writes: Vec<_> = slot_writes.iter().collect();
                writes.sort_by_key(|(tx_idx, _)| *tx_idx);
                for (tx_idx, value) in writes {
                    if (*tx_idx as u64) <= idx {
                        res.storage_writes.as_mut().unwrap().insert(*slot, *value);
                    } else {
                        break;
                    }
                }
            }
        }

        Some(res)
    }

    /// Returns the state changes of an account at a given index (not accumulated).
    /// Returns None if there are no changes at this specific index.
    ///
    /// This matches the Go implementation's accountChangesAt method.
    pub fn account_changes_at(&self, addr: Address, idx: u64) -> Option<AccountState> {
        let acct = self.bal.accounts.get(&addr)?;
        let mut res = AccountState::default();

        // Find balance change at this exact index
        // Go implementation iterates backwards through sorted array
        let mut balance_changes: Vec<_> = acct.balance_changes.iter().collect();
        balance_changes.sort_by_key(|(tx_idx, _)| *tx_idx);
        balance_changes.reverse();

        for (tx_idx, balance) in balance_changes {
            if (*tx_idx as u64) == idx {
                res.balance = Some(*balance);
            }
            if (*tx_idx as u64) < idx {
                break;
            }
        }

        // Find code change at this exact index
        let mut code_changes: Vec<_> = acct.code_changes.iter().collect();
        code_changes.sort_by_key(|(tx_idx, _)| *tx_idx);
        code_changes.reverse();

        for (tx_idx, code) in code_changes {
            if (*tx_idx as u64) == idx {
                res.code = Some(code.clone());
                break;
            }
            if (*tx_idx as u64) < idx {
                break;
            }
        }

        // Find nonce change at this exact index
        let mut nonce_changes: Vec<_> = acct.nonce_changes.iter().collect();
        nonce_changes.sort_by_key(|(tx_idx, _)| *tx_idx);
        nonce_changes.reverse();

        for (tx_idx, nonce) in nonce_changes {
            if (*tx_idx as u64) == idx {
                res.nonce = Some(*nonce);
                break;
            }
            if (*tx_idx as u64) < idx {
                break;
            }
        }

        // Find storage changes at this exact index
        for (slot, slot_writes) in &acct.storage_writes {
            let mut writes: Vec<_> = slot_writes.iter().collect();
            writes.sort_by_key(|(tx_idx, _)| *tx_idx);
            writes.reverse();

            for (tx_idx, value) in writes {
                if (*tx_idx as u64) == idx {
                    if res.storage_writes.is_none() {
                        res.storage_writes = Some(HashMap::new());
                    }
                    res.storage_writes.as_mut().unwrap().insert(*slot, *value);
                    break;
                }
                if (*tx_idx as u64) < idx {
                    break;
                }
            }
        }

        if res.storage_writes.as_ref().is_none_or(|w| w.is_empty()) {
            res.storage_writes = None;
        }

        if res.is_empty() {
            None
        } else {
            Some(res)
        }
    }

    /// Returns all state changes at the given index (not accumulated).
    /// This matches the Go implementation's changesAt method.
    pub fn changes_at(&self, idx: u64) -> StateDiff {
        let mut res = StateDiff {
            mutations: HashMap::new(),
        };

        for addr in self.bal.accounts.keys() {
            if let Some(account_changes) = self.account_changes_at(*addr, idx) {
                res.mutations.insert(*addr, account_changes);
            }
        }

        res
    }

    /// Validates that the computed state diff matches the BAL entry at the given index.
    /// Returns an error if they don't match.
    ///
    /// This matches the Go implementation's ValidateStateDiff method.
    pub fn validate_state_diff(&self, idx: u64, computed_diff: &StateDiff) -> Result<()> {
        let bal_changes = self.changes_at(idx);

        for (addr, state) in &bal_changes.mutations {
            let computed_account_diff = computed_diff.mutations.get(addr).ok_or_else(|| {
                eyre!(
                    "BAL contained account {:?} which wasn't present in computed state diff",
                    addr
                )
            })?;

            if !state.eq(computed_account_diff) {
                return Err(eyre!(
                    "difference between computed state diff and BAL entry for account {:?}",
                    addr
                ));
            }
        }

        if bal_changes.mutations.len() != computed_diff.mutations.len() {
            return Err(eyre!(
                "computed state diff contained mutated accounts which weren't reported in BAL"
            ));
        }

        Ok(())
    }

    /// Validates that the read set in the BAL matches the computed reads exactly.
    /// Removes any slots from all_reads which were written before checking.
    ///
    /// This matches the Go implementation's ValidateStateReads method.
    pub fn validate_state_reads(
        &self,
        mut all_reads: HashMap<Address, HashSet<B256>>,
    ) -> Result<()> {
        let num_txs = self.bal.max_tx_index - self.bal.min_tx_index;

        for (addr, reads) in &mut all_reads {
            // Remove any slots that were written
            if let Some(bal_acct_diff) = self.read_account_diff(*addr, num_txs + 1) {
                if let Some(writes) = bal_acct_diff.storage_writes {
                    for write_slot in writes.keys() {
                        reads.remove(write_slot);
                    }
                }
            }

            // Validate reads match
            let expected_reads = self
                .bal
                .accounts
                .get(addr)
                .map(|a| &a.storage_reads)
                .cloned()
                .unwrap_or_default();

            if reads.len() != expected_reads.len() {
                return Err(eyre!(
                    "mismatch between the number of computed reads and number of expected reads for address {:?}",
                    addr
                ));
            }

            for slot in &expected_reads {
                if !reads.contains(slot) {
                    return Err(eyre!(
                        "expected read is missing from BAL for address {:?}",
                        addr
                    ));
                }
            }
        }

        Ok(())
    }

    /// Checks if an account has been modified in the BAL.
    pub fn is_modified(&self, addr: Address) -> bool {
        if let Some(access) = self.bal.accounts.get(&addr) {
            !access.storage_writes.is_empty()
                || !access.balance_changes.is_empty()
                || !access.code_changes.is_empty()
                || !access.nonce_changes.is_empty()
        } else {
            false
        }
    }

    /// Initializes a state object from a diff, applying changes directly to the state.
    /// Used for mutated objects (similar to Go's initMutatedObjFromDiff).
    fn init_mutated_account_from_diff(
        &self,
        base_acct: Option<AccountInfo>,
        diff: Option<&AccountState>,
    ) -> Result<AccountInfo> {
        let mut acct = base_acct.unwrap_or_else(|| AccountInfo {
            nonce: 0,
            balance: U256::ZERO,
            code_hash: B256::ZERO,
            code: None,
        });

        if let Some(diff) = diff {
            if let Some(nonce) = diff.nonce {
                acct.nonce = nonce;
            }
            if let Some(balance) = diff.balance {
                acct.balance = balance;
            }
            if let Some(code) = &diff.code {
                acct.code_hash = keccak256(code);
                acct.code = Some(Bytecode::new_raw(code.clone()));
            }
        }

        Ok(acct)
    }

    /// Reconstructs storage for an account from the diff.
    fn reconstruct_storage_from_diff(
        &self,
        addr: Address,
        diff: Option<&AccountState>,
    ) -> Result<HashMap<U256, StorageSlot>> {
        let mut storage = HashMap::new();

        if let Some(diff) = diff {
            if let Some(storage_writes) = &diff.storage_writes {
                for (slot, value) in storage_writes {
                    let slot_u256 = U256::from_be_bytes(slot.0);

                    // Get original value from parent block state
                    let original_value = self
                        .state_provider
                        .storage(addr, slot_u256.into())?
                        .unwrap_or(U256::ZERO);

                    let present_value = U256::from_be_bytes(value.0);

                    storage.insert(
                        slot_u256,
                        StorageSlot::new_changed(original_value, present_value),
                    );
                }
            }
        }

        Ok(storage)
    }

    /// Computes the state root by applying all mutations up to the last transaction index.
    /// Returns the state root along with timing information.
    ///
    /// This matches the Go implementation's StateRoot method.
    pub fn state_root(&self) -> Result<(B256, std::time::Duration, std::time::Duration)> {
        use std::time::Instant;

        let last_idx = self.bal.max_tx_index;
        let modified_accts = self.modified_accounts();

        let start_prestate_load = Instant::now();

        // Build bundle state with all modifications
        let mut bundle = BundleState::default();

        for addr in modified_accts {
            let diff = self.read_account_diff(addr, last_idx);
            let base_info = self
                .state_provider
                .basic_account(&addr)?
                .map(|a| AccountInfo {
                    nonce: a.nonce,
                    balance: a.balance,
                    code_hash: a.bytecode_hash.unwrap_or(B256::ZERO),
                    code: None,
                });

            let present_info =
                self.init_mutated_account_from_diff(base_info.clone(), diff.as_ref())?;

            let storage = self.reconstruct_storage_from_diff(addr, diff.as_ref())?;

            let status = if diff.is_some() {
                AccountStatus::Changed
            } else {
                AccountStatus::Loaded
            };

            let bundle_account = BundleAccount::new(
                base_info,
                Some(present_info),
                storage.into_iter().collect(),
                status,
            );

            bundle.state.insert(addr, bundle_account);
        }

        let prestate_load_time = start_prestate_load.elapsed();

        let root_update_start = Instant::now();
        let hashed_state = self.state_provider.hashed_post_state(&bundle);
        let (state_root, _) = self.state_provider.state_root_with_updates(hashed_state)?;
        let root_update_time = root_update_start.elapsed();

        Ok((state_root, prestate_load_time, root_update_time))
    }

    /// Builds a minimal pre-state bundle containing only accounts that will be accessed
    /// during the execution of transaction at `tx_index`.
    ///
    /// This is useful for parallel execution where you only need the minimal state
    /// required to execute a specific transaction.
    pub fn bundle_pre_state_at(&self, tx_index: u64) -> Result<BundleState> {
        let mut bundle = BundleState::default();

        // Only include accounts that are accessed at exactly tx_index
        for (address, account_access) in &self.bal.accounts {
            // Check if this account is accessed at tx_index
            let accessed_at_index = account_access
                .balance_changes
                .contains_key(&(tx_index as u16))
                || account_access
                    .nonce_changes
                    .contains_key(&(tx_index as u16))
                || account_access.code_changes.contains_key(&(tx_index as u16))
                || account_access
                    .storage_writes
                    .iter()
                    .any(|(_, writes)| writes.contains_key(&(tx_index as u16)))
                || !account_access.storage_reads.is_empty();

            if accessed_at_index {
                let base_info = self
                    .state_provider
                    .basic_account(address)?
                    .map(|a| AccountInfo {
                        nonce: a.nonce,
                        balance: a.balance,
                        code_hash: a.bytecode_hash.unwrap_or(B256::ZERO),
                        code: None,
                    });

                let diff = self.read_account_diff(*address, tx_index);
                let present_info =
                    self.init_mutated_account_from_diff(base_info.clone(), diff.as_ref())?;

                let storage = self.reconstruct_storage_from_diff(*address, diff.as_ref())?;

                let status = if diff.is_some() {
                    AccountStatus::Changed
                } else {
                    AccountStatus::Loaded
                };

                let bundle_account = BundleAccount::new(
                    base_info,
                    Some(present_info),
                    storage.into_iter().collect(),
                    status,
                );

                bundle.state.insert(*address, bundle_account);
            }
        }

        Ok(bundle)
    }

    /// Validates that the BAL is properly ordered and consistent.
    ///
    /// Checks:
    /// - Balance changes are in ascending order
    /// - Nonce changes are in ascending order
    /// - Code changes are in ascending order
    /// - Storage changes are properly ordered
    pub fn pre_checks(&self, bal: &FlashblockBlockAccessList) -> Result<()> {
        for (address, account_access) in &bal.accounts {
            // Validate balance changes are ordered
            let mut balance_changes: Vec<_> = account_access.balance_changes.keys().collect();
            balance_changes.sort();
            let mut prev_idx = None;
            for idx in balance_changes {
                if let Some(prev) = prev_idx {
                    if *idx <= prev {
                        return Err(eyre!(
                            "Balance changes not ordered for address {:?}: {} <= {}",
                            address,
                            idx,
                            prev
                        ));
                    }
                }
                prev_idx = Some(*idx);
            }

            // Validate nonce changes are ordered
            let mut nonce_changes: Vec<_> = account_access.nonce_changes.keys().collect();
            nonce_changes.sort();
            let mut prev_idx = None;
            for idx in nonce_changes {
                if let Some(prev) = prev_idx {
                    if *idx <= prev {
                        return Err(eyre!(
                            "Nonce changes not ordered for address {:?}: {} <= {}",
                            address,
                            idx,
                            prev
                        ));
                    }
                }
                prev_idx = Some(*idx);
            }

            // Validate code changes are ordered
            let mut code_changes: Vec<_> = account_access.code_changes.keys().collect();
            code_changes.sort();
            let mut prev_idx = None;
            for idx in code_changes {
                if let Some(prev) = prev_idx {
                    if *idx <= prev {
                        return Err(eyre!(
                            "Code changes not ordered for address {:?}: {} <= {}",
                            address,
                            idx,
                            prev
                        ));
                    }
                }
                prev_idx = Some(*idx);
            }

            // Validate storage writes are ordered
            for (slot, writes) in &account_access.storage_writes {
                let mut write_indices: Vec<_> = writes.keys().collect();
                write_indices.sort();
                let mut prev_idx = None;
                for idx in write_indices {
                    if let Some(prev) = prev_idx {
                        if *idx <= prev {
                            return Err(eyre!(
                                "Storage writes not ordered for address {:?} slot {:?}: {} <= {}",
                                address,
                                slot,
                                idx,
                                prev
                            ));
                        }
                    }
                    prev_idx = Some(*idx);
                }
            }
        }

        Ok(())
    }
}

/// Result from executing a single transaction in parallel
#[derive(Debug, Clone)]
pub struct TransactionExecutionResult {
    /// Transaction index in the block
    pub index: u64,
    /// The state diff from this transaction (changes made)
    pub computed_state_diff: StateDiff,
    /// Gas used by this transaction
    pub gas_used: u64,
    /// The receipt for this transaction
    pub receipt: Vec<OpReceipt>,
}

/// Result from the parallel state root computation
#[derive(Debug)]
pub struct StateRootResult {
    /// The computed state root
    pub state_root: B256,
    /// Time taken to load pre-state
    pub prestate_load_time: std::time::Duration,
    /// Time taken to compute state root
    pub root_update_time: std::time::Duration,
}

#[derive(Debug, Clone, Default)]
pub enum PendingBlockStatus {
    #[default]
    Validating,
    Valid,
    Invalid(String),
}

/// A type used as a collector from multiple tasks for pieces of the fully execution
///
/// [`ExecutedBlockWithTrieUpdates`]
pub struct PendingBlock<'a> {
    /// Total transactions to be executed.
    transaction_count: usize,
    /// Pending Root
    state_root: OnceLock<B256>,
    /// The aggregate of [`FlashblocksPayloadV1`] associated with the current payload.
    payloads: Vec<FlashblocksPayloadV1>,
    /// Valid state diffs
    validated_state_diffs: Arc<Mutex<Vec<StateDiff>>>,
    /// Execution outcomes for each transaction
    outcomes: Arc<Mutex<Vec<TransactionExecutionResult>>>,
    /// The status of the BAL validation
    status: Arc<Mutex<PendingBlockStatus>>,
    /// The BAL reader
    reader: Arc<BalReader<'a>>,
    /// Cumulative bundle over the entire block
    bundle: BundleState,
}

impl<'a> PendingBlock<'a> {
    pub fn new(
        transaction_count: usize,
        payloads: Vec<FlashblocksPayloadV1>,
        reader: Arc<BalReader<'a>>,
    ) -> Self {
        Self {
            payloads,
            transaction_count,
            state_root: OnceLock::new(),
            validated_state_diffs: Arc::new(Mutex::new(Vec::with_capacity(transaction_count))),
            status: Arc::new(Mutex::new(PendingBlockStatus::Validating)),
            reader,
            outcomes: Arc::new(Mutex::new(Vec::with_capacity(transaction_count))),
            bundle: BundleState::default(), // TODO
        }
    }

    /// Setter hook for state root task
    pub fn on_state_root(&self, state_root: StateRootResult) {
        let pending = self.state_root.clone();
        pending.set(state_root.state_root).ok();
        if self.outcomes.lock().len() == self.transaction_count {
            // Make sure the state roots are coherent
            if self.payloads.last().unwrap().diff.state_root == state_root.state_root {
                self.set_status(PendingBlockStatus::Valid);
            } else {
                self.set_status(PendingBlockStatus::Invalid(format!(
                    "State roots do not match: expected {:?}, got {:?}",
                    self.payloads.last().unwrap().diff.state_root,
                    state_root.state_root
                )));
            }
        }
    }

    /// Setter hook for transaction execution task(s) results
    /// Validates the state diff against the BAL and stores if valid
    ///
    /// If invalid, sets the status to Invalid with error message
    pub fn on_result(&self, result: TransactionExecutionResult) {
        let validated = self.validated_state_diffs.clone();
        let reader = self.reader.clone();
        let status = self.status.clone();
        let results = self.outcomes.clone();

        results.lock().push(result.clone());

        let mut validated = validated.lock();

        match reader.validate_state_diff(result.index, &result.computed_state_diff) {
            Ok(_) => {
                validated.push(result.computed_state_diff);
                if validated.len() == self.transaction_count && self.state_root.get().is_some() {
                    *status.lock() = PendingBlockStatus::Valid;
                }
            }
            Err(e) => {
                *status.lock() = PendingBlockStatus::Invalid(e.to_string());
            }
        }
    }

    pub fn set_status(&self, status: PendingBlockStatus) {
        let mut lock = self.status.lock();
        *lock = status;
    }

    pub fn is_valid(&self) -> bool {
        matches!(*self.status.lock(), PendingBlockStatus::Valid)
    }

    pub fn finish(&self) -> eyre::Result<ExecutedBlockWithTrieUpdates<OpPrimitives>> {
        let flashblocks = Flashblock::reduce(Flashblocks::new(
            self.payloads
                .iter()
                .map(|p| Flashblock {
                    flashblock: p.clone(),
                })
                .collect(),
        )?)
        .ok_or(eyre!("Failed to reduce flashblocks"))?;

        // Ensure the block is valid
        match &*self.status.lock() {
            PendingBlockStatus::Valid => {}
            PendingBlockStatus::Invalid(err) => {
                return Err(eyre!("Pending block is invalid: {}", err));
            }
            PendingBlockStatus::Validating => {
                return Err(eyre!("Pending block is still validating"));
            }
        }

        // Grab the outcomes
        let mut outcomes = self.outcomes.lock().clone();
        outcomes.sort_by(|a, b| a.index.cmp(&b.index));

        let receipts: Vec<OpReceipt> = outcomes.iter().flat_map(|o| o.receipt.clone()).collect();
        let recovered_block: RecoveredBlock<Block<OpTransactionSigned>> =
            RecoveredBlock::try_from(flashblocks)?;

        if recovered_block.state_root()
            != *self.state_root.get().ok_or(eyre!("State root not set"))?
        {
            return Err(eyre!(
                "State root mismatch: expected {:?}, got {:?}",
                recovered_block.state_root(),
                self.state_root.get().unwrap()
            ));
        }

        let bundle = self.bundle.clone(); // TODO
        let hashed_state = self.reader.provider().hashed_post_state(&bundle);

        Ok(ExecutedBlockWithTrieUpdates::<OpPrimitives> {
            block: ExecutedBlock {
                recovered_block: Arc::new(recovered_block),
                hashed_state: hashed_state.clone().into(),
                execution_output: Arc::new(ExecutionOutcome {
                    bundle,
                    receipts: vec![receipts],
                    ..Default::default()
                }),
            },
            trie: ExecutedTrieUpdates::Present(Arc::new(TrieInput::from_state(hashed_state).nodes)),
        })
    }
}

/// Coordinator for parallel BAL validation and execution.
///
/// This type orchestrates:
/// 1. Pre-checks on the BAL structure
/// 2. Parallel state root computation
/// 3. Parallel transaction execution
/// 4. State diff validation
/// 5. Final block construction
pub struct BalPayloadValidator<'a> {
    reader: Arc<BalReader<'a>>,
    evm_config: OpEvmConfig,
    chain_spec: Arc<OpChainSpec>,
    pending_block: PendingBlock<'a>,
    sealed_header: SealedHeader,
}

impl<'a> BalPayloadValidator<'a> {
    /// Creates a new coordinator from a BAL reader and payload.
    pub fn new(
        reader: BalReader<'a>,
        evm_config: OpEvmConfig,
        chain_spec: Arc<OpChainSpec>,
        active_payload: FlashblocksPayloadV1,
        aggregated_payload: Vec<FlashblocksPayloadV1>,
        sealed_header: SealedHeader,
    ) -> Self {
        let reader = Arc::new(reader);
        let transaction_count = active_payload.diff.transactions.len();

        Self {
            reader: reader.clone(),
            evm_config,
            chain_spec,
            pending_block: PendingBlock::new(transaction_count, aggregated_payload, reader.clone()),
            sealed_header,
        }
    }

    /// Validates and executes the payload in parallel.
    ///
    /// This performs the following steps:
    /// 1. Validates BAL structure (pre_checks)
    /// 2. Spawns two parallel tasks:
    ///    a. State root computation using `reader.state_root()`
    ///    b. Transaction execution using `execute_all_transactions()`
    /// 3. Waits for both to complete
    /// 4. Validates that computed state diffs match BAL
    /// 5. Constructs and returns `ExecutedBlockWithTrieUpdates`
    pub fn validate_and_execute(
        self,
        mut payloads: Vec<FlashblocksPayloadV1>,
    ) -> eyre::Result<ExecutedBlockWithTrieUpdates<OpPrimitives>> {
        let payload = payloads
            .pop()
            .ok_or_else(|| eyre!("No active payload found"))?;

        let bal_payload = payload.diff.flash_bal.clone();

        // let bal_hash = payload.diff.bal_hash;
        // let buff = &[0u8; 32][..];
        // payload.diff.flash_bal.encode(&mut buff);
        // let computed_bal_hash = keccak256(buff);

        // if bal_hash != computed_bal_hash {
        //     return Err(eyre!(
        //         "BAL hash mismatch: expected {:?}, got {:?}",
        //         bal_hash,
        //         computed_bal_hash
        //     ));
        // }

        let bal: FlashblockBlockAccessList = bal_payload.into();

        // Step 1: Run pre-checks on BAL structure
        self.reader.pre_checks(&bal)?;

        let transactions = &payload.diff.transactions;
        let recovered = transactions
            .iter()
            .map(|tx_bytes| {
                let mut tx_bytes_slice = tx_bytes.as_ref();
                let tx = OpTxEnvelope::decode_2718(&mut tx_bytes_slice)
                    .map_err(|e| eyre!("Failed to decode transaction: {}", e));
                tx.map(|t| {
                    t.try_into_recovered_unchecked()
                        .map_err(|e| eyre!("Failed to recover tx: {:?}", e))
                })
                .and_then(|r| r)
            })
            .collect::<Result<Vec<Recovered<OpTxEnvelope>>>>()?;

        let tx_iter = recovered.into_iter().enumerate().collect::<Vec<_>>();
        let parent = self.sealed_header.clone();

        // Step 2: Execute state root computation and transaction execution in parallel
        let this = Arc::new(&self);

        let (_, tx_execution_result) = rayon::join(
            || this.compute_state_root(),
            || this.execute_all_transactions(tx_iter, &parent),
        );

        tx_execution_result?;

        this.pending_block.finish()
    }

    /// Computes the state root using the BAL reader.
    ///
    /// This can run in parallel with transaction execution.
    fn compute_state_root(&self) {
        if let Ok((state_root, prestate_load_time, root_update_time)) = self.reader.state_root() {
            self.pending_block.on_state_root(StateRootResult {
                state_root,
                prestate_load_time,
                root_update_time,
            });
        }
    }

    /// Executes all transactions in parallel.
    ///
    /// Returns results for each transaction including their state diffs.
    fn execute_all_transactions<I>(
        &self,
        transactions: I,
        parent_header: &SealedHeader,
    ) -> Result<()>
    where
        I: IntoParallelIterator<Item = (usize, Recovered<OpTxEnvelope>)>,
    {
        let this = Arc::new(self);
        transactions
            .into_par_iter()
            .map(|(i, tx)| {
                let tx = tx.clone();
                let this = this.clone();
                let res = this.execute_transaction_at_index(
                    i as u64,
                    parent_header,
                    move |mut executor| {
                        let tx_envelope = tx.clone();
                        let exec_result = executor
                            .execute_transaction(tx_envelope)
                            .inspect_err(|e| {
                                eprintln!("Error executing transaction at index {}: {:?}", i, e);
                            })
                            .unwrap_or(0);

                        let (mut evm, result) = executor.finish()?;
                        let bundle = evm.db_mut().take_bundle();

                        let result = TransactionExecutionResult {
                            index: i as u64,
                            computed_state_diff: StateDiff::from(bundle),
                            gas_used: exec_result,
                            receipt: result.receipts,
                        };

                        Ok(result)
                    },
                )?;

                self.pending_block.on_result(res);
                Ok::<(), eyre::Report>(())
            })
            .collect::<Result<Vec<()>>>()?;

        Ok(())
    }

    /// Executes a single transaction at the given index using the BAL pre-state.
    fn execute_transaction_at_index<F>(
        &self,
        tx_index: u64,
        header: &SealedHeader,
        f: F,
    ) -> Result<TransactionExecutionResult>
    where
        F: FnOnce(
                FlashblocksBlockExecutor<
                    OpEvm<
                        &mut State<StateProviderDatabase<&Box<dyn StateProvider>>>,
                        BalInspector,
                        PrecompilesMap,
                    >,
                    OpRethReceiptBuilder,
                    Arc<OpChainSpec>,
                >,
            ) -> eyre::Result<TransactionExecutionResult>
            + Send
            + 'a,
    {
        // 1. Reconstruct pre-state bundle for this transaction (state at tx_index - 1)
        let pre_state_index = if tx_index == 0 { 0 } else { tx_index - 1 };
        let bundle_prestate = self.reader.bundle_pre_state_at(pre_state_index)?;

        // 2. Initialize EVM with this pre-state
        let state = StateProviderDatabase::new(self.reader().provider());
        let mut state = State::builder()
            .with_database(state)
            .with_bundle_prestate(bundle_prestate)
            .with_bundle_update()
            .build();

        let attributes = OpNextBlockEnvAttributes::build_pending_env(&header.clone());

        let execution_ctx = self
            .evm_config
            .context_for_next_block(&header, attributes.clone())?;

        let evm_env = self
            .evm_config
            .next_evm_env(header, &attributes)
            .map_err(PayloadBuilderError::other)?;

        let evm =
            self.evm_config
                .evm_with_env_and_inspector(&mut state, evm_env, BalInspector::new());

        let block_executor = FlashblocksBlockExecutor::new(
            evm,
            execution_ctx,
            self.chain_spec.clone(),
            OpRethReceiptBuilder::default(),
        )
        .with_receipts(Vec::new());

        f(block_executor)
    }

    /// Returns the underlying BAL reader.
    pub fn reader(&self) -> &BalReader<'a> {
        &self.reader
    }
}

/// Represents all state changes at a specific transaction index.
#[derive(Clone, Debug, Default)]
pub struct StateDiff {
    pub mutations: HashMap<Address, AccountState>,
}

impl From<BundleState> for StateDiff {
    fn from(value: BundleState) -> Self {
        let mutations = value
            .state
            .into_iter()
            .filter_map(|(account, state)| {
                if let Some(info) = state.info {
                    let account_state = AccountState {
                        nonce: Some(info.nonce),
                        balance: Some(info.balance),
                        code: info.code.as_ref().map(|c| c.bytes().clone()),
                        storage_writes: Some(
                            state
                                .storage
                                .iter()
                                .map(|(slot, s)| (B256::from(*slot), B256::from(s.present_value)))
                                .collect(),
                        ),
                    };
                    Some((account, account_state))
                } else {
                    None
                }
            })
            .collect();

        Self { mutations }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_bal() {
        let bal = FlashblockBlockAccessList::default();
        assert_eq!(bal.accounts.len(), 0);
    }

    #[test]
    fn test_account_state_equality() {
        let state1 = AccountState {
            nonce: Some(1),
            balance: Some(U256::from(100)),
            code: None,
            storage_writes: None,
        };

        let state2 = AccountState {
            nonce: Some(1),
            balance: Some(U256::from(100)),
            code: None,
            storage_writes: None,
        };

        assert!(state1.eq(&state2));
    }
}
