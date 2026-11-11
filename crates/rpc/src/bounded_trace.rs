//! Inert, fallible projection of native call/prestate data; no RPC registration or worker owner.
//!
//! Portions are modified from revm-inspectors 0.43.0 (MIT), revision
//! 453c67d7ccdf51327c9e7687ac6ba0b8651e7f87, src/tracing/builder/geth.rs.
//! See THIRD_PARTY_NOTICES.md and docs/bounded-native-trace-projection.md.

use alloy_primitives::{B256, Bytes, U256};
use alloy_rpc_types_trace::geth::{
    AccountState, CallConfig, CallFrame, CallLogFrame, DiffMode, PreStateConfig, PreStateFrame,
    PreStateMode,
};
use revm::{
    DatabaseRef,
    bytecode::Bytecode,
    context_interface::result::ResultGas,
    primitives::{KECCAK_EMPTY, hardfork::SpecId},
    state::{AccountInfo, EvmState},
};
use revm_inspectors::tracing::types::CallTraceNode;
use std::{error::Error, fmt};

/// Cumulative materialized entries, including diff entries later filtered out.
/// Variable byte payloads are decoded retained data, not JSON-encoded response bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectionCounts {
    pub frames_and_logs: usize,
    pub accounts: usize,
    pub storage_entries: usize,
    pub data_bytes: usize,
}

/// Caller-selected bounds. Call depth counts EVM parent edges, not JSON nesting levels.
#[derive(Clone, Copy, Debug)]
pub struct ProjectionLimits {
    pub counts: ProjectionCounts,
    pub max_call_depth: usize,
}

/// Reuse across every projected item of one operation; errors never refund consumed work.
#[derive(Debug)]
pub struct ProjectionBudget {
    limits: ProjectionLimits,
    used: ProjectionCounts,
}

impl ProjectionBudget {
    pub const fn new(limits: ProjectionLimits) -> Self {
        Self {
            limits,
            used: ProjectionCounts {
                frames_and_logs: 0,
                accounts: 0,
                storage_entries: 0,
                data_bytes: 0,
            },
        }
    }

    pub const fn used(&self) -> ProjectionCounts {
        self.used
    }

    fn charge(
        &mut self,
        resource: ProjectionResource,
        amount: usize,
    ) -> Result<(), ProjectionError> {
        let (used, limit) = match resource {
            ProjectionResource::FramesAndLogs => (
                &mut self.used.frames_and_logs,
                self.limits.counts.frames_and_logs,
            ),
            ProjectionResource::Accounts => (&mut self.used.accounts, self.limits.counts.accounts),
            ProjectionResource::StorageEntries => (
                &mut self.used.storage_entries,
                self.limits.counts.storage_entries,
            ),
            ProjectionResource::DataBytes => {
                (&mut self.used.data_bytes, self.limits.counts.data_bytes)
            }
            ProjectionResource::CallDepth => return Err(ProjectionError::LimitExceeded(resource)),
        };
        let next = used
            .checked_add(amount)
            .ok_or(ProjectionError::LimitExceeded(resource))?;
        if next > limit {
            return Err(ProjectionError::LimitExceeded(resource));
        }
        *used = next;
        Ok(())
    }

    fn depth(&self, depth: usize) -> Result<(), ProjectionError> {
        if depth > self.limits.max_call_depth {
            return Err(ProjectionError::LimitExceeded(
                ProjectionResource::CallDepth,
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionResource {
    FramesAndLogs,
    Accounts,
    StorageEntries,
    DataBytes,
    CallDepth,
}

/// Display/Debug redact provider data; the original provider failure remains an error source.
pub enum ProjectionError {
    Cancelled,
    DeadlineExceeded,
    LimitExceeded(ProjectionResource),
    InvalidArena,
    Provider(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("trace projection cancelled"),
            Self::DeadlineExceeded => f.write_str("trace projection deadline exceeded"),
            Self::LimitExceeded(resource) => {
                write!(f, "trace projection limit exceeded: {resource:?}")
            }
            Self::InvalidArena => f.write_str("invalid trace arena"),
            Self::Provider(_) => f.write_str("trace projection provider unavailable"),
        }
    }
}

impl fmt::Debug for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Error for ProjectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// Project a native arena, checking each visited frame/log and assembly step.
/// `onlyTopCall` intentionally visits only the root, like the pinned native builder.
pub fn project_call_trace(
    nodes: &[CallTraceNode],
    config: CallConfig,
    gas: ResultGas,
    spec: SpecId,
    budget: &mut ProjectionBudget,
    checkpoint: &mut impl FnMut() -> Result<(), ProjectionError>,
) -> Result<CallFrame, ProjectionError> {
    checkpoint()?;
    let Some(root) = nodes.first() else {
        budget.charge(ProjectionResource::FramesAndLogs, 1)?;
        return Ok(CallFrame::default());
    };
    if root.parent.is_some() || root.idx != 0 || root.trace.depth != 0 {
        return Err(ProjectionError::InvalidArena);
    }
    let only_top = config.only_top_call.unwrap_or_default();
    let mut frames: Vec<CallFrame> = Vec::new();
    let mut failed = Vec::new();
    for (index, node) in nodes
        .iter()
        .take(if only_top { 1 } else { nodes.len() })
        .enumerate()
    {
        checkpoint()?;
        if node.idx != index {
            return Err(ProjectionError::InvalidArena);
        }
        let parent_failed = match node.parent {
            None if index == 0 => false,
            Some(parent) if parent < index => {
                let parent_node = nodes.get(parent).ok_or(ProjectionError::InvalidArena)?;
                if parent_node.trace.depth.checked_add(1) != Some(node.trace.depth) {
                    return Err(ProjectionError::InvalidArena);
                }
                *failed.get(parent).ok_or(ProjectionError::InvalidArena)?
            }
            _ => return Err(ProjectionError::InvalidArena),
        };
        budget.depth(node.trace.depth)?;
        budget.charge(ProjectionResource::FramesAndLogs, 1)?;
        budget.charge(ProjectionResource::DataBytes, node.trace.data.len())?;
        budget.charge(ProjectionResource::DataBytes, node.trace.output.len())?;
        if !node.trace.success {
            // Pinned alloy-sol-types 1.6.1 uses lossy UTF-8 (at most 3*n).
            // A valid Panic needs >=36 input bytes and its reason is <=86 bytes,
            // also covered by 3*n. GenericRevertReason cannot decode custom errors.
            let reason_bound =
                node.trace
                    .output
                    .len()
                    .checked_mul(3)
                    .ok_or(ProjectionError::LimitExceeded(
                        ProjectionResource::DataBytes,
                    ))?;
            budget.charge(ProjectionResource::DataBytes, reason_bound)?;
        }
        let is_failed = parent_failed || node.trace.is_error();
        let mut frame = node.geth_empty_call_frame(false);
        checkpoint()?;
        // Native root logs are retained even when the root failed.
        if config.with_log.unwrap_or_default() && (index == 0 || !is_failed) {
            for log in &node.logs {
                checkpoint()?;
                budget.charge(ProjectionResource::FramesAndLogs, 1)?;
                let topic_bytes = log.raw_log.topics().len().checked_mul(32).ok_or(
                    ProjectionError::LimitExceeded(ProjectionResource::DataBytes),
                )?;
                budget.charge(ProjectionResource::DataBytes, topic_bytes)?;
                budget.charge(ProjectionResource::DataBytes, log.raw_log.data.len())?;
                frame.logs.push(CallLogFrame {
                    address: Some(log.address),
                    topics: Some(log.raw_log.topics().to_vec()),
                    data: Some(log.raw_log.data.clone()),
                    position: Some(log.position),
                    index: Some(log.index),
                });
            }
        }
        if let Some(selfdestruct) = node.geth_selfdestruct_call_trace() {
            checkpoint()?;
            budget.charge(ProjectionResource::FramesAndLogs, 1)?;
            frame.calls.push(selfdestruct);
        }
        if index == 0 {
            frame.gas_used = U256::from(gas.tx_gas_used());
            if SpecId::is_enabled_in(spec, SpecId::AMSTERDAM) {
                frame.execution_gas_used = Some(U256::from(gas.block_regular_gas_used()));
                frame.state_gas_used = Some(U256::from(gas.block_state_gas_used()));
                frame.gas_refund = Some(U256::from(gas.final_refunded()));
            }
        }
        frames.push(frame);
        failed.push(is_failed);
    }
    while let Some(mut frame) = frames.pop() {
        checkpoint()?;
        // Children arrived in reverse order; avoid quadratic front-insertion.
        for left in 0..frame.calls.len() / 2 {
            checkpoint()?;
            let right = frame.calls.len() - 1 - left;
            frame.calls.swap(left, right);
        }
        let node = nodes
            .get(frames.len())
            .ok_or(ProjectionError::InvalidArena)?;
        if let Some(parent) = node.parent {
            frames
                .get_mut(parent)
                .ok_or(ProjectionError::InvalidArena)?
                .calls
                .push(frame);
        } else {
            checkpoint()?;
            return Ok(frame);
        }
    }
    Err(ProjectionError::InvalidArena)
}

/// Project prestate/diff with checks at every account, storage and provider boundary.
/// Shared counts include materialized pre+post entries even when native diff filtering removes them.
pub fn project_prestate<DB: DatabaseRef>(
    state: &EvmState,
    config: PreStateConfig,
    db: &DB,
    budget: &mut ProjectionBudget,
    checkpoint: &mut impl FnMut() -> Result<(), ProjectionError>,
) -> Result<PreStateFrame, ProjectionError> {
    checkpoint()?;
    let mut result = DiffMode::default();
    for (address, account) in state {
        checkpoint()?;
        budget.charge(ProjectionResource::Accounts, 1)?;
        let original = provider_read(checkpoint, || db.basic_ref(*address))?.unwrap_or_default();
        let code = if config.code_enabled() {
            load_code(db, &original, budget, checkpoint)?
        } else {
            None
        };
        let mut pre = AccountState::from_account_info(original.nonce, original.balance, code);
        let mut post = if config.is_diff_mode() {
            budget.charge(ProjectionResource::Accounts, 1)?;
            let code = if config.code_enabled() {
                if account.info.code_hash == original.code_hash {
                    if let Some(code) = &pre.code {
                        budget.charge(ProjectionResource::DataBytes, code.len())?;
                    }
                    pre.code.clone()
                } else if let Some(code) = &account.info.code {
                    Some(copy_code(code, budget)?)
                } else {
                    None
                }
            } else {
                None
            };
            AccountState::from_account_info(account.info.nonce, account.info.balance, code)
        } else {
            AccountState::default()
        };
        if config.storage_enabled() {
            for (key, slot) in &account.storage {
                checkpoint()?;
                if config.is_diff_mode() && !slot.is_changed() {
                    continue;
                }
                budget.charge(ProjectionResource::StorageEntries, 1)?;
                pre.storage
                    .insert((*key).into(), slot.original_value.into());
                if config.is_diff_mode() {
                    budget.charge(ProjectionResource::StorageEntries, 1)?;
                    post.storage
                        .insert((*key).into(), slot.present_value.into());
                }
            }
        }
        if !config.is_diff_mode() {
            result.pre.insert(*address, pre);
            continue;
        }
        checkpoint()?;
        if !account.is_selfdestructed() && pre == post {
            continue;
        }
        // Match native filtering order: compare full states before removing zero storage.
        for entry in [&mut pre, &mut post] {
            for (key, value) in std::mem::take(&mut entry.storage) {
                checkpoint()?;
                if value != B256::ZERO {
                    entry.storage.insert(key, value);
                }
            }
        }
        if !account.is_selfdestructed() {
            checkpoint()?;
            post.remove_matching_account_info(&pre);
            checkpoint()?;
            result.post.insert(*address, post);
        }
        let was_empty =
            original.balance.is_zero() && original.nonce == 0 && original.code_hash == KECCAK_EMPTY;
        if !(account.is_created() && was_empty) {
            result.pre.insert(*address, pre);
        }
    }
    checkpoint()?;
    if config.is_diff_mode() {
        Ok(PreStateFrame::Diff(result))
    } else {
        Ok(PreStateFrame::Default(PreStateMode(result.pre)))
    }
}

fn copy_code(code: &Bytecode, budget: &mut ProjectionBudget) -> Result<Bytes, ProjectionError> {
    budget.charge(
        ProjectionResource::DataBytes,
        code.original_byte_slice().len(),
    )?;
    Ok(code.original_bytes())
}

fn load_code<DB: DatabaseRef>(
    db: &DB,
    info: &AccountInfo,
    budget: &mut ProjectionBudget,
    checkpoint: &mut impl FnMut() -> Result<(), ProjectionError>,
) -> Result<Option<Bytes>, ProjectionError> {
    checkpoint()?;
    if let Some(code) = &info.code {
        return copy_code(code, budget).map(Some);
    }
    if info.code_hash == KECCAK_EMPTY {
        return Ok(None);
    }
    let code = provider_read(checkpoint, || db.code_by_hash_ref(info.code_hash))?;
    copy_code(&code, budget).map(Some)
}

fn provider_read<T, E: Error + Send + Sync + 'static>(
    checkpoint: &mut impl FnMut() -> Result<(), ProjectionError>,
    read: impl FnOnce() -> Result<T, E>,
) -> Result<T, ProjectionError> {
    checkpoint()?;
    let result = read();
    let after = checkpoint();
    // Retain a provider error as the cause even if cancellation also arrived during the call.
    let value = result.map_err(|error| ProjectionError::Provider(Box::new(error)))?;
    after?;
    Ok(value)
}

#[cfg(test)]
mod tests;
