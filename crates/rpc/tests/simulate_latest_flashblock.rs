use alloy_primitives::{Address, Bytes};
use jsonrpsee::types::error::INVALID_PARAMS_CODE;
use reth_chain_state::ExecutedBlock;
use reth_optimism_primitives::OpPrimitives;
use reth_provider::ChainSpecProvider;
use reth_tasks::pool::{BlockingTaskGuard, BlockingTaskPool};
use serde_json::Value;
use tokio::sync::watch;
use world_chain_evm::WorldChainEvmConfig;
use world_chain_rpc::{LatestFlashblockReceiver, Simulate, SimulateApiServer};
use world_chain_test_utils::node::WorldChainNoopProvider;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulate_endpoint_rejects_latest_flashblock_when_flashblocks_are_disabled() {
    let response = call_simulate_latest_flashblock(None, None).await;

    assert_rpc_error(
        response,
        "useLatestFlashblock requires flashblocks to be enabled on the node",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulate_endpoint_defaults_to_latest_full_block_without_flashblock_flag() {
    let provider = WorldChainNoopProvider::default();
    let evm_config = WorldChainEvmConfig::optimism(provider.chain_spec());
    let module = Simulate::new(
        provider,
        evm_config,
        BlockingTaskPool::build().expect("build blocking pool"),
        BlockingTaskGuard::new(1),
    )
    .into_rpc();

    let (response, _) = module
        .raw_json_request(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "simulate_unsignedUserOp",
                "params": [{
                    "sender": Address::ZERO,
                    "callData": Bytes::new(),
                    "entryPoint": Address::ZERO,
                }],
                "id": 1,
            })
            .to_string(),
            1,
        )
        .await
        .expect("valid JSON-RPC request");

    assert_rpc_error(
        serde_json::from_str(response.get()).expect("valid JSON-RPC response"),
        "Block not found: latest",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulate_endpoint_rejects_latest_flashblock_when_no_flashblock_exists() {
    let (_, pending_block) = watch::channel::<Option<ExecutedBlock<OpPrimitives>>>(None);
    let response = call_simulate_latest_flashblock(Some(pending_block), None).await;

    assert_rpc_error(response, "Latest flashblock not found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulate_endpoint_rejects_latest_flashblock_with_explicit_block() {
    let response = call_simulate_latest_flashblock(None, Some(serde_json::json!("latest"))).await;

    assert_rpc_error(
        response,
        "block cannot be specified when useLatestFlashblock is true",
    );
}

async fn call_simulate_latest_flashblock(
    pending_block: Option<LatestFlashblockReceiver>,
    block: Option<Value>,
) -> Value {
    let provider = WorldChainNoopProvider::default();
    let evm_config = WorldChainEvmConfig::optimism(provider.chain_spec());
    let module = Simulate::new(
        provider,
        evm_config,
        BlockingTaskPool::build().expect("build blocking pool"),
        BlockingTaskGuard::new(1),
    )
    .with_latest_flashblock(pending_block)
    .into_rpc();
    let mut request = serde_json::json!({
        "sender": Address::ZERO,
        "callData": Bytes::new(),
        "entryPoint": Address::ZERO,
        "useLatestFlashblock": true,
    });

    if let Some(block) = block {
        request["block"] = block;
    }

    let (response, _) = module
        .raw_json_request(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "simulate_unsignedUserOp",
                "params": [request],
                "id": 1,
            })
            .to_string(),
            1,
        )
        .await
        .expect("valid JSON-RPC request");

    serde_json::from_str(response.get()).expect("valid JSON-RPC response")
}

fn assert_rpc_error(response: Value, message: &str) {
    let error = response.get("error").expect("response should be an error");
    assert_eq!(
        error.get("code").and_then(Value::as_i64),
        Some(INVALID_PARAMS_CODE.into())
    );
    assert_eq!(error.get("message").and_then(Value::as_str), Some(message));
}
