use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, B256, Bytes, Sealed, TxKind, U256, keccak256};
use alloy_rpc_types_engine::{ForkchoiceState, PayloadStatusEnum};
use eyre::eyre::{WrapErr, eyre};
use jsonrpsee::{
    core::client::{ClientT, Error as RpcError, SubscriptionClientT},
    rpc_params,
};
use op_alloy_consensus::{OpTxEnvelope, TxDeposit};
use reth_optimism_node::OpEngineTypes;
use reth_rpc_api::Web3ApiClient;
use reth_rpc_server_types::{RethRpcModule, RpcModuleSelection};
use serde_json::{Value, json};
use std::time::Duration;
use world_chain_node::{
    context::WorldChainDefaultContext,
    version::{
        WORLD_CHAIN_CLIENT_NAME, WORLD_CHAIN_CLIENT_VERSION, WORLD_CHAIN_CLIENT_VERSION_SHA,
    },
};
use world_chain_rpc::engine::FlashblocksEngineApiExtClient;
use world_chain_test_utils::{
    e2e_harness::setup::{
        TX_SET_L1_BLOCK, WorldChainTestBuilder, build_payload_attributes,
        create_authorization_generator, current_timestamp, encode_eip1559_params,
    },
    utils::{eip1559, raw_tx},
};

const PRECONFIRMED_LOGS_CAPABILITY: &str = "worldchain_preconfirmed_logs_v1";

fn assert_rpc_error(error: RpcError, expected_code: i32) {
    match error {
        RpcError::Call(error) => assert_eq!(error.code(), expected_code, "{error}"),
        error => panic!("expected JSON-RPC error {expected_code}, got {error}"),
    }
}

fn registered_event_deployment() -> (Bytes, Address, [B256; 4]) {
    let from = Address::with_last_byte(0x42);
    let topics = [
        keccak256("Registered(uint64,uint64,uint256)"),
        U256::from(7).into(),
        U256::from(1_800_000_000u64).into(),
        U256::from(42).into(),
    ];
    let mut runtime = Vec::new();
    for _ in 0..2 {
        for topic in topics.iter().rev() {
            runtime.push(0x7f); // PUSH32
            runtime.extend_from_slice(topic.as_slice());
        }
        runtime.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0xa4]);
    }
    // Identical logs test transaction-local identity; nonempty calldata reverts both emissions.
    let success = u16::try_from(runtime.len() + 11)
        .expect("test jump offset fits PUSH2")
        .to_be_bytes();
    runtime.extend_from_slice(&[
        0x36, 0x15, 0x61, success[0], success[1], 0x57, 0x60, 0, 0x60, 0, 0xfd, 0x5b, 0,
    ]);
    let length = u16::try_from(runtime.len())
        .expect("test runtime fits PUSH2")
        .to_be_bytes();
    let mut initcode = vec![
        0x61, length[0], length[1], 0x60, 14, 0x60, 0, 0x39, 0x61, length[0], length[1], 0x60, 0,
        0xf3,
    ];
    initcode.extend_from_slice(&runtime);
    let deposit = TxDeposit {
        source_hash: B256::with_last_byte(1),
        from,
        to: TxKind::Create,
        mint: 0,
        value: U256::ZERO,
        gas_limit: 200_000,
        is_system_transaction: false,
        input: initcode.into(),
    };
    let envelope = OpTxEnvelope::Deposit(Sealed::new_unchecked(deposit, B256::ZERO));
    (envelope.encoded_2718().into(), from.create(0), topics)
}

fn response_logs(result: &Value) -> Vec<&Value> {
    result["events"]
        .as_array()
        .expect("response events must be an array")
        .iter()
        .flat_map(|event| {
            event["logs"]
                .as_array()
                .expect("event logs must be an array")
        })
        .collect()
}

#[tokio::test]
async fn web3_client_version_reports_world_chain_build_identity() -> eyre::Result<()> {
    world_chain_node::init_version_metadata();

    let (_, nodes, _exec, _env, _spammer) = WorldChainTestBuilder::builder()
        .flashblocks(false)
        .build()
        .setup::<WorldChainDefaultContext>()
        .await?;

    let client = nodes[0]
        .node
        .rpc_client()
        .ok_or_else(|| eyre!("expected HTTP RPC client"))?;

    let version = Web3ApiClient::client_version(&client).await?;

    assert_eq!(version, WORLD_CHAIN_CLIENT_VERSION);
    assert!(version.starts_with(&format!("{WORLD_CHAIN_CLIENT_NAME}/")));
    assert!(version.contains(WORLD_CHAIN_CLIENT_VERSION_SHA));

    Ok(())
}

#[tokio::test]
async fn preconfirmed_logs_are_unavailable_without_opt_in() -> eyre::Result<()> {
    let (_, nodes, _exec, _env, _spammer) = WorldChainTestBuilder::builder()
        .flashblocks(true)
        .ws_rpc(true)
        .build()
        .setup::<WorldChainDefaultContext>()
        .await?;
    let http = nodes[0]
        .node
        .rpc_client()
        .ok_or_else(|| eyre!("expected HTTP RPC client"))?;
    let ws = nodes[0]
        .node
        .inner
        .rpc_server_handle()
        .ws_client()
        .await
        .ok_or_else(|| eyre!("expected WebSocket RPC client"))?;

    let capabilities: Vec<String> = http
        .request("op_supportedCapabilities", rpc_params![])
        .await?;
    assert!(
        !capabilities
            .iter()
            .any(|capability| capability == PRECONFIRMED_LOGS_CAPABILITY)
    );
    assert_rpc_error(
        http.request::<Value, _>("worldchain_getPreconfirmedLogs", rpc_params![json!({})])
            .await
            .expect_err("disabled feed must not return an empty successful snapshot"),
        -32601,
    );
    assert_rpc_error(
        ws.subscribe::<Value, _>(
            "worldchain_subscribe",
            rpc_params!["preconfirmedLogs", json!({})],
            "worldchain_unsubscribe",
        )
        .await
        .expect_err("disabled feed must not register subscriptions"),
        -32601,
    );
    Ok(())
}

#[tokio::test]
async fn preconfirmed_logs_report_initializing_and_reject_unknown_subscriptions() -> eyre::Result<()>
{
    let (_, nodes, _exec, _env, _spammer) = WorldChainTestBuilder::builder()
        .flashblocks(true)
        .preconfirmed_logs(true)
        .ws_rpc(true)
        .build()
        .setup::<WorldChainDefaultContext>()
        .await?;
    let http = nodes[0]
        .node
        .rpc_client()
        .ok_or_else(|| eyre!("expected HTTP RPC client"))?;
    let ws = nodes[0]
        .node
        .inner
        .rpc_server_handle()
        .ws_client()
        .await
        .ok_or_else(|| eyre!("expected WebSocket RPC client"))?;

    for capabilities in [
        http.request::<Vec<String>, _>("op_supportedCapabilities", rpc_params![])
            .await?,
        ws.request::<Vec<String>, _>("op_supportedCapabilities", rpc_params![])
            .await?,
    ] {
        assert!(
            capabilities
                .iter()
                .any(|capability| capability == PRECONFIRMED_LOGS_CAPABILITY)
        );
    }
    for error in [
        http.request::<Value, _>("worldchain_getPreconfirmedLogs", rpc_params![json!({})])
            .await
            .expect_err("feed must fail explicitly before the first validated view"),
        ws.request::<Value, _>("worldchain_getPreconfirmedLogs", rpc_params![json!({})])
            .await
            .expect_err("WebSocket queries must use the same feed state"),
    ] {
        assert_rpc_error(error, -32050);
    }
    assert_rpc_error(
        http.request::<Value, _>(
            "worldchain_subscribe",
            rpc_params!["preconfirmedLogs", json!({})],
        )
        .await
        .expect_err("subscriptions must only be exposed over WebSocket"),
        -32601,
    );
    assert_rpc_error(
        ws.subscribe::<Value, _>(
            "worldchain_subscribe",
            rpc_params!["logs", json!({})],
            "worldchain_unsubscribe",
        )
        .await
        .expect_err("only the documented subscription kind is supported"),
        -32602,
    );
    Ok(())
}

#[tokio::test]
async fn preconfirmed_capability_matches_each_transport_namespace() -> eyre::Result<()> {
    let (_, nodes, _exec, _env, _spammer) = WorldChainTestBuilder::builder()
        .flashblocks(true)
        .preconfirmed_logs(true)
        .http_api(RpcModuleSelection::Selection([RethRpcModule::Net].into()))
        .ws_rpc(true)
        .build()
        .setup::<WorldChainDefaultContext>()
        .await?;
    let http = nodes[0]
        .node
        .rpc_client()
        .ok_or_else(|| eyre!("expected HTTP RPC client"))?;
    let ws = nodes[0]
        .node
        .inner
        .rpc_server_handle()
        .ws_client()
        .await
        .ok_or_else(|| eyre!("expected WebSocket RPC client"))?;
    let http_capabilities: Vec<String> = http
        .request("op_supportedCapabilities", rpc_params![])
        .await?;
    assert!(
        !http_capabilities
            .iter()
            .any(|capability| capability == PRECONFIRMED_LOGS_CAPABILITY)
    );
    assert_rpc_error(
        http.request::<Value, _>("worldchain_getPreconfirmedLogs", rpc_params![json!({})])
            .await
            .expect_err("net-only HTTP must not expose the custom Eth API"),
        -32601,
    );
    let ws_capabilities: Vec<String> = ws
        .request("op_supportedCapabilities", rpc_params![])
        .await?;
    assert!(
        ws_capabilities
            .iter()
            .any(|capability| capability == PRECONFIRMED_LOGS_CAPABILITY)
    );
    assert_rpc_error(
        ws.request::<Value, _>("worldchain_getPreconfirmedLogs", rpc_params![json!({})])
            .await
            .expect_err("Eth WebSocket must expose the configured, initializing feed"),
        -32050,
    );
    Ok(())
}

#[tokio::test]
async fn preconfirmed_shared_listener_keeps_subscriptions_websocket_only() -> eyre::Result<()> {
    let (_, nodes, _exec, _env, _spammer) = WorldChainTestBuilder::builder()
        .flashblocks(true)
        .preconfirmed_logs(true)
        .ws_rpc(true)
        .shared_rpc_port(true)
        .build()
        .setup::<WorldChainDefaultContext>()
        .await?;
    let handle = nodes[0].node.inner.rpc_server_handle();
    assert_eq!(handle.http_local_addr(), handle.ws_local_addr());
    let http = nodes[0]
        .node
        .rpc_client()
        .ok_or_else(|| eyre!("expected HTTP RPC client"))?;
    let ws = handle
        .ws_client()
        .await
        .ok_or_else(|| eyre!("expected WebSocket RPC client"))?;
    let capabilities: Vec<String> = ws
        .request("op_supportedCapabilities", rpc_params![])
        .await?;
    assert!(
        capabilities
            .iter()
            .any(|capability| capability == PRECONFIRMED_LOGS_CAPABILITY)
    );
    assert_rpc_error(
        ws.request::<Value, _>("worldchain_getPreconfirmedLogs", rpc_params![json!({})])
            .await
            .expect_err("shared WebSocket query must reach the initializing feed"),
        -32050,
    );
    assert_rpc_error(
        ws.subscribe::<Value, _>(
            "worldchain_subscribe",
            rpc_params!["logs", json!({})],
            "worldchain_unsubscribe",
        )
        .await
        .expect_err("shared WebSocket subscription must validate its kind"),
        -32602,
    );
    assert_rpc_error(
        ws.subscribe::<Value, _>(
            "worldchain_subscribe",
            rpc_params!["preconfirmedLogs", json!({})],
            "worldchain_unsubscribe",
        )
        .await
        .expect_err("shared WebSocket subscription must reach the initializing feed"),
        -32050,
    );
    for error in [
        http.request::<Value, _>(
            "worldchain_subscribe",
            rpc_params!["preconfirmedLogs", json!({})],
        )
        .await
        .expect_err("HTTP must reject subscriptions before dispatch"),
        http.request::<Value, _>(
            "worldchain_unsubscribe",
            rpc_params!["unknown-subscription"],
        )
        .await
        .expect_err("HTTP must reject unsubscribe before dispatch"),
    ] {
        assert_rpc_error(error, -32603);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preconfirmed_registered_logs_stream_and_resume() -> eyre::Result<()> {
    let (_, nodes, _exec, env, _spammer) = WorldChainTestBuilder::builder()
        .nodes(2)
        .flashblocks(true)
        .preconfirmed_logs(true)
        .ws_rpc(true)
        .build()
        .setup::<WorldChainDefaultContext>()
        .await?;
    let follower = &nodes[1].node;
    let http = follower
        .rpc_client()
        .ok_or_else(|| eyre!("expected HTTP RPC client"))?;
    let ws = follower
        .inner
        .rpc_server_handle()
        .ws_client()
        .await
        .ok_or_else(|| eyre!("expected WebSocket RPC client"))?;
    let (deployment_tx, emitter, topics) = registered_event_deployment();
    let filter = json!({"address": emitter, "topics": [topics[0]]});

    let parent_hash = nodes[0].node.block_hash(0);
    let chain_spec = nodes[0].node.inner.chain_spec();
    let timestamp = current_timestamp() + 2;
    let attributes = build_payload_attributes(
        timestamp,
        encode_eip1559_params(chain_spec.as_ref(), timestamp)?,
        Some(vec![TX_SET_L1_BLOCK.clone(), deployment_tx]),
    );
    let builder = nodes[0]
        .ext_context
        .as_ref()
        .ok_or_else(|| eyre!("expected Flashblocks context"))?;
    let builder_key = builder.flashblocks_handle.builder_sk()?.verifying_key();
    let authorization =
        create_authorization_generator(parent_hash, builder_key)(attributes.clone());
    let engine = env.node_clients[0].engine.http_client();
    let fcu = FlashblocksEngineApiExtClient::<OpEngineTypes>::flashblocks_fork_choice_updated_v3(
        &engine,
        ForkchoiceState {
            head_block_hash: parent_hash,
            safe_block_hash: parent_hash,
            finalized_block_hash: parent_hash,
        },
        Some(attributes),
        Some(authorization),
    )
    .await?;
    assert!(matches!(
        fcu.payload_status.status,
        PayloadStatusEnum::Valid
    ));
    assert!(fcu.payload_id.is_some());

    // Poll an actual follower's validated receipt feed rather than injecting RPC-store state.
    let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match http
                .request::<Value, _>(
                    "worldchain_getPreconfirmedLogs",
                    rpc_params![filter.clone()],
                )
                .await
            {
                Ok(snapshot) => break Ok::<_, eyre::Report>(snapshot),
                Err(RpcError::Call(error)) if error.code() == -32050 => {}
                Err(error) => return Err(error.into()),
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .wrap_err("timed out waiting for the follower's initial validated view")??;
    assert_eq!(snapshot["version"], 1);
    assert_eq!(snapshot["chainId"], json!(chain_spec.chain.id()));
    assert_eq!(snapshot["status"], "ready");
    assert_eq!(snapshot["canonical"]["number"], 0);
    assert_eq!(snapshot["canonical"]["hash"], json!(parent_hash));
    assert_eq!(snapshot["view"]["parentHash"], json!(parent_hash));
    assert_eq!(snapshot["view"]["blockNumber"], 1);
    assert_eq!(snapshot["hasMore"], false);
    assert!(response_logs(&snapshot).is_empty());

    let mut subscription = ws
        .subscribe::<Value, _>(
            "worldchain_subscribe",
            rpc_params!["preconfirmedLogs", filter.clone()],
            "worldchain_unsubscribe",
        )
        .await?;
    let notification = tokio::time::timeout(Duration::from_secs(2), subscription.next())
        .await?
        .ok_or_else(|| eyre!("subscription closed before its snapshot"))??;
    assert_eq!(notification["type"], "update");
    assert!(response_logs(&notification["result"]).is_empty());

    // Positive tips ensure each transaction improves the warmed payload's fee total.
    let event_tx = raw_tx(
        0,
        eip1559()
            .chain_id(chain_spec.chain.id())
            .to(emitter)
            .gas_limit(100_000)
            .max_fee_per_gas(200_000_000_000)
            .max_priority_fee_per_gas(1_000_000_000)
            .call(),
    )
    .await;
    let tx_hash: B256 = env.node_clients[0]
        .rpc
        .request("eth_sendRawTransaction", rpc_params![event_tx])
        .await?;
    let reverted_tx = raw_tx(
        0,
        eip1559()
            .chain_id(chain_spec.chain.id())
            .nonce(1)
            .to(emitter)
            .input(Bytes::from_static(&[1]))
            .gas_limit(100_000)
            .max_fee_per_gas(200_000_000_000)
            .max_priority_fee_per_gas(1_000_000_000)
            .call(),
    )
    .await;
    let reverted_hash: B256 = env.node_clients[0]
        .rpc
        .request("eth_sendRawTransaction", rpc_params![reverted_tx])
        .await?;
    let notification = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let notification = subscription
                .next()
                .await
                .ok_or_else(|| eyre!("subscription closed before the Registered event"))??;
            assert_eq!(notification["type"], "update");
            if !response_logs(&notification["result"]).is_empty() {
                break Ok::<_, eyre::Report>(notification);
            }
        }
    })
    .await
    .wrap_err("timed out waiting for the follower's Registered log notification")??;
    let streamed_logs = response_logs(&notification["result"]);
    assert_eq!(streamed_logs.len(), 2);
    assert_ne!(streamed_logs[0]["eventId"], streamed_logs[1]["eventId"]);
    for (index, log) in streamed_logs.iter().enumerate() {
        assert_eq!(log["address"], json!(emitter));
        assert_eq!(log["topics"], json!(topics));
        assert_eq!(log["data"], "0x");
        assert_eq!(log["transactionHash"], json!(tx_hash));
        assert_eq!(log["logIndex"], json!(format!("0x{index:x}")));
        assert_eq!(log["removed"], false);
        assert!(log["eventId"].as_str().is_some());
    }
    let reverted_receipt = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let receipt: Option<Value> = http
                .request("eth_getTransactionReceipt", rpc_params![reverted_hash])
                .await?;
            if let Some(receipt) = receipt {
                break Ok::<_, eyre::Report>(receipt);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .wrap_err("timed out waiting for the follower's reverted transaction receipt")??;
    assert_eq!(reverted_receipt["status"], "0x0");
    assert_eq!(reverted_receipt["logs"], json!([]));

    let replay: Value = http
        .request(
            "worldchain_getPreconfirmedLogs",
            rpc_params![filter.clone(), json!({"cursor": snapshot["cursor"]})],
        )
        .await?;
    let replayed_logs = response_logs(&replay);
    assert_eq!(replayed_logs.len(), 2);
    for (replayed, streamed) in replayed_logs.iter().zip(&streamed_logs) {
        assert_eq!(replayed["eventId"], streamed["eventId"]);
    }
    // Wait for the feed to consume the exact validated view containing the failed transaction.
    let snapshot = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot: Value = http
                .request(
                    "worldchain_getPreconfirmedLogs",
                    rpc_params![filter.clone()],
                )
                .await?;
            if snapshot["view"]["blockHash"] == reverted_receipt["blockHash"] {
                break Ok::<_, eyre::Report>(snapshot);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .wrap_err("timed out waiting for the feed view containing the reverted transaction")??;
    let logs = response_logs(&snapshot);
    assert_eq!(logs.len(), 2, "reverted LOG4s must not reach the feed");
    for (log, streamed) in logs.iter().zip(&streamed_logs) {
        assert_eq!(log["transactionHash"], json!(tx_hash));
        assert_eq!(log["eventId"], streamed["eventId"]);
    }
    let replay: Value = http
        .request(
            "worldchain_getPreconfirmedLogs",
            rpc_params![filter, json!({"cursor": replay["cursor"]})],
        )
        .await?;
    assert!(
        response_logs(&replay).is_empty(),
        "cumulative Flashblocks must not repeat the event"
    );
    tokio::time::timeout(Duration::from_secs(2), subscription.unsubscribe()).await??;
    Ok(())
}
