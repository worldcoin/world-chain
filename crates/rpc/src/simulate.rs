use alloy_consensus::BlockHeader;
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_rpc_types::{BlockId, BlockNumberOrTag};
use jsonrpsee::{
    core::{RpcResult, async_trait},
    proc_macros::rpc,
};
use lru::LruCache;
use op_revm::{OpSpecId, OpTransaction};
use reth_evm::{ConfigureEvm, Evm as RethEvm, EvmFactory};
use reth_optimism_evm::OpEvmConfig;
use reth_provider::{BlockReaderIdExt, HeaderProvider, StateProviderFactory};
use reth_revm::{State, database::StateProviderDatabase};
use reth_rpc_eth_api::helpers::SpawnBlocking;
use reth_tasks::pool::{BlockingTaskGuard, BlockingTaskPool};
use revm::{
    Inspector,
    context::{
        CfgEnv, TxEnv,
        result::{ExecutionResult, Output},
    },
    interpreter::{CallInputs, CallOutcome, InstructionResult},
};
use revm_primitives::TxKind;
use serde::{Deserialize, Serialize};
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

// ═══════════════════════════════════════════════════════════════════════════════
// Request types
// ═══════════════════════════════════════════════════════════════════════════════

/// Request for `simulate_unsignedUserOp`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulateUnsignedUserOpRequest {
    /// The smart account address to call.
    pub sender: Address,
    /// The encoded execution payload (e.g. `executeUserOp` calldata).
    pub call_data: Bytes,
    /// The ERC-4337 EntryPoint address used as `msg.sender` in the simulation.
    pub entry_point: Address,
    /// Optional gas limit for the simulated call. Defaults to `MAX_SIMULATION_GAS`.
    /// Must not exceed `MAX_SIMULATION_GAS`.
    #[serde(default)]
    pub call_gas_limit: Option<U256>,
}

/// Maximum gas limit accepted for a simulated call.
pub const MAX_SIMULATION_GAS: u64 = 8_000_000;

// ═══════════════════════════════════════════════════════════════════════════════
// Response types
// ═══════════════════════════════════════════════════════════════════════════════

/// The standard a token/asset conforms to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssetType {
    Native,
    Erc20,
    Erc721,
    Erc1155,
}

/// Token/asset metadata resolved from on-chain state.
///
/// `address` is the contract address for ERC-20/721/1155 and `Address::ZERO`
/// for native ETH.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetInfo {
    pub address: Address,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    #[serde(rename = "type")]
    pub asset_type: AssetType,
}

/// A discrete asset transfer detected during simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetChange {
    #[serde(rename = "type")]
    pub change_type: AssetType,
    pub from: Address,
    pub to: Address,
    pub raw_amount: String,
    /// Per-asset id inside a multi-token contract: ERC-721 NFT id or
    /// ERC-1155 token-class id. `None` for ERC-20 and native ETH, where the
    /// contract address (or absence of one) fully identifies the asset.
    /// Decimal string so JS clients don't truncate ids ≥ 2^53.
    pub token_id: Option<String>,
    pub asset: AssetInfo,
}

/// An ERC-20/721/1155 token approval detected during simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExposureChange {
    pub owner: Address,
    pub spender: Address,
    pub raw_amount: String,
    pub is_approved_for_all: bool,
    pub asset: AssetInfo,
}

/// A call made during execution, forming a full stack trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEntry {
    pub from: Address,
    pub to: Address,
    pub method: Option<String>,
    pub selector: String,
    pub value: String,
}

/// Outcome of the simulation execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SimulationStatus {
    Success,
    Revert,
}

/// Full response for `simulate_unsignedUserOp`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulateUnsignedUserOpResult {
    pub status: SimulationStatus,
    /// Decoded reason for a top-level revert/halt; `None` on success.
    /// For nested reverts this is the *deepest* reverted frame's payload
    /// (the root cause), not the outermost — important for ERC-4337 where
    /// EntryPoint always wraps inner reverts in `FailedOp(...)`.
    pub revert_reason: Option<String>,
    pub block_number: u64,
    pub gas_used: String,
    pub asset_changes: Vec<AssetChange>,
    pub exposure_changes: Vec<ExposureChange>,
    pub trace: Vec<TraceEntry>,
    /// Warning generation is not yet implemented. Always serialized as `[]`;
    /// reserved so callers can rely on the field being present.
    pub warnings: Vec<serde_json::Value>,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Simulation inspector — captures call trace and native ETH transfers
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug)]
struct RawTrace {
    from: Address,
    to: Address,
    selector: [u8; 4],
    value: U256,
}

#[derive(Debug)]
struct NativeTransfer {
    from: Address,
    to: Address,
    value: U256,
}

/// Captures the call stack and native ETH transfers of a single simulation.
///
/// Single-threaded by construction — the EVM drives it from one call site,
/// and the caller recovers it via `evm.components_mut()` once `transact`
/// returns. No interior mutability needed.
#[derive(Debug, Default)]
pub struct SimulationInspector {
    /// Full call stack trace — every CALL/STATICCALL/DELEGATECALL at every depth.
    /// Includes reverted calls so debug consumers can see what was attempted.
    traces: Vec<RawTrace>,
    /// Native ETH transfers from successfully completed call frames only.
    /// Reverted/halted frames don't actually move ETH, so their tentative
    /// entries in `pending_frames` are dropped instead of being committed here.
    native_transfers: Vec<NativeTransfer>,
    /// One entry per active call frame, holding tentative native transfers
    /// recorded on entry. On `call_end` the frame is popped: committed (or
    /// merged into the parent frame) on success, dropped on revert/halt.
    pending_frames: Vec<Vec<NativeTransfer>>,
    /// Raw payload of the deepest frame that exited via REVERT. Set on the
    /// first non-ok `call_end` whose `InstructionResult` is `Revert` — since
    /// `call_end` fires bottom-up, that's the innermost reverter and so the
    /// root cause when wrappers like EntryPoint's `FailedOp(...)` re-revert
    /// up the stack. Halt frames (OOG, invalid opcode, etc.) are skipped:
    /// they have no payload to decode.
    deepest_revert_payload: Option<Bytes>,
}

impl SimulationInspector {
    pub fn take_trace_entries(&mut self) -> Vec<TraceEntry> {
        std::mem::take(&mut self.traces)
            .into_iter()
            .map(|t| TraceEntry {
                from: t.from,
                to: t.to,
                method: selector_to_name(t.selector).map(str::to_string),
                selector: format!("0x{}", hex::encode(t.selector)),
                value: format!("{:#x}", t.value),
            })
            .collect()
    }

    pub fn take_native_asset_changes(&mut self) -> Vec<AssetChange> {
        std::mem::take(&mut self.native_transfers)
            .into_iter()
            .map(|t| AssetChange {
                change_type: AssetType::Native,
                from: t.from,
                to: t.to,
                raw_amount: t.value.to_string(),
                token_id: None,
                asset: AssetInfo {
                    address: Address::ZERO,
                    symbol: "ETH".to_string(),
                    name: "Ether".to_string(),
                    decimals: 18,
                    asset_type: AssetType::Native,
                },
            })
            .collect()
    }

    /// Take the decoded reason of the deepest reverted frame, if any.
    pub fn take_deepest_revert_reason(&mut self) -> Option<String> {
        self.deepest_revert_payload
            .take()
            .map(|output| decode_revert_reason(&output))
    }
}

impl<CTX: revm::context_interface::ContextTr> Inspector<CTX> for SimulationInspector {
    fn call(&mut self, context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        use revm::context_interface::LocalContextTr;

        // Record every call at every depth for a full stack trace.
        // `CallInput::SharedBuffer` is a range into the EVM shared-memory
        // buffer (an internal optimization that avoids cloning calldata for
        // child calls). Without explicit handling those calls would record a
        // null selector — making methods like `addOwnerWithThreshold`
        // invisible to the backend's forbidden-method detection.
        // We resolve only the first 4 bytes; the full calldata isn't needed.
        let selector = match &inputs.input {
            revm::interpreter::CallInput::Bytes(b) if b.len() >= 4 => {
                let mut sel = [0u8; 4];
                sel.copy_from_slice(&b[..4]);
                sel
            }
            revm::interpreter::CallInput::SharedBuffer(range) if range.len() >= 4 => {
                let head = range.start..range.start + 4;
                let mut sel = [0u8; 4];
                if let Some(slice) = context.local().shared_memory_buffer_slice(head) {
                    sel.copy_from_slice(&slice);
                }
                sel
            }
            _ => [0u8; 4],
        };
        let value = inputs.value.transfer().unwrap_or(U256::ZERO);
        self.traces.push(RawTrace {
            from: inputs.caller,
            to: inputs.target_address,
            selector,
            value,
        });

        // Open a new frame for tentative native transfers. If this call frame
        // reverts, `call_end` will drop the frame; only successful frames
        // commit their transfers.
        let mut frame = Vec::new();
        if let Some(value) = inputs.value.transfer()
            && value > U256::ZERO
        {
            frame.push(NativeTransfer {
                from: inputs.caller,
                to: inputs.target_address,
                value,
            });
        }
        self.pending_frames.push(frame);

        None
    }

    fn call_end(&mut self, _context: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        let Some(frame) = self.pending_frames.pop() else {
            return;
        };
        let result = outcome.instruction_result();
        if !result.is_ok() {
            // Frame reverted or halted — drop tentative native transfers
            // either way. For explicit REVERTs, capture the deepest payload
            // (the first one we see, since `call_end` fires bottom-up) so
            // wrappers like EntryPoint's `FailedOp(...)` don't mask the
            // root cause. Halts and the other `is_revert()` variants
            // (CallTooDeep, OutOfFunds, EOF init-code) have empty outputs.
            if matches!(result, InstructionResult::Revert) && self.deepest_revert_payload.is_none()
            {
                self.deepest_revert_payload = Some(outcome.output().clone());
            }
            return;
        }
        // Successful frame: bubble transfers up to the parent's tentative list,
        // or commit them as final if this was the outermost frame.
        if let Some(parent) = self.pending_frames.last_mut() {
            parent.extend(frame);
        } else {
            self.native_transfers.extend(frame);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// RPC trait
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg_attr(not(test), rpc(server, namespace = "simulate"))]
#[cfg_attr(test, rpc(server, client, namespace = "simulate"))]
#[async_trait]
pub trait SimulateApi {
    /// Simulates an unsigned ERC-4337 v0.7 PackedUserOperation against the
    /// specified block state. Returns asset transfers, approval changes,
    /// decoded trace, and warnings.
    #[method(name = "unsignedUserOp")]
    async fn simulate_unsigned_user_op(
        &self,
        request: SimulateUnsignedUserOpRequest,
    ) -> RpcResult<SimulateUnsignedUserOpResult>;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Implementation
// ═══════════════════════════════════════════════════════════════════════════════

/// Maximum number of token metadata entries kept across requests.
///
/// Bounded to prevent unbounded memory growth from adversarial UserOps that
/// emit Transfer events from many fresh token contracts.
const METADATA_CACHE_CAPACITY: usize = 1000;

/// Cross-request cache for resolved token metadata. LRU-bounded.
type MetadataCache = Arc<Mutex<LruCache<Address, AssetInfo>>>;

/// Hard wall-clock cap on a single simulation, observed by the client. Beyond
/// this we return `internal error: simulation deadline exceeded`. The blocking
/// task continues to drain on the rayon pool until the EVM finishes (bounded
/// by `MAX_SIMULATION_GAS`), holding its concurrency permit until then so the
/// guard correctly accounts for slow simulations.
pub const SIMULATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Relax EVM rules so simulations succeed regardless of the caller's gas
/// pricing, balance, or block limits — matching `eth_call` semantics. The
/// `disable_fee_charge` flag is the critical one on Optimism: without it
/// op-revm's handler computes the L1 data fee from `enveloped_tx` and
/// rejects the tx with `LackOfFundForMaxFee` because the synthetic caller
/// (EntryPoint, `Address::ZERO`) has no ETH. `disable_balance_check`
/// covers the L2 path symmetrically.
///
/// Exposed so fork-based integration tests can mirror the exact prod cfg
/// instead of redeclaring the flag list (which silently drifts).
pub fn relax_cfg_for_simulation(cfg_env: &mut CfgEnv<OpSpecId>) {
    cfg_env.disable_block_gas_limit = true;
    cfg_env.disable_eip3607 = true;
    cfg_env.disable_base_fee = true;
    cfg_env.disable_balance_check = true;
    cfg_env.disable_fee_charge = true;
    // EntryPoint is a contract, so its account nonce on chain is non-zero.
    // `TxEnv::default()` sets tx.nonce = 0, which revm would reject with
    // `NonceTooLow`. Simulate is "what would happen if…", not a tx that will
    // be mined — matches eth_call semantics.
    cfg_env.disable_nonce_check = true;
}

/// Implementation of the `simulate_unsignedUserOp` RPC endpoint.
#[derive(Debug, Clone)]
pub struct Simulate<Client> {
    client: Client,
    evm_config: OpEvmConfig,
    metadata_cache: MetadataCache,
    /// Shared with `eth_call` / `debug_*` so simulate inherits the same
    /// CPU-bound rayon pool and doesn't compete with general tokio work.
    task_pool: BlockingTaskPool,
    /// Shared concurrency cap (semaphore) — bounds the number of simultaneous
    /// long-lived MDBX read transactions across tracing-class RPCs.
    task_guard: BlockingTaskGuard,
}

impl<Client> Simulate<Client> {
    pub fn new(
        client: Client,
        evm_config: OpEvmConfig,
        task_pool: BlockingTaskPool,
        task_guard: BlockingTaskGuard,
    ) -> Self {
        Self {
            client,
            evm_config,
            metadata_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(METADATA_CACHE_CAPACITY).expect("non-zero capacity"),
            ))),
            task_pool,
            task_guard,
        }
    }

    /// Wire the simulate API to the same blocking pool and concurrency guard
    /// the node uses for `eth_call` / `debug_trace*`, by pulling them off the
    /// already-installed eth API.
    pub fn from_eth_api<E: SpawnBlocking>(
        client: Client,
        evm_config: OpEvmConfig,
        eth_api: &E,
    ) -> Self {
        Self::new(
            client,
            evm_config,
            eth_api.tracing_task_pool().clone(),
            eth_api.tracing_task_guard().clone(),
        )
    }
}

#[async_trait]
impl<Client> SimulateApiServer for Simulate<Client>
where
    Client: BlockReaderIdExt
        + StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + Clone
        + Send
        + Sync
        + 'static,
{
    async fn simulate_unsigned_user_op(
        &self,
        request: SimulateUnsignedUserOpRequest,
    ) -> RpcResult<SimulateUnsignedUserOpResult> {
        // Bound concurrent simulations against the shared tracing guard so a
        // burst of slow callers can't open arbitrarily many long-lived MDBX
        // readers (each pinning freelist pages until it closes).
        let permit = self
            .task_guard
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| internal_err(format!("blocking task guard closed: {e}")))?;

        // Run on the dedicated rayon pool shared with eth_call / debug_*. The
        // permit moves into the closure so it's released exactly when the
        // EVM-bound work finishes, not when the tokio future resolves.
        let this = self.clone();
        let handle = self.task_pool.spawn(move || {
            let _permit = permit;
            this.simulate_blocking(request)
        });

        // Wall-clock cap from the client's perspective. Combined with the 8M
        // gas cap and bounded concurrency, this keeps reader lifetimes finite
        // even under adversarial input.
        match tokio::time::timeout(SIMULATION_TIMEOUT, handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(_panic)) => Err(internal_err("simulation task panicked")),
            Err(_) => Err(internal_err(format!(
                "simulation exceeded {}s deadline",
                SIMULATION_TIMEOUT.as_secs()
            ))),
        }
    }
}

impl<Client> Simulate<Client>
where
    Client:
        BlockReaderIdExt + StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header>,
{
    fn simulate_blocking(
        &self,
        request: SimulateUnsignedUserOpRequest,
    ) -> RpcResult<SimulateUnsignedUserOpResult> {
        // 1. Resolve the latest sealed header. Subsequent lookups go by its
        //    concrete hash so a new block arriving mid-request can't desync
        //    the header from the state we read.
        let header = self
            .client
            .sealed_header_by_id(BlockNumberOrTag::Latest.into())
            .map_err(internal_err)?
            .ok_or_else(|| {
                jsonrpsee::types::ErrorObjectOwned::owned(
                    jsonrpsee::types::error::INVALID_PARAMS_CODE,
                    "Latest block not found",
                    None::<String>,
                )
            })?;
        let block_number = header.number();
        let block_id = BlockId::Hash(header.hash().into());

        // 2. Build the EVM environment from the header (same as eth_call)
        let mut evm_env = self
            .evm_config
            .evm_env(header.header())
            .map_err(internal_err)?;

        relax_cfg_for_simulation(&mut evm_env.cfg_env);

        // 3. Get state at the target block (same as eth_call)
        let state_provider = self
            .client
            .state_by_block_id(block_id)
            .map_err(internal_err)?;

        // 4. Build a revm State backed by the provider — no bundle tracking
        //    needed since we discard post-execution state (same as eth_call).
        let db = StateProviderDatabase::new(state_provider.as_ref());
        let mut state = State::builder().with_database(db).build();

        // 5. Create the Optimism-aware EVM with our simulation inspector.
        //    Capture chain_id first — it'd otherwise be unreachable after
        //    `evm_env` moves into the EVM, and `TxEnv::default()` hardcodes
        //    Some(1) which mismatches any non-mainnet chainspec.
        let chain_id = evm_env.cfg_env.chain_id;
        let mut evm = OpEvmFactory::default().create_evm_with_inspector(
            &mut state,
            evm_env,
            SimulationInspector::default(),
        );

        // 6. Build the transaction environment
        //    from = entry_point → msg.sender inside the smart account is the EntryPoint
        //    to   = sender      → call the smart account
        //    data = callData    → the UserOp's execution payload
        let gas_limit: u64 = match request.call_gas_limit {
            Some(g) => {
                let g: u64 = g.try_into().map_err(|_| {
                    jsonrpsee::types::ErrorObjectOwned::owned(
                        jsonrpsee::types::error::INVALID_PARAMS_CODE,
                        format!("callGasLimit exceeds maximum of {MAX_SIMULATION_GAS}"),
                        None::<String>,
                    )
                })?;
                if g > MAX_SIMULATION_GAS {
                    return Err(jsonrpsee::types::ErrorObjectOwned::owned(
                        jsonrpsee::types::error::INVALID_PARAMS_CODE,
                        format!("callGasLimit exceeds maximum of {MAX_SIMULATION_GAS}"),
                        None::<String>,
                    ));
                }
                g
            }
            None => MAX_SIMULATION_GAS,
        };

        let tx = OpTx(OpTransaction {
            base: TxEnv {
                caller: request.entry_point,
                kind: TxKind::Call(request.sender),
                data: request.call_data.clone(),
                value: U256::ZERO,
                gas_limit,
                gas_price: 0,
                chain_id: Some(chain_id),
                ..Default::default()
            },
            ..Default::default()
        });

        // 7. Execute
        let result_and_state =
            RethEvm::transact(&mut evm, tx).map_err(|e| internal_err(format!("{e}")))?;

        // 8. Pull captured logs and the gas/status for the response.
        let (status, gas_used, logs, halt_reason) = match &result_and_state.result {
            ExecutionResult::Success { gas, logs, .. } => (
                SimulationStatus::Success,
                gas.tx_gas_used(),
                logs.clone(),
                None,
            ),
            ExecutionResult::Revert { gas, logs, .. } => (
                SimulationStatus::Revert,
                gas.tx_gas_used(),
                logs.clone(),
                None,
            ),
            ExecutionResult::Halt { gas, logs, reason } => {
                // Halts (out-of-gas, stack overflow, invalid opcode, etc.) are
                // surfaced as `Revert` to the consumer: from their perspective
                // both mean "this UserOp will not land", and the distinction
                // between an EVM revert and an EVM halt isn't actionable on
                // the caller side.
                //
                // The Debug-format of revm's `HaltReason` variant is used as
                // the revert reason — e.g. `"OutOfGas(BasicOutOfGas)"`,
                // `"StackOverflow"`, `"InvalidJump"`, `"CallTooDeep"`,
                // `"PrecompileErrorWithContext(\"...\")"`.
                (
                    SimulationStatus::Revert,
                    gas.tx_gas_used(),
                    logs.clone(),
                    Some(format!("{reason:?}")),
                )
            }
        };

        // 9. Parse logs into structured asset/approval changes
        let mut asset_changes = parse_asset_changes(&logs).map_err(|m| {
            jsonrpsee::types::ErrorObjectOwned::owned(
                jsonrpsee::types::error::INVALID_PARAMS_CODE,
                m,
                None::<String>,
            )
        })?;
        let mut exposure_changes = parse_exposure_changes(&logs);

        // 10. Extract trace and native transfers from the inspector. The EVM
        //     owns it; recover a `&mut` via `components_mut()` and drain.
        let (_, inspector, _) = evm.components_mut();
        let trace = inspector.take_trace_entries();
        asset_changes.extend(inspector.take_native_asset_changes());

        // `revert_reason` is the *root cause* — the deepest reverted frame's
        // payload — so consumers see the contract-specific custom error from
        // the inner call rather than wrappers like EntryPoint's
        // `FailedOp(...)` that live further up the stack. Falls back to the
        // outer payload if the inspector somehow saw no `Revert` frame, and
        // halts surface their `HaltReason` debug name unchanged.
        let revert_reason = match status {
            SimulationStatus::Success => None,
            SimulationStatus::Revert => halt_reason.or_else(|| {
                inspector
                    .take_deepest_revert_reason()
                    .or_else(|| match &result_and_state.result {
                        ExecutionResult::Revert { output, .. } => {
                            Some(decode_revert_reason(output))
                        }
                        _ => None,
                    })
            }),
        };

        // 11. Resolve on-chain token metadata (name, symbol, decimals) — cached
        resolve_all_metadata(
            &self.client,
            &self.evm_config,
            header.header(),
            block_id,
            &self.metadata_cache,
            &mut asset_changes,
            &mut exposure_changes,
        );

        Ok(SimulateUnsignedUserOpResult {
            status,
            revert_reason,
            block_number,
            gas_used: format!("{gas_used:#x}"),
            asset_changes,
            exposure_changes,
            trace,
            warnings: vec![],
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Event signatures (keccak256 of canonical event strings)
// ═══════════════════════════════════════════════════════════════════════════════

/// `Transfer(address,address,uint256)` — ERC-20 and ERC-721
const TRANSFER_TOPIC: B256 =
    alloy_primitives::b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

/// `TransferSingle(address,address,address,uint256,uint256)` — ERC-1155
const TRANSFER_SINGLE_TOPIC: B256 =
    alloy_primitives::b256!("c3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62");

/// `TransferBatch(address,address,address,uint256[],uint256[])` — ERC-1155
const TRANSFER_BATCH_TOPIC: B256 =
    alloy_primitives::b256!("4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb");

/// `Approval(address,address,uint256)` — ERC-20 and ERC-721
const APPROVAL_TOPIC: B256 =
    alloy_primitives::b256!("8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925");

/// `ApprovalForAll(address,address,bool)` — ERC-721 / ERC-1155
const APPROVAL_FOR_ALL_TOPIC: B256 =
    alloy_primitives::b256!("17307eab39ab6107e8899845ad3d59bd9653f200f220920489ca2b5937696c31");

// ═══════════════════════════════════════════════════════════════════════════════
// Log parsing — asset changes
// ═══════════════════════════════════════════════════════════════════════════════

/// Maximum number of (id, value) pairs accepted in a single ERC-1155 TransferBatch log.
///
/// Bounded to prevent adversarially crafted logs from forcing huge allocations
/// in `decode_batch_transfer_data`.
pub const MAX_BATCH_TRANSFERS: usize = 1000;

pub fn parse_asset_changes(
    logs: &[alloy_primitives::Log],
) -> Result<Vec<AssetChange>, &'static str> {
    let mut changes = Vec::new();

    for log in logs {
        let topics = log.data.topics();
        if topics.is_empty() {
            continue;
        }

        match topics[0] {
            t if t == TRANSFER_TOPIC => {
                if topics.len() == 4 {
                    // ERC-721: Transfer(address indexed from, address indexed to, uint256 indexed tokenId)
                    let from = address_from_topic(topics[1]);
                    let to = address_from_topic(topics[2]);
                    let token_id = U256::from_be_bytes(topics[3].0);
                    changes.push(AssetChange {
                        change_type: AssetType::Erc721,
                        from,
                        to,
                        raw_amount: "1".to_string(),
                        token_id: Some(token_id.to_string()),
                        asset: placeholder_asset(log.address, AssetType::Erc721),
                    });
                } else if topics.len() == 3 && log.data.data.len() >= 32 {
                    // ERC-20: Transfer(address indexed from, address indexed to, uint256 value)
                    let from = address_from_topic(topics[1]);
                    let to = address_from_topic(topics[2]);
                    let amount = U256::from_be_slice(&log.data.data[..32]);
                    changes.push(AssetChange {
                        change_type: AssetType::Erc20,
                        from,
                        to,
                        raw_amount: amount.to_string(),
                        token_id: None,
                        asset: placeholder_asset(log.address, AssetType::Erc20),
                    });
                }
            }
            t if t == TRANSFER_SINGLE_TOPIC
                // ERC-1155: TransferSingle(operator indexed, from indexed, to indexed, id, value)
                // 4 topics: [sig, operator, from, to]; data: [id, value]
                && topics.len() == 4 && log.data.data.len() >= 64 =>
            {
                let from = address_from_topic(topics[2]);
                let to = address_from_topic(topics[3]);
                let id = U256::from_be_slice(&log.data.data[..32]);
                let value = U256::from_be_slice(&log.data.data[32..64]);
                changes.push(AssetChange {
                    change_type: AssetType::Erc1155,
                    from,
                    to,
                    raw_amount: value.to_string(),
                    token_id: Some(id.to_string()),
                    asset: placeholder_asset(log.address, AssetType::Erc1155),
                });
            }
            t if t == TRANSFER_BATCH_TOPIC
                // ERC-1155: TransferBatch(operator indexed, from indexed, to indexed, ids[], values[])
                // 4 topics: [sig, operator, from, to]; data: ABI-encoded arrays
                && topics.len() == 4 =>
            {
                let from = address_from_topic(topics[2]);
                let to = address_from_topic(topics[3]);
                if let Some(pairs) = decode_batch_transfer_data(&log.data.data)? {
                    for (id, value) in pairs {
                        changes.push(AssetChange {
                            change_type: AssetType::Erc1155,
                            from,
                            to,
                            raw_amount: value.to_string(),
                            token_id: Some(id.to_string()),
                            asset: placeholder_asset(log.address, AssetType::Erc1155),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(changes)
}

/// Decode ABI-encoded `(uint256[], uint256[])` from TransferBatch data.
///
/// Returns:
/// - `Ok(Some(pairs))` on successful decode
/// - `Ok(None)` if the log is malformed (callers should silently skip)
/// - `Err(_)` if the encoded length exceeds [`MAX_BATCH_TRANSFERS`] — propagated
///   to the RPC caller rather than skipped, so a malicious log can't both poison
///   simulation results and avoid surfacing.
fn decode_batch_transfer_data(data: &[u8]) -> Result<Option<Vec<(U256, U256)>>, &'static str> {
    // ABI: offset_ids (32) | offset_values (32) | ids_len (32) | ids... | values_len (32) | values...
    if data.len() < 128 {
        return Ok(None);
    }

    let Ok(ids_offset): Result<usize, _> = U256::from_be_slice(&data[..32]).try_into() else {
        return Ok(None);
    };
    let Ok(values_offset): Result<usize, _> = U256::from_be_slice(&data[32..64]).try_into() else {
        return Ok(None);
    };

    let Some(ids_len_slice) = data.get(ids_offset..ids_offset + 32) else {
        return Ok(None);
    };
    let Ok(ids_len): Result<usize, _> = U256::from_be_slice(ids_len_slice).try_into() else {
        return Ok(None);
    };
    let Some(values_len_slice) = data.get(values_offset..values_offset + 32) else {
        return Ok(None);
    };
    let Ok(values_len): Result<usize, _> = U256::from_be_slice(values_len_slice).try_into() else {
        return Ok(None);
    };

    if ids_len != values_len {
        return Ok(None);
    }

    if ids_len > MAX_BATCH_TRANSFERS {
        return Err("erc1155 batch transfer exceeds maximum of 1000 entries");
    }

    let mut pairs = Vec::with_capacity(ids_len);
    for i in 0..ids_len {
        let id_start = ids_offset + 32 + i * 32;
        let val_start = values_offset + 32 + i * 32;
        let Some(id_slice) = data.get(id_start..id_start + 32) else {
            return Ok(None);
        };
        let Some(val_slice) = data.get(val_start..val_start + 32) else {
            return Ok(None);
        };
        pairs.push((
            U256::from_be_slice(id_slice),
            U256::from_be_slice(val_slice),
        ));
    }

    Ok(Some(pairs))
}

// ═══════════════════════════════════════════════════════════════════════════════
// Log parsing — approval changes
// ═══════════════════════════════════════════════════════════════════════════════

pub fn parse_exposure_changes(logs: &[alloy_primitives::Log]) -> Vec<ExposureChange> {
    let mut changes = Vec::new();

    for log in logs {
        let topics = log.data.topics();
        if topics.is_empty() {
            continue;
        }

        match topics[0] {
            t if t == APPROVAL_TOPIC => {
                if topics.len() == 3 && log.data.data.len() >= 32 {
                    // ERC-20: Approval(owner, spender, value)
                    let owner = address_from_topic(topics[1]);
                    let spender = address_from_topic(topics[2]);
                    let amount = U256::from_be_slice(&log.data.data[..32]);
                    changes.push(ExposureChange {
                        owner,
                        spender,
                        raw_amount: amount.to_string(),
                        is_approved_for_all: false,
                        asset: placeholder_asset(log.address, AssetType::Erc20),
                    });
                } else if topics.len() == 4 {
                    // ERC-721: Approval(owner, approved, tokenId)
                    // Not a spending approval in the ERC-20 sense — skip or handle differently.
                    // The spec focuses on Approval events, so include it.
                    let owner = address_from_topic(topics[1]);
                    let spender = address_from_topic(topics[2]);
                    changes.push(ExposureChange {
                        owner,
                        spender,
                        raw_amount: "1".to_string(),
                        is_approved_for_all: false,
                        asset: placeholder_asset(log.address, AssetType::Erc721),
                    });
                }
            }
            t if t == APPROVAL_FOR_ALL_TOPIC && topics.len() == 3 => {
                let owner = address_from_topic(topics[1]);
                let operator = address_from_topic(topics[2]);
                // ABI: bool approved is the first 32-byte word; the value byte
                // is at the fixed offset 31. Reading log.data.last() would
                // misclassify approvals when a contract emits trailing bytes.
                let approved = log.data.data.get(31).is_some_and(|&b| b != 0);
                changes.push(ExposureChange {
                    owner,
                    spender: operator,
                    raw_amount: if approved {
                        U256::MAX.to_string()
                    } else {
                        "0".to_string()
                    },
                    is_approved_for_all: approved,
                    asset: placeholder_asset(log.address, AssetType::Erc721),
                });
            }
            _ => {}
        }
    }

    changes
}

// ═══════════════════════════════════════════════════════════════════════════════
// Revert reason decoding
// ═══════════════════════════════════════════════════════════════════════════════

/// `Error(string)` selector — `keccak256("Error(string)")[..4]`.
const ERROR_STRING_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];

/// `Panic(uint256)` selector — `keccak256("Panic(uint256)")[..4]`.
const PANIC_UINT256_SELECTOR: [u8; 4] = [0x4e, 0x48, 0x7b, 0x71];

/// Minimum payload length for a well-formed `Error(string)` revert:
/// selector(4) + string-offset(32) + string-length(32).
const MIN_ERROR_STRING_LEN: usize = 4 + 32 + 32;

/// Minimum payload length for a well-formed `Panic(uint256)` revert:
/// selector(4) + uint256(32).
const MIN_PANIC_UINT256_LEN: usize = 4 + 32;

/// Solidity panic codes — see the [Solidity docs][1] for the canonical list.
/// Sentinel returned when the panic code is too large to fit in u64.
///
/// [1]: https://docs.soliditylang.org/en/latest/control-structures.html#panic-via-assert-and-error-via-require
mod panic_code {
    pub const GENERIC: u64 = 0x00;
    pub const ASSERTION_FAILED: u64 = 0x01;
    pub const ARITHMETIC_OVER_UNDERFLOW: u64 = 0x11;
    pub const DIVISION_BY_ZERO: u64 = 0x12;
    pub const INVALID_ENUM_VALUE: u64 = 0x21;
    pub const INVALID_STORAGE_BYTE_ARRAY: u64 = 0x22;
    pub const POP_ON_EMPTY_ARRAY: u64 = 0x31;
    pub const ARRAY_INDEX_OUT_OF_BOUNDS: u64 = 0x32;
    pub const OUT_OF_MEMORY: u64 = 0x41;
    pub const UNINITIALIZED_FUNCTION_POINTER: u64 = 0x51;
    /// Returned by `try_into` when the on-chain panic code exceeds u64::MAX.
    /// Reusing 0xFF here is safe because Solidity never emits it.
    pub const UNKNOWN_SENTINEL: u64 = 0xFF;
}

/// Decode revert data into a human-readable string.
/// Handles `Error(string)` and `Panic(uint256)`; falls back to hex for
/// custom errors and other unknown payloads.
pub fn decode_revert_reason(output: &Bytes) -> String {
    if output.len() < 4 {
        return format!("0x{}", hex::encode(output.as_ref()));
    }

    let selector = &output[..4];

    if selector == ERROR_STRING_SELECTOR && output.len() >= MIN_ERROR_STRING_LEN {
        let offset: usize = U256::from_be_slice(&output[4..36]).try_into().unwrap_or(0);
        let abs_offset = 4 + offset;
        if abs_offset + 32 <= output.len() {
            let len: usize = U256::from_be_slice(&output[abs_offset..abs_offset + 32])
                .try_into()
                .unwrap_or(0);
            let str_start = abs_offset + 32;
            if str_start + len <= output.len()
                && let Ok(s) = std::str::from_utf8(&output[str_start..str_start + len])
            {
                return s.to_string();
            }
        }
    }

    if selector == PANIC_UINT256_SELECTOR && output.len() >= MIN_PANIC_UINT256_LEN {
        let code = U256::from_be_slice(&output[4..36]);
        let reason = match code.try_into().unwrap_or(panic_code::UNKNOWN_SENTINEL) {
            panic_code::GENERIC => "generic compiler panic",
            panic_code::ASSERTION_FAILED => "assertion failed",
            panic_code::ARITHMETIC_OVER_UNDERFLOW => "arithmetic overflow/underflow",
            panic_code::DIVISION_BY_ZERO => "division by zero",
            panic_code::INVALID_ENUM_VALUE => "invalid enum value",
            panic_code::INVALID_STORAGE_BYTE_ARRAY => "invalid storage byte array",
            panic_code::POP_ON_EMPTY_ARRAY => "pop on empty array",
            panic_code::ARRAY_INDEX_OUT_OF_BOUNDS => "array index out of bounds",
            panic_code::OUT_OF_MEMORY => "out of memory",
            panic_code::UNINITIALIZED_FUNCTION_POINTER => "uninitialized function pointer",
            _ => "unknown panic",
        };
        return format!("Panic({reason})");
    }

    format!("0x{}", hex::encode(output.as_ref()))
}

// ═══════════════════════════════════════════════════════════════════════════════
// Selector-to-name mapping
// ═══════════════════════════════════════════════════════════════════════════════

/// Well-known ERC-20 metadata view selectors. Defined here so they're shared
/// between trace decoding (`selector_to_name`) and the metadata resolver, which
/// needs the raw bytes to make the calls.
pub const NAME_SELECTOR: [u8; 4] = [0x06, 0xfd, 0xde, 0x03]; // name()
pub const SYMBOL_SELECTOR: [u8; 4] = [0x95, 0xd8, 0x9b, 0x41]; // symbol()
pub const DECIMALS_SELECTOR: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67]; // decimals()

/// Best-effort decode of a 4-byte function selector to a human-readable name.
pub fn selector_to_name(selector: [u8; 4]) -> Option<&'static str> {
    match selector {
        // ERC-20 metadata views
        NAME_SELECTOR => Some("name"),
        SYMBOL_SELECTOR => Some("symbol"),
        DECIMALS_SELECTOR => Some("decimals"),
        // ERC-20
        [0xa9, 0x05, 0x9c, 0xbb] => Some("transfer"),
        [0x23, 0xb8, 0x72, 0xdd] => Some("transferFrom"),
        [0x09, 0x5e, 0xa7, 0xb3] => Some("approve"),
        // ERC-721
        [0x42, 0x84, 0x2e, 0x0e] => Some("safeTransferFrom"),
        [0xb8, 0x8d, 0x4f, 0xde] => Some("safeTransferFrom"),
        [0xa2, 0x2c, 0xb4, 0x65] => Some("setApprovalForAll"),
        // ERC-1155
        [0xf2, 0x42, 0x43, 0x2a] => Some("safeTransferFrom"),
        [0x2e, 0xb2, 0xc2, 0xd6] => Some("safeBatchTransferFrom"),
        // Multicall / execute
        [0xb6, 0x1d, 0x27, 0xf6] => Some("execute"),
        [0x51, 0x94, 0x54, 0x47] => Some("executeBatch"),
        [0x8d, 0x80, 0xff, 0x0a] => Some("multiSend"),
        // Safe admin methods (flagged by backend as forbidden)
        [0x0d, 0x58, 0x2f, 0x13] => Some("addOwnerWithThreshold"),
        [0xf8, 0xdc, 0x5d, 0xd9] => Some("removeOwner"),
        [0xe3, 0x18, 0xb5, 0x2b] => Some("swapOwner"),
        [0x69, 0x4e, 0x80, 0xc3] => Some("changeThreshold"),
        [0x61, 0x0b, 0x59, 0x25] => Some("enableModule"),
        [0xe0, 0x09, 0xcf, 0xde] => Some("disableModule"),
        [0xe1, 0x9a, 0x9d, 0xd9] => Some("setGuard"),
        [0xf0, 0x8a, 0x03, 0x23] => Some("setFallbackHandler"),
        [0xe3, 0x19, 0xf3, 0x23] => Some("setModuleGuard"),
        [0xb6, 0x31, 0x28, 0x05] => Some("setup"),
        // Permit2
        [0x87, 0x51, 0x7c, 0x45] => Some("permit"),
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Token metadata resolution
// ═══════════════════════════════════════════════════════════════════════════════

/// Resolve metadata (name, symbol, decimals) for every token in `tokens`
/// against a single shared EVM instance and one state provider.
///
/// Each token previously paid for 3 separate state-provider open + EVM build
/// cycles; this batches all 3·N calls onto the same warm state cache.
fn run_metadata_calls<Client>(
    client: &Client,
    evm_config: &OpEvmConfig,
    header: &alloy_consensus::Header,
    block_id: alloy_rpc_types::BlockId,
    tokens: &[(Address, AssetType)],
) -> Vec<(Address, AssetInfo)>
where
    Client: StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header>,
{
    let Ok(mut evm_env) = evm_config.evm_env(header) else {
        return Vec::new();
    };
    relax_cfg_for_simulation(&mut evm_env.cfg_env);

    let Ok(state_provider) = client.state_by_block_id(block_id) else {
        return Vec::new();
    };
    let db = StateProviderDatabase::new(state_provider.as_ref());
    let mut state = State::builder().with_database(db).build();
    // Capture chain_id before `evm_env` moves into the EVM — `TxEnv::default()`
    // hardcodes Some(1), which would mismatch any non-mainnet chainspec.
    let chain_id = evm_env.cfg_env.chain_id;
    let mut evm = OpEvmFactory::default().create_evm(&mut state, evm_env);

    let mut call_view = |to: Address, selector: &[u8; 4]| -> Option<Bytes> {
        let tx = OpTx(OpTransaction {
            base: TxEnv {
                caller: Address::ZERO,
                kind: TxKind::Call(to),
                data: Bytes::copy_from_slice(selector),
                value: U256::ZERO,
                gas_limit: 100_000,
                gas_price: 0,
                chain_id: Some(chain_id),
                ..Default::default()
            },
            ..Default::default()
        });
        match RethEvm::transact(&mut evm, tx).ok()?.result {
            ExecutionResult::Success {
                output: Output::Call(data),
                ..
            } => Some(data),
            _ => None,
        }
    };

    let mut out = Vec::with_capacity(tokens.len());
    for (addr, asset_type) in tokens {
        let name = call_view(*addr, &NAME_SELECTOR)
            .and_then(|b| decode_abi_string(&b))
            .unwrap_or_default();
        let symbol = call_view(*addr, &SYMBOL_SELECTOR)
            .and_then(|b| decode_abi_string(&b))
            .unwrap_or_default();
        let decimals = call_view(*addr, &DECIMALS_SELECTOR)
            .and_then(|b| b.get(31).copied())
            .unwrap_or(0);
        out.push((
            *addr,
            AssetInfo {
                address: *addr,
                symbol,
                name,
                decimals,
                asset_type: *asset_type,
            },
        ));
    }
    out
}

/// Decode an ABI-encoded string from raw bytes.
fn decode_abi_string(data: &[u8]) -> Option<String> {
    if data.len() < 64 {
        return None;
    }
    let offset: usize = U256::from_be_slice(&data[..32]).try_into().ok()?;
    if offset + 32 > data.len() {
        return None;
    }
    let len: usize = U256::from_be_slice(&data[offset..offset + 32])
        .try_into()
        .ok()?;
    let str_start = offset + 32;
    if str_start + len > data.len() {
        return None;
    }
    String::from_utf8(data[str_start..str_start + len].to_vec()).ok()
}

/// Resolve metadata for all unique token addresses, using a persistent cache.
fn resolve_all_metadata<Client>(
    client: &Client,
    evm_config: &OpEvmConfig,
    header: &alloy_consensus::Header,
    block_id: alloy_rpc_types::BlockId,
    cache: &MetadataCache,
    asset_changes: &mut [AssetChange],
    exposure_changes: &mut [ExposureChange],
) where
    Client: StateProviderFactory + HeaderProvider<Header = alloy_consensus::Header>,
{
    // Collect unique addresses that need resolution (not in cache and not yet resolved).
    let mut to_resolve: Vec<(Address, AssetType)> = Vec::new();

    {
        let cache_guard = cache.lock().unwrap();
        let mut seen_this_call = std::collections::HashSet::new();

        // Native ETH has its metadata pre-populated (symbol="ETH"), so the
        // `symbol.is_empty()` gate keeps `Address::ZERO` out of the resolver.
        for change in asset_changes.iter() {
            let addr = change.asset.address;
            if change.asset.symbol.is_empty()
                && !cache_guard.contains(&addr)
                && seen_this_call.insert(addr)
            {
                to_resolve.push((addr, change.asset.asset_type));
            }
        }
        for change in exposure_changes.iter() {
            let addr = change.asset.address;
            if change.asset.symbol.is_empty()
                && !cache_guard.contains(&addr)
                && seen_this_call.insert(addr)
            {
                to_resolve.push((addr, change.asset.asset_type));
            }
        }
    }

    // Resolve missing metadata via a single shared EVM instance — the lock is
    // intentionally not held during the EVM/disk work so concurrent requests
    // can read other entries in parallel.
    if !to_resolve.is_empty() {
        let resolved = run_metadata_calls(client, evm_config, header, block_id, &to_resolve);
        let mut cache_guard = cache.lock().unwrap();
        for (addr, info) in resolved {
            cache_guard.put(addr, info);
        }
    }

    // Apply cached metadata to all changes (`get` bumps LRU recency). Native
    // ETH (`Address::ZERO`) is never inserted into the cache, so this is a
    // no-op for it.
    let mut cache_guard = cache.lock().unwrap();
    for change in asset_changes.iter_mut() {
        if let Some(info) = cache_guard.get(&change.asset.address) {
            change.asset = info.clone();
        }
    }
    for change in exposure_changes.iter_mut() {
        if let Some(info) = cache_guard.get(&change.asset.address) {
            change.asset = info.clone();
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn address_from_topic(topic: B256) -> Address {
    Address::from_slice(&topic[12..])
}

/// Placeholder asset info — metadata will be resolved in a later step.
fn placeholder_asset(contract: Address, asset_type: AssetType) -> AssetInfo {
    AssetInfo {
        address: contract,
        symbol: String::new(),
        name: String::new(),
        decimals: 0,
        asset_type,
    }
}

fn internal_err(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObjectOwned {
    jsonrpsee::types::ErrorObjectOwned::owned(
        jsonrpsee::types::error::INTERNAL_ERROR_CODE,
        msg.to_string(),
        None::<String>,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Stand-in "malicious payload": anything that pegs a worker for longer
    /// than `SIMULATION_TIMEOUT`. Crafting bytecode that reliably blows
    /// past 5s with the 8M gas cap is hardware-dependent and flaky in unit
    /// tests, so we model the worst case directly with a blocking sleep.
    /// The primitive under test is identical to the one in
    /// `simulate_unsigned_user_op`: acquire a `BlockingTaskGuard` permit,
    /// hand the work to `BlockingTaskPool`, wrap with `tokio::time::timeout`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_aborts_long_running_simulation() {
        let pool = BlockingTaskPool::build().expect("build blocking pool");
        let guard = BlockingTaskGuard::new(4);
        let timeout = Duration::from_millis(100);

        let permit = guard.clone().acquire_owned().await.expect("acquire permit");
        let handle = pool.spawn(move || {
            let _permit = permit;
            std::thread::sleep(Duration::from_secs(10));
            42_u64
        });

        let started = Instant::now();
        let result = tokio::time::timeout(timeout, handle).await;
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "expected wall-clock timeout, got {result:?}"
        );
        // The future must resolve when the deadline expires, not wait for
        // the rayon task to finish. The task itself keeps running and
        // releases its permit on its own — that's the design.
        assert!(
            elapsed < Duration::from_secs(1),
            "timeout should resolve at the deadline, elapsed={elapsed:?}"
        );
    }

    /// The blocking task keeps holding its concurrency permit until it
    /// finishes naturally — even after the tokio future has timed out.
    /// This is what bounds reader lifetimes under sustained adversarial
    /// load: a slow caller continues to occupy a slot, so a flood of
    /// "malicious" simulations can't open arbitrarily many readers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn permit_is_held_until_blocking_task_finishes() {
        let pool = BlockingTaskPool::build().expect("build blocking pool");
        let guard = BlockingTaskGuard::new(1);

        let permit = guard
            .clone()
            .acquire_owned()
            .await
            .expect("acquire first permit");
        let handle = pool.spawn(move || {
            let _permit = permit;
            std::thread::sleep(Duration::from_millis(300));
        });

        // Even after timing out, the rayon task is still running and
        // holding the only permit, so a fresh acquire must wait.
        let _ = tokio::time::timeout(Duration::from_millis(50), handle).await;

        let acquire_started = Instant::now();
        let _second_permit = guard.clone().acquire_owned().await.expect("second permit");
        let waited = acquire_started.elapsed();

        assert!(
            waited >= Duration::from_millis(150),
            "second acquire should block until the rayon task releases its \
             permit; waited={waited:?}"
        );
    }
}
