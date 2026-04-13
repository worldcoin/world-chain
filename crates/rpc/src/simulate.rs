use alloy_op_evm::OpEvmFactory;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rpc_types::BlockId;
use jsonrpsee::core::{RpcResult, async_trait};
use jsonrpsee::proc_macros::rpc;
use reth_evm::{ConfigureEvm, Evm as RethEvm, EvmFactory};
use reth_evm::op_revm::OpTransaction;
use reth_optimism_evm::OpEvmConfig;
use reth_provider::{BlockReaderIdExt, HeaderProvider, StateProviderFactory};
use reth_revm::{State, database::StateProviderDatabase};
use revm::context::TxEnv;
use revm::context::result::{ExecutionResult, Output};
use revm_database::BundleState;
use revm_primitives::TxKind;
use serde::{Deserialize, Serialize};

/// Request type for simulating a UserOp's execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulateUserOpRequest {
    /// Smart account address (the sender of the UserOp).
    pub sender: Address,
    /// The callData field from the UserOp (typically encodes `execute(to, value, data)`).
    pub call_data: Bytes,
    /// Optional gas limit for the simulation (defaults to 30M).
    #[serde(default)]
    pub gas_limit: Option<u64>,
}

/// A log entry emitted during simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

/// Result of simulating a UserOp's execution phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulateUserOpResult {
    /// Whether the execution succeeded.
    pub success: bool,
    /// Gas consumed by the execution.
    pub gas_used: u64,
    /// Event logs emitted during execution.
    pub logs: Vec<SimulationLog>,
    /// Return data (or revert data on failure).
    pub return_data: Bytes,
}

/// RPC trait for simulating unsigned UserOp execution.
#[cfg_attr(not(test), rpc(server, namespace = "worldchain"))]
#[cfg_attr(test, rpc(server, client, namespace = "worldchain"))]
#[async_trait]
pub trait WorldChainSimulateApi {
    /// Simulates the execution phase of a UserOp by calling the sender's smart account
    /// with the provided callData, as if `msg.sender` were the EntryPoint.
    ///
    /// This bypasses signature validation entirely — only the execution phase is run.
    /// Returns the execution logs, gas used, and return/revert data.
    #[method(name = "simulateUserOp")]
    async fn simulate_user_op(
        &self,
        user_op: SimulateUserOpRequest,
        entry_point: Address,
        block_id: Option<BlockId>,
    ) -> RpcResult<SimulateUserOpResult>;
}

/// Implementation of the `worldchain_simulateUserOp` RPC endpoint.
#[derive(Debug, Clone)]
pub struct WorldChainSimulate<Client> {
    client: Client,
    evm_config: OpEvmConfig,
}

impl<Client> WorldChainSimulate<Client> {
    pub fn new(client: Client, evm_config: OpEvmConfig) -> Self {
        Self { client, evm_config }
    }
}

#[async_trait]
impl<Client> WorldChainSimulateApiServer for WorldChainSimulate<Client>
where
    Client: BlockReaderIdExt
        + StateProviderFactory
        + HeaderProvider<Header = alloy_consensus::Header>
        + 'static,
{
    async fn simulate_user_op(
        &self,
        user_op: SimulateUserOpRequest,
        entry_point: Address,
        block_id: Option<BlockId>,
    ) -> RpcResult<SimulateUserOpResult> {
        let block_id = block_id.unwrap_or(BlockId::latest());

        // 1. Resolve the sealed header for the target block
        let header = self
            .client
            .sealed_header_by_id(block_id)
            .map_err(internal_err)?
            .ok_or_else(|| {
                jsonrpsee::types::ErrorObjectOwned::owned(
                    jsonrpsee::types::error::INVALID_PARAMS_CODE,
                    "Block not found",
                    None::<String>,
                )
            })?;

        // 2. Build the EVM environment (block env + cfg) from the header
        let evm_env = self.evm_config.evm_env(header.header()).map_err(internal_err)?;

        // 3. Get state at the target block
        let state_provider = self
            .client
            .state_by_block_id(block_id)
            .map_err(internal_err)?;

        // 4. Build a revm State backed by the provider
        let db = StateProviderDatabase::new(state_provider.as_ref());
        let mut state = State::builder()
            .with_database(db)
            .with_bundle_prestate(BundleState::default())
            .with_bundle_update()
            .build();

        // 5. Create the Optimism-aware EVM (includes OP precompiles, L1 fee handling, etc.)
        let mut evm = OpEvmFactory::default().create_evm(&mut state, evm_env);

        // 6. Build the transaction environment.
        //    from = entry_point  → so msg.sender inside the smart account is the EntryPoint
        //    to   = sender       → we're calling the smart account
        //    data = callData     → the UserOp's execution payload
        let gas_limit = user_op.gas_limit.unwrap_or(30_000_000);

        let tx = OpTransaction {
            base: TxEnv {
                caller: entry_point,
                kind: TxKind::Call(user_op.sender),
                data: user_op.call_data,
                value: U256::ZERO,
                gas_limit,
                gas_price: 0,
                ..Default::default()
            },
            ..Default::default()
        };

        // 7. Execute
        let result_and_state =
            RethEvm::transact(&mut evm, tx).map_err(|e| internal_err(format!("{e}")))?;

        // 8. Convert the execution result into our RPC response
        let sim_result = match result_and_state.result {
            ExecutionResult::Success {
                gas_used,
                logs,
                output,
                ..
            } => {
                let return_data = match output {
                    Output::Call(data) => data,
                    Output::Create(data, _) => data,
                };
                SimulateUserOpResult {
                    success: true,
                    gas_used,
                    logs: logs
                        .into_iter()
                        .map(|log| SimulationLog {
                            address: log.address,
                            topics: log.data.topics().to_vec(),
                            data: log.data.data.clone(),
                        })
                        .collect(),
                    return_data,
                }
            }
            ExecutionResult::Revert { gas_used, output } => SimulateUserOpResult {
                success: false,
                gas_used,
                logs: vec![],
                return_data: output,
            },
            ExecutionResult::Halt { gas_used, .. } => SimulateUserOpResult {
                success: false,
                gas_used,
                logs: vec![],
                return_data: Bytes::new(),
            },
        };

        Ok(sim_result)
    }
}

fn internal_err(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObjectOwned {
    jsonrpsee::types::ErrorObjectOwned::owned(
        jsonrpsee::types::error::INTERNAL_ERROR_CODE,
        msg.to_string(),
        None::<String>,
    )
}
