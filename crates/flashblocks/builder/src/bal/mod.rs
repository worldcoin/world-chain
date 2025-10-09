//! Block Access List (BAL) utilities for reconstructing state at any transaction index.
//!
//! This module provides functionality to reconstruct the pre-state `BundleState` for any
//! transaction index from the EIP-7928 Block Access List, enabling executionless state updates
//! and parallel transaction validation.
//!
//! This implementation follows the go-ethereum BAL specification.

use alloy_consensus::transaction::SignerRecoverable;
use alloy_eips::eip7685::Requests;
use alloy_eips::Decodable2718;
use alloy_op_evm::OpBlockExecutionCtx;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use eyre::{eyre::eyre, Result};
use flashblocks_primitives::bal::FlashblockBlockAccessList;
use flashblocks_primitives::primitives::FlashblocksPayloadV1;
use op_alloy_consensus::OpTxEnvelope;
use parking_lot::Mutex;
use rayon::prelude::*;
use reth::api::{Block as _, PayloadBuilderError};
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
use reth_evm::block::StateChangeSource;
use reth_evm::execute::{BlockAssemblerInput, BlockExecutor};

use alloy_consensus::{Block, BlockBody, BlockHeader, Header};
use alloy_eips::eip4895::Withdrawals;
use reth_chain_state::{ExecutedBlock, ExecutedBlockWithTrieUpdates, ExecutedTrieUpdates};
use reth_evm::precompiles::PrecompilesMap;
use reth_evm::{op_revm::OpSpecId, Evm};
use reth_evm::{ConfigureEvm, EvmEnv, OnStateHook};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::{OpBlockAssembler, OpEvm, OpNextBlockEnvAttributes};
use reth_optimism_node::{OpEvmConfig, OpRethReceiptBuilder};
use reth_optimism_primitives::{OpPrimitives, OpReceipt};
use reth_primitives::{Recovered, SealedHeader};
use reth_provider::{BlockExecutionResult, HeaderProvider};
use revm::state::EvmState;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

mod inspector;
pub use inspector::BalInspector;

use crate::executor::{FlashblocksBlockExecutor, FlashblocksBlockExecutorFactory};

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

/// Represents all state changes at a specific transaction index.
#[derive(Clone, Debug, Default)]
pub struct StateDiff {
    pub mutations: HashMap<Address, AccountState>,
}

/// BALReader provides methods for reading account state from a block access list.
/// State values returned from the Reader methods must not be modified.
///
/// This implementation follows the go-ethereum specification for BAL handling.
pub struct BalReader<'a, P> {
    /// The block access list containing all state changes
    bal: &'a FlashblockBlockAccessList,
    /// State provider for fetching base state (parent block state)
    state_provider: &'a P,
}

impl<'a, P: StateProvider + HeaderProvider<Header = alloy_consensus::Header> + Send + Sync>
    BalReader<'a, P>
{
    pub fn new(bal: &'a FlashblockBlockAccessList, state_provider: &'a P) -> Self {
        Self {
            bal,
            state_provider,
        }
    }

    pub fn provider(&self) -> &P {
        self.state_provider
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
        _addr: Address,
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
            // Note: storage is handled separately in bundle state
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

            // Get base account info
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
                self.init_mutated_account_from_diff(addr, base_info.clone(), diff.as_ref())?;

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
                let present_info = self.init_mutated_account_from_diff(
                    *address,
                    base_info.clone(),
                    diff.as_ref(),
                )?;

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
    pub state_diff: StateDiff,
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

/// State hook for collecting execution results during parallel transaction execution.
///
/// This hook is thread-safe and collects state changes, receipts, and gas usage
/// for each transaction as it executes.
#[derive(Debug, Clone, Default)]
pub struct PendingBlock {
    /// Next state diff
    next_state_diff: Option<(u64, StateDiff)>,
    /// Pending Root
    state_root: Arc<Mutex<B256>>,
    /// Flashblocks Payloads
    payloads: Vec<FlashblocksPayloadV1>,
    /// Mapping from current transaction indcex to vality of the payload
    payload_tx_validated: HashMap<u64, bool>,
    /// Ubfilled transaction indices
    unfilled_tx_indices: HashSet<u64>,
    /// Valid state diffs
    invalidated_state_diffs: HashMap<u64, StateDiff>,
    /// All transaction indices that have been processed
    results: Arc<Mutex<Vec<TransactionExecutionResult>>>,
}

impl PendingBlock {
    /// Creates a new collector.
    pub fn new() -> Self {
        Self {
            results: Arc::new(parking_lot::Mutex::new(Vec::new())),
            next_state_diff: (0, StateDiff::default()).into(),
            ..Default::default()
        }
    }

    pub fn latest_state(&self) -> Option<(u64, StateDiff)> {
        self.next_state_diff.clone()
    }

    pub fn on_state_root(&self) -> impl Fn(StateRootResult) + Send + Sync + Clone + 'static {
        let pending = self.state_root.clone();
        move |root_result| {
            *pending.lock() = root_result.state_root;
        }
    }

    pub fn on_result(&self) -> impl Fn(TransactionExecutionResult) + Send + Sync + Clone + 'static {
        let results = self.results.clone();
        move |res| {
            results.lock().push(res);
        }
    }

    /// Returns the number of collected results.
    pub fn len(&self) -> usize {
        self.results.lock().len()
    }

    /// Returns true if no results have been collected.
    pub fn is_empty(&self) -> bool {
        self.results.lock().is_empty()
    }
}

impl<'a> OnStateHook for &'static PendingBlock {
    fn on_state(&mut self, source: StateChangeSource, state: &EvmState) {
        self.clone().on_state(source, state);
    }
}

impl reth_evm::OnStateHook for PendingBlock {
    // Validate the integrity of the State Diff on the current BAL index
    // every time we get a state hook from the EVM
    fn on_state(&mut self, s: StateChangeSource, _state: &EvmState) {
        // check the evm state matches our next_state_diff
        if let Some((_, state_diff)) = &self.next_state_diff {
            for (addr, acct) in &state_diff.mutations {
                if let Some(evm_acct) = _state.get(addr) {
                    if let Some(nonce) = acct.nonce {
                        if evm_acct.info.nonce != nonce {
                            panic!(
                                "Nonce mismatch for account {:?}: expected {}, got {}",
                                addr, nonce, evm_acct.info.nonce
                            );
                        }
                    }
                    if let Some(balance) = acct.balance {
                        if evm_acct.info.balance != balance {
                            panic!(
                                "Balance mismatch for account {:?}: expected {}, got {}",
                                addr, balance, evm_acct.info.balance
                            );
                        }
                    }
                    if let Some(code) = &acct.code {
                        let code_hash = keccak256(code);
                        if evm_acct.info.code_hash != code_hash {
                            panic!(
                                "Code hash mismatch for account {:?}: expected {:?}, got {:?}",
                                addr, code_hash, evm_acct.info.code_hash
                            );
                        }
                    }
                    if let Some(storage_writes) = &acct.storage_writes {
                        for (slot, value) in storage_writes {
                            let slot_u256 = U256::from_be_bytes(slot.0);
                            let expected_value = U256::from_be_bytes(value.0);
                            let actual_value = _state
                                .get(addr)
                                .and_then(|s| s.storage.get(&slot_u256))
                                .cloned();

                            if actual_value.is_none()
                                || actual_value.clone().unwrap_or_default().present_value
                                    != expected_value
                            {
                                panic!(
                                "Storage mismatch for account {:?} slot {:?}: expected {}, got {:?}",
                                addr, slot, expected_value, actual_value
                            );
                            }
                        }
                    } else {
                        panic!("Account {:?} not found in EVM state", addr);
                    }
                }
            }

            let index = match s {
                StateChangeSource::Transaction(index) => index,
                StateChangeSource::PostBlock(_) => self.unfilled_tx_indices.len(),
                StateChangeSource::PreBlock(_) => 0,
            };

            self.next_state_diff = Some((
                index as u64,
                StateDiff {
                    mutations: _state
                        .iter()
                        .map(|(a, s)| {
                            let storage_writes = if s.storage.is_empty() {
                                None
                            } else {
                                Some(
                                    s.storage
                                        .iter()
                                        .map(|(k, v)| ((*k).into(), B256::from(v.present_value)))
                                        .collect(),
                                )
                            };
                            (
                                *a,
                                AccountState {
                                    nonce: Some(s.info.nonce),
                                    balance: Some(s.info.balance),
                                    code: s.info.code.as_ref().map(|c| c.bytes().clone()),
                                    storage_writes,
                                },
                            )
                        })
                        .collect::<HashMap<Address, AccountState>>(),
                },
            ));

            // Clear next_state_diff after checking
            self.invalidated_state_diffs.remove_entry(&(index as u64));
            self.payload_tx_validated.insert(index as u64, true);
        }
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
pub struct BalPayloadValidator<'a, P> {
    reader: BalReader<'a, P>,
    evm_config: OpEvmConfig,
    chain_spec: OpChainSpec,
    active_payload: FlashblocksPayloadV1,
    aggregated_payload: Vec<FlashblocksPayloadV1>,
    evm_env: EvmEnv<OpSpecId>,
    execution_ctx: OpBlockExecutionCtx,
    pending_block: Option<ExecutedBlockWithTrieUpdates<reth_optimism_primitives::OpPrimitives>>,
}

impl<'a, P> BalPayloadValidator<'a, P>
where
    P: StateProvider + HeaderProvider<Header = alloy_consensus::Header> + Clone + Send + Sync,
{
    /// Creates a new coordinator from a BAL reader and payload.
    pub fn new(
        reader: BalReader<'a, P>,
        evm_config: OpEvmConfig,
        chain_spec: OpChainSpec,
        active_payload: FlashblocksPayloadV1,
        aggregated_payload: Vec<FlashblocksPayloadV1>,
    ) -> Self {
        Self {
            reader,
            evm_config,
            chain_spec,
            active_payload,
            aggregated_payload,
            pending_block: None,
            evm_env: EvmEnv::default(),
            execution_ctx: OpBlockExecutionCtx::default(),
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
        self: Arc<Self>,
        payload: &FlashblocksPayloadV1,
    ) -> eyre::Result<()> {
        let bal_payload = payload.diff.flash_bal.clone();

        let bal_hash = payload.diff.bal_hash;
        let buff = &[0u8; 32][..];
        let computed_bal_hash = keccak256(buff);
        if bal_hash != computed_bal_hash {
            return Err(eyre!(
                "BAL hash mismatch: expected {:?}, got {:?}",
                bal_hash,
                computed_bal_hash
            ));
        }

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

        let parent_header = self
            .reader()
            .provider()
            .header(&payload.base.as_ref().unwrap().parent_hash)?
            .ok_or_else(|| eyre!("Parent header not found"))?;

        let pending = PendingBlock::new();

        let state_hook = Box::new(pending.clone().on_state_root());
        let result_hook = Box::new(pending.clone().on_result());

        // Step 2: Execute state root computation and transaction execution in parallel
        let this = self.clone();
        let (state_root_result, tx_results) = rayon::join(
            || this.compute_state_root(state_hook),
            || {
                this.execute_all_transactions(
                    tx_iter,
                    &parent_header,
                    Box::new(pending.clone()),
                    result_hook,
                )
            },
        );

        let tx_results = pending.clone().results.lock().clone();

        // Step 3: Validate state diffs from execution match BAL
        self.validate_execution_diffs(&tx_results)?;

        // Step 4: Build final ExecutedBlockWithTrieUpdates
        if let Some(pending_block) = &self.pending_block {
            self.build_block(tx_results, pending_block.recovered_block().state_root())?;
        }
        Ok(())
    }

    /// Computes the state root using the BAL reader.
    ///
    /// This can run in parallel with transaction execution.
    fn compute_state_root(
        &self,
        state_hook: Box<dyn Fn(StateRootResult) + Send + Sync + 'static>,
    ) -> Result<()> {
        let (state_root, prestate_load_time, root_update_time) = self.reader.state_root()?;
        state_hook(StateRootResult {
            state_root,
            prestate_load_time,
            root_update_time,
        });

        Ok(())
    }

    /// Executes all transactions in parallel.
    ///
    /// Returns results for each transaction including their state diffs.
    fn execute_all_transactions<I>(
        &self,
        transactions: I,
        parent_header: &Header,
        state_hook: Box<PendingBlock>,
        result_hook: Box<impl Fn(TransactionExecutionResult) + Clone + Send + Sync + 'static>,
    ) where
        I: IntoParallelIterator<Item = (usize, Recovered<OpTxEnvelope>)>,
    {
        let this = Arc::new(self);
        transactions.into_par_iter().for_each(|(i, tx)| {
            let state_hook = state_hook.clone();
            let tx = tx.clone();
            let this = this.clone();
            let result_hook = result_hook.clone();
            let _ =
                this.execute_transaction_at_index(i as u64, parent_header, move |mut executor| {
                    let tx_envelope = tx.clone();
                    let exec_result = executor
                        .execute_transaction(tx_envelope)
                        .inspect_err(|e| {
                            eprintln!("Error executing transaction at index {}: {:?}", i, e);
                        })
                        .unwrap_or(0);

                    executor.set_state_hook(Some(state_hook));

                    let res = executor
                        .finish()
                        .map(|(fees, receipts)| {
                            TransactionExecutionResult {
                                index: i as u64,
                                state_diff: StateDiff::default(), // Placeholder, would be filled in
                                gas_used: exec_result,
                                receipt: receipts.receipts,
                            }
                        })
                        .map_err(|e| eyre!("Execution error: {:?}", e));

                    let res = res.unwrap();

                    result_hook(res);
                });
        });
    }

    /// Validates that the state diffs from execution match the BAL.
    ///
    /// For each transaction, compares the computed state diff against
    /// the expected state diff from the BAL.
    fn validate_execution_diffs(&self, tx_results: &[TransactionExecutionResult]) -> Result<()> {
        for result in tx_results {
            // Get expected state diff from BAL
            let expected_diff = self.reader.changes_at(result.index);

            // Validate that execution result matches BAL
            self.reader
                .validate_state_diff(result.index, &expected_diff)?;
        }

        Ok(())
    }

    /// Executes a single transaction at the given index using the BAL pre-state.
    fn execute_transaction_at_index<F>(
        &self,
        tx_index: u64,
        header: &Header,
        f: F,
    ) -> Result<TransactionExecutionResult>
    where
        F: FnOnce(
                FlashblocksBlockExecutor<
                    OpEvm<&mut State<StateProviderDatabase<P>>, BalInspector, PrecompilesMap>,
                    OpRethReceiptBuilder,
                    OpChainSpec,
                >,
            ) + Send
            + 'a,
    {
        // 1. Reconstruct pre-state bundle for this transaction (state at tx_index - 1)
        let pre_state_index = if tx_index == 0 { 0 } else { tx_index - 1 };
        let bundle_prestate = self.reader.bundle_pre_state_at(pre_state_index)?;

        // 2. Initialize EVM with this pre-state

        let state = StateProviderDatabase::new(self.reader().provider().clone());
        let mut state = State::builder()
            .with_database(state)
            .with_bundle_prestate(bundle_prestate)
            .with_bundle_update()
            .build();

        let parent = self
            .reader()
            .provider()
            .sealed_header_by_hash(header.parent_hash)?
            .ok_or_else(|| eyre!("Parent header not found"))?;

        let attributes = OpNextBlockEnvAttributes::build_pending_env(&parent);

        let execution_ctx = self
            .evm_config
            .context_for_next_block(&parent, attributes.clone())?;

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

        f(block_executor);

        // Get the state diff from BAL
        let state_diff = self.reader.changes_at(tx_index);

        let receipt = OpReceipt::Eip1559(alloy_consensus::Receipt {
            status: alloy_consensus::Eip658Value::Eip658(true),
            cumulative_gas_used: 0, // Will be filled in during aggregation
            logs: vec![],           // Would come from actual execution
        });

        Ok(TransactionExecutionResult {
            index: tx_index,
            state_diff,
            gas_used: 21000, // Placeholder - would come from actual execution
            receipt: vec![receipt],
        })
    }

    /// Builds the final ExecutedBlockWithTrieUpdates from validated results.
    fn build_block(
        &self,
        tx_results: Vec<TransactionExecutionResult>,
        state_root_result: B256,
    ) -> Result<ExecutedBlockWithTrieUpdates<OpPrimitives>> {
        // Aggregate receipts and compute cumulative gas
        let mut cumulative_gas_used = 0u64;
        let mut receipts = Vec::new();

        for result in &tx_results {
            cumulative_gas_used += result.gas_used;

            receipts.push(result.receipt.clone());
        }

        // Build final bundle state from BAL
        let num_txs = self.active_payload.diff.transactions.len() as u64;
        let last_tx_index = num_txs - 1;

        let mut final_bundle = BundleState::default();
        let modified_accounts = self.reader.modified_accounts();

        for addr in modified_accounts {
            let diff = self.reader.read_account_diff(addr, last_tx_index);
            let base_info = self.reader.state_provider.basic_account(&addr)?.map(|a| {
                reth::revm::state::AccountInfo {
                    nonce: a.nonce,
                    balance: a.balance,
                    code_hash: a.bytecode_hash.unwrap_or(B256::ZERO),
                    code: None,
                }
            });

            let present_info = self.reader.init_mutated_account_from_diff(
                addr,
                base_info.clone(),
                diff.as_ref(),
            )?;

            let storage = self
                .reader
                .reconstruct_storage_from_diff(addr, diff.as_ref())?;

            let status = if diff.is_some() {
                reth::revm::db::AccountStatus::Changed
            } else {
                reth::revm::db::AccountStatus::Loaded
            };

            let bundle_account = BundleAccount::new(
                base_info,
                Some(present_info),
                storage.into_iter().collect(),
                status,
            );

            final_bundle.state.insert(addr, bundle_account);
        }

        // Calculate trie updates
        let hashed_state = self.reader.state_provider.hashed_post_state(&final_bundle);
        let (_, trie_updates) = self
            .reader
            .state_provider
            .state_root_with_updates(hashed_state.clone())?;

        // Decode transactions for the block
        let transactions: Vec<_> = self
            .active_payload
            .diff
            .transactions
            .iter()
            .map(|bytes| {
                let mut slice = bytes.as_ref();
                OpTxEnvelope::decode_2718(&mut slice)
                    .map_err(|e| eyre!("Failed to decode transaction: {}", e))
            })
            .collect::<Result<Vec<_>>>()?;

        // Build block header (using data from payload)
        let base = self
            .active_payload
            .base
            .as_ref()
            .ok_or_else(|| eyre!("Payload missing base"))?;

        let header = Header {
            parent_hash: base.parent_hash,
            ommers_hash: alloy_primitives::b256!(
                "1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347"
            ), // Empty ommers
            beneficiary: base.fee_recipient,
            state_root: state_root_result,
            transactions_root: self.active_payload.diff.block_hash, // Placeholder
            receipts_root: self.active_payload.diff.receipts_root,
            logs_bloom: self.active_payload.diff.logs_bloom,
            difficulty: U256::ZERO,
            number: base.block_number,
            gas_limit: base.gas_limit,
            gas_used: self.active_payload.diff.gas_used,
            timestamp: base.timestamp,
            extra_data: base.extra_data.clone(),
            mix_hash: base.prev_randao,
            nonce: alloy_primitives::B64::ZERO,
            base_fee_per_gas: Some(base.base_fee_per_gas.to()),
            withdrawals_root: Some(self.active_payload.diff.withdrawals_root),
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: Some(base.parent_beacon_block_root),
            requests_hash: None,
        };

        let sealed_header = SealedHeader::new(header.clone(), self.active_payload.diff.block_hash);

        // Create block body
        let body = BlockBody {
            transactions: transactions.clone(),
            ommers: vec![],
            withdrawals: Some(Withdrawals::new(
                self.active_payload.diff.withdrawals.clone(),
            )),
        };

        // Create block with transactions
        let block = Block {
            header: header.clone(),
            body,
        };

        // Extract senders from transactions
        let senders: Vec<_> = block
            .body
            .transactions
            .iter()
            .cloned()
            .map(|tx| tx.recover_signer().unwrap_or_default())
            .collect();

        let block_assembler = OpBlockAssembler::new(self.chain_spec.clone().into());

        let result = block_assembler.assemble_block(BlockAssemblerInput::<
            FlashblocksBlockExecutorFactory,
            Header,
        >::new(
            self.evm_env.clone(),
            self.execution_ctx.clone(),
            &SealedHeader::new(header.clone(), header.hash_slow()),
            transactions.clone(),
            &BlockExecutionResult::<OpReceipt> {
                receipts: self
                    .pending_block
                    .as_ref()
                    .map(|b| &b.execution_outcome().receipts)
                    .unwrap()
                    .iter()
                    .flatten()
                    .cloned()
                    .collect(),
                gas_used: cumulative_gas_used,
                requests: Requests::default(),
            },
            &self
                .pending_block
                .as_ref()
                .map(|b| &b.execution_outcome().bundle)
                .cloned()
                .unwrap_or_default(),
            &self.reader().state_provider.clone() as &dyn StateProvider,
            self.pending_block
                .as_ref()
                .map(|b| b.sealed_block().state_root())
                .unwrap_or_default(),
        ))?;

        Ok(ExecutedBlockWithTrieUpdates {
            block: ExecutedBlock {
                recovered_block: result.try_into_recovered().unwrap().into(),
                hashed_state: hashed_state.clone().into(),
                execution_output: Arc::new(
                    self.pending_block
                        .as_ref()
                        .map(|b| b.execution_outcome().clone())
                        .unwrap_or_default(),
                ),
            },
            trie: ExecutedTrieUpdates::Present(Arc::new(trie_updates)),
        })
    }

    /// Returns the underlying BAL reader.
    pub fn reader(&self) -> &BalReader<'a, P> {
        &self.reader
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
