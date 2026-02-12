use alloy_genesis::Genesis;
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_engine::PayloadId;
use ed25519_dalek::SigningKey;
use eyre::eyre::eyre;
use flashblocks_cli::FlashblocksArgs;
use flashblocks_p2p::{
    monitor,
    protocol::handler::{FlashblocksHandle, FlashblocksP2PProtocol, PeerMsg},
};
use flashblocks_primitives::{
    flashblocks::FlashblockMetadata,
    p2p::{
        Authorization, Authorized, AuthorizedMsg, AuthorizedPayload, FlashblocksP2PMsg,
        StartPublish,
    },
    primitives::{ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1},
};
use op_alloy_consensus::{OpPooledTransaction, OpTxEnvelope};
use reth_e2e_test_utils::TmpDB;
use reth_eth_wire::BasicNetworkPrimitives;
use reth_ethereum::network::{NetworkProtocols, protocol::IntoRlpxSubProtocol};
use reth_network::{NetworkHandle, Peers, PeersInfo};
use reth_network_peers::{NodeRecord, PeerId, TrustedPeer};
use reth_node_api::{FullNodeTypesAdapter, NodeTypesWithDBAdapter};
use reth_node_builder::{Node, NodeBuilder, NodeConfig, NodeHandle};
use reth_node_core::{
    args::{DiscoveryArgs, NetworkArgs, RpcServerArgs},
    exit::NodeExitFuture,
};
use reth_optimism_chainspec::OpChainSpecBuilder;
use reth_optimism_primitives::{OpPrimitives, OpReceipt};
use reth_provider::providers::BlockchainProvider;
use reth_tasks::{TaskExecutor, TaskManager};
use reth_tracing::tracing_subscriber::{self, util::SubscriberInitExt};
use serde::{Deserialize, Serialize};
use std::{
    any::Any,
    collections::HashMap,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};
use tempfile::NamedTempFile;
use tokio::time::{Duration, Instant, sleep};
use tracing::{Dispatch, info};
use url::Host;
use world_chain_node::{
    args::{BuilderArgs, PbhArgs, WorldChainArgs},
    config::WorldChainNodeConfig,
    context::FlashblocksContext,
    node::WorldChainNode,
};
use world_chain_test::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR, utils::signer,
};

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct Metadata {
    pub receipts: HashMap<String, OpReceipt>,
    pub new_account_balances: HashMap<String, String>, // Address -> Balance (hex)
    pub block_number: u64,
}

type Network = NetworkHandle<
    BasicNetworkPrimitives<
        OpPrimitives,
        OpPooledTransaction,
        reth_network::types::NewBlock<alloy_consensus::Block<OpTxEnvelope>>,
    >,
>;

pub struct NodeContext {
    p2p_handle: FlashblocksHandle,
    pub local_node_record: NodeRecord,
    http_api_addr: SocketAddr,
    _node_exit_future: NodeExitFuture,
    _node: Box<dyn Any + Sync + Send>,
    network_handle: Network,
}

impl NodeContext {
    pub async fn provider(&self) -> eyre::Result<RootProvider> {
        let url = format!("http://{}", self.http_api_addr);
        let client = RpcClient::builder().http(url.parse()?);

        Ok(RootProvider::new(client))
    }
}

fn init_tracing(filter: &str) -> tracing::subscriber::DefaultGuard {
    let sub = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_target(false)
        .without_time()
        .finish();

    Dispatch::new(sub).set_default()
}

async fn setup_node(
    exec: TaskExecutor,
    authorizer_sk: SigningKey,
    builder_sk: SigningKey,
    peers: Vec<(PeerId, SocketAddr)>,
) -> eyre::Result<NodeContext> {
    setup_node_extended_cfg(exec, authorizer_sk, builder_sk, peers, None, None).await
}

async fn setup_node_extended_cfg(
    exec: TaskExecutor,
    authorizer_sk: SigningKey,
    builder_sk: SigningKey,
    peers: Vec<(PeerId, SocketAddr)>,
    port: Option<u16>,
    p2p_secret_key: Option<PathBuf>,
) -> eyre::Result<NodeContext> {
    let genesis: Genesis = serde_json::from_str(include_str!("assets/genesis.json")).unwrap();
    let chain_spec = Arc::new(
        OpChainSpecBuilder::base_mainnet()
            .genesis(genesis)
            .ecotone_activated()
            .build(),
    );

    let mut network_config = NetworkArgs {
        discovery: DiscoveryArgs {
            disable_discovery: true,
            ..DiscoveryArgs::default()
        },
        ..NetworkArgs::default()
    };

    // Set the port if one was specified
    if let Some(p) = port {
        network_config.port = p;
    }

    // Set the P2P secret key if one was specified (for deterministic peer ID)
    if let Some(key_path) = p2p_secret_key {
        network_config.p2p_secret_key = Some(key_path);
    }

    // Add trusted peers via NetworkArgs (simulate --trusted-peers CLI flag)
    network_config.trusted_peers = peers
        .into_iter()
        .map(|(peer_id, addr)| {
            let host = match addr.ip() {
                IpAddr::V4(ip) => Host::Ipv4(ip),
                IpAddr::V6(ip) => Host::Ipv6(ip),
            };
            TrustedPeer::new(host, addr.port(), peer_id)
        })
        .collect();

    // Use with_unused_ports() only if no specific port was provided
    // This lets Reth allocate random ports to avoid port collisions
    let mut node_config = NodeConfig::new(chain_spec.clone())
        .with_network(network_config.clone())
        .with_rpc(RpcServerArgs::default().with_unused_ports().with_http());

    if port.is_none() {
        node_config = node_config.with_unused_ports();
    }

    let pbh = PbhArgs {
        verified_blockspace_capacity: 70,
        entrypoint: PBH_DEV_ENTRYPOINT,
        signature_aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
        world_id: DEV_WORLD_ID,
    };

    let builder = BuilderArgs {
        enabled: false,
        private_key: signer(6),
    };

    let world_chain_node_config = WorldChainNodeConfig {
        args: WorldChainArgs {
            rollup: Default::default(),
            builder,
            pbh,
            flashblocks: Some(FlashblocksArgs {
                enabled: true,
                authorizer_vk: Some(authorizer_sk.verifying_key()),
                builder_sk: Some(builder_sk.clone()),
                spoof_authorizer_sk: None,
                flashblocks_interval: 200,
                recommit_interval: 200,
                access_list: true,
            }),
            tx_peers: None,
        },
        builder_config: Default::default(),
    };

    let node = WorldChainNode::<FlashblocksContext>::new(
        world_chain_node_config
            .args
            .clone()
            .into_config(&chain_spec)?,
    );

    let ext_context = node.ext_context::<FullNodeTypesAdapter<
        WorldChainNode<FlashblocksContext>,
        TmpDB,
        BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode<FlashblocksContext>, TmpDB>>,
    >>();
    // Safe unwrap because node has flashblocks enabled
    let p2p_handle = ext_context.unwrap().flashblocks_handle.clone();

    let NodeHandle {
        node,
        node_exit_future,
    } = NodeBuilder::new(node_config.clone())
        .testing_node(exec.clone())
        .with_types_and_provider::<WorldChainNode<FlashblocksContext>, BlockchainProvider<_>>()
        .with_components(node.components_builder())
        .with_add_ons(node.add_ons())
        .launch()
        .await?;

    let p2p_protocol = FlashblocksP2PProtocol {
        network: node.network.clone(),
        handle: p2p_handle.clone(),
    };

    node.network
        .add_rlpx_sub_protocol(p2p_protocol.into_rlpx_sub_protocol());

    // Trusted peers are now added via NetworkArgs (like CLI), so no manual addition needed

    let http_api_addr = node
        .rpc_server_handle()
        .http_local_addr()
        .ok_or_else(|| eyre!("Failed to get http api address"))?;

    let network_handle = node.network.clone();
    let local_node_record = network_handle.local_node_record();

    Ok(NodeContext {
        p2p_handle,
        local_node_record,
        http_api_addr,
        _node_exit_future: node_exit_future,
        _node: Box::new(node),
        network_handle,
    })
}

fn base_payload(
    block_number: u64,
    payload_id: PayloadId,
    index: u64,
    hash: B256,
) -> FlashblocksPayloadV1 {
    FlashblocksPayloadV1 {
        payload_id,
        index,
        base: Some(ExecutionPayloadBaseV1 {
            parent_beacon_block_root: B256::default(),
            parent_hash: hash,
            fee_recipient: Address::ZERO,
            prev_randao: B256::default(),
            block_number,
            gas_limit: 0,
            timestamp: 0,
            extra_data: Bytes::new(),
            base_fee_per_gas: U256::ZERO,
        }),
        metadata: FlashblockMetadata::default(),
        diff: ExecutionPayloadFlashblockDeltaV1::default(),
    }
}

fn next_payload(payload_id: PayloadId, index: u64) -> FlashblocksPayloadV1 {
    let tx1 = Bytes::from_str("0x7ef8f8a042a8ae5ec231af3d0f90f68543ec8bca1da4f7edd712d5b51b490688355a6db794deaddeaddeaddeaddeaddeaddeaddeaddead00019442000000000000000000000000000000000000158080830f424080b8a4440a5e200000044d000a118b00000000000000040000000067cb7cb0000000000077dbd4000000000000000000000000000000000000000000000000000000000000000a00000000000000000000000000000000000000000000000000000000000000014edd27304108914dd6503b19b9eeb9956982ef197febbeeed8a9eac3dbaaabdf000000000000000000000000fc56e7272eebbba5bc6c544e159483c4a38f8ba3").unwrap();
    let tx2 = Bytes::from_str("0xf8cd82016d8316e5708302c01c94f39635f2adf40608255779ff742afe13de31f57780b8646e530e9700000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000001bc16d674ec8000000000000000000000000000000000000000000000000000156ddc81eed2a36d68302948ba0a608703e79b22164f74523d188a11f81c25a65dd59535bab1cd1d8b30d115f3ea07f4cfbbad77a139c9209d3bded89091867ff6b548dd714109c61d1f8e7a84d14").unwrap();

    // Send another test flashblock payload
    FlashblocksPayloadV1 {
        payload_id,
        index,
        base: None,
        diff: ExecutionPayloadFlashblockDeltaV1 {
            state_root: B256::default(),
            receipts_root: B256::default(),
            gas_used: 0,
            block_hash: B256::default(),
            transactions: vec![tx1, tx2],
            withdrawals: Vec::new(),
            logs_bloom: Default::default(),
            withdrawals_root: Default::default(),
            access_list_data: Default::default(),
        },
        metadata: FlashblockMetadata::default(),
    }
}

async fn setup_nodes(n: u8) -> eyre::Result<(Vec<NodeContext>, SigningKey)> {
    let mut nodes = Vec::new();
    let mut peers = Vec::new();
    let tasks = Box::leak(Box::new(TaskManager::current()));
    let exec = Box::leak(Box::new(tasks.executor()));
    let authorizer = SigningKey::from_bytes(&[0; 32]);

    for i in 0..n {
        let builder = SigningKey::from_bytes(&[(i + 1) % n; 32]);
        let node = setup_node(exec.clone(), authorizer.clone(), builder, peers.clone()).await?;
        let enr = node.local_node_record;
        peers.push((enr.id, enr.tcp_addr()));
        nodes.push(node);
    }

    sleep(Duration::from_millis(6000)).await;

    Ok((nodes, authorizer))
}

#[tokio::test]
#[ignore = "flaky"]
async fn test_double_failover() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let (nodes, authorizer) = setup_nodes(3).await?;

    let mut publish_flashblocks = nodes[0].p2p_handle.ctx.flashblock_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(payload) = publish_flashblocks.recv().await {
            println!("\n////////////////////////////////////////////////////////////////////\n");
            println!(
                "Received flashblock, payload_id: {}, index: {}",
                payload.payload_id, payload.index
            );
            println!("\n////////////////////////////////////////////////////////////////////\n");
        }
    });

    let latest_block = nodes[0]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    assert_eq!(latest_block.number(), 0);

    // Querying pending block when it does not exists yet
    let pending_block = nodes[0]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?;
    assert_eq!(pending_block.unwrap().number(), latest_block.number());

    let payload_0 = base_payload(0, PayloadId::new([0; 8]), 0, latest_block.hash());
    let authorization_0 = Authorization::new(
        payload_0.payload_id,
        0,
        &authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let msg = payload_0.clone();
    let authorized_0 =
        AuthorizedPayload::new(nodes[0].p2p_handle.builder_sk()?, authorization_0, msg);
    nodes[0].p2p_handle.start_publishing(authorization_0)?;
    nodes[0].p2p_handle.publish_new(authorized_0).unwrap();
    sleep(Duration::from_millis(100)).await;

    let payload_1 = next_payload(payload_0.payload_id, 1);
    let authorization_1 = Authorization::new(
        payload_1.payload_id,
        0,
        &authorizer,
        nodes[1].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized_1 = AuthorizedPayload::new(
        nodes[1].p2p_handle.builder_sk()?,
        authorization_1,
        payload_1.clone(),
    );
    nodes[1].p2p_handle.start_publishing(authorization_1)?;
    sleep(Duration::from_millis(100)).await;
    nodes[1].p2p_handle.publish_new(authorized_1).unwrap();
    sleep(Duration::from_millis(100)).await;

    // Send a new block, this time from node 1
    let payload_2 = next_payload(payload_0.payload_id, 2);
    let msg = payload_2.clone();
    let authorization_2 = Authorization::new(
        payload_2.payload_id,
        0,
        &authorizer,
        nodes[2].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized_2 = AuthorizedPayload::new(
        nodes[2].p2p_handle.builder_sk()?,
        authorization_2,
        msg.clone(),
    );
    nodes[2].p2p_handle.start_publishing(authorization_2)?;
    sleep(Duration::from_millis(100)).await;
    nodes[2].p2p_handle.publish_new(authorized_2).unwrap();
    sleep(Duration::from_millis(100)).await;

    Ok(())
}

#[tokio::test]
#[ignore = "flaky"]
async fn test_force_race_condition() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let (nodes, authorizer) = setup_nodes(3).await?;

    let mut publish_flashblocks = nodes[0].p2p_handle.ctx.flashblock_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(payload) = publish_flashblocks.recv().await {
            println!("\n////////////////////////////////////////////////////////////////////\n");
            println!(
                "Received flashblock, payload_id: {}, index: {}",
                payload.payload_id, payload.index
            );
            println!("\n////////////////////////////////////////////////////////////////////\n");
        }
    });

    let latest_block = nodes[0]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    assert_eq!(latest_block.number(), 0);

    // Querying pending block when it does not exists yet
    let pending_block = nodes[0]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?;
    assert_eq!(pending_block.unwrap().number(), latest_block.number());

    let payload_0 = base_payload(0, PayloadId::new([0; 8]), 0, latest_block.hash());
    info!("Sending payload 0, index 0");
    let authorization = Authorization::new(
        payload_0.payload_id,
        0,
        &authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let msg = payload_0.clone();
    let authorized = AuthorizedPayload::new(nodes[0].p2p_handle.builder_sk()?, authorization, msg);
    nodes[0].p2p_handle.start_publishing(authorization)?;
    nodes[0].p2p_handle.publish_new(authorized).unwrap();
    sleep(Duration::from_millis(100)).await;

    // Query pending block after sending the base payload with an empty delta
    let pending_block = nodes[1]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?
        .expect("pending block expected");

    assert_eq!(pending_block.number(), 0);
    assert_eq!(pending_block.transactions.hashes().len(), 0);

    info!("Sending payload 0, index 1");
    let payload_1 = next_payload(payload_0.payload_id, 1);
    let authorization = Authorization::new(
        payload_1.payload_id,
        0,
        &authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(
        nodes[0].p2p_handle.builder_sk()?,
        authorization,
        payload_1.clone(),
    );
    nodes[0].p2p_handle.publish_new(authorized).unwrap();
    sleep(Duration::from_millis(100)).await;

    // Query pending block after sending the second payload with two transactions
    let block = nodes[1]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?
        .expect("pending block expected");

    assert_eq!(block.number(), 0);
    assert_eq!(block.transactions.hashes().len(), 2);

    // Send a new block, this time from node 1
    let payload_2 = base_payload(1, PayloadId::new([1; 8]), 0, latest_block.hash());
    info!("Sending payload 1, index 0");
    let authorization_1 = Authorization::new(
        payload_2.payload_id,
        1,
        &authorizer,
        nodes[1].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorization_2 = Authorization::new(
        payload_2.payload_id,
        1,
        &authorizer,
        nodes[2].p2p_handle.builder_sk()?.verifying_key(),
    );
    let msg = payload_2.clone();
    let authorized_1 = AuthorizedPayload::new(
        nodes[1].p2p_handle.builder_sk()?,
        authorization_1,
        msg.clone(),
    );
    nodes[1].p2p_handle.start_publishing(authorization_1)?;
    nodes[2].p2p_handle.start_publishing(authorization_2)?;
    // Wait for clearance to go through
    sleep(Duration::from_millis(100)).await;
    tracing::error!(
        "{}",
        nodes[1]
            .p2p_handle
            .publish_new(authorized_1.clone())
            .unwrap_err()
    );
    sleep(Duration::from_millis(100)).await;

    nodes[2].p2p_handle.stop_publishing()?;
    sleep(Duration::from_millis(100)).await;

    nodes[1].p2p_handle.publish_new(authorized_1)?;
    sleep(Duration::from_millis(100)).await;

    Ok(())
}

#[tokio::test]
#[ignore = "flaky"]
async fn test_get_block_by_number_pending() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let (nodes, authorizer) = setup_nodes(1).await?;

    let provider = nodes[0].provider().await?;

    let latest_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    assert_eq!(latest_block.number(), 0);

    // Querying pending block when it does not exists yet
    let pending_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?;
    assert_eq!(pending_block.unwrap().number(), latest_block.number());

    let payload_id = PayloadId::new([0; 8]);
    let base_payload = base_payload(0, payload_id, 0, latest_block.hash());
    let authorization = Authorization::new(
        base_payload.payload_id,
        0,
        &authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(
        nodes[0].p2p_handle.builder_sk()?,
        authorization,
        base_payload,
    );
    nodes[0].p2p_handle.start_publishing(authorization)?;
    nodes[0].p2p_handle.publish_new(authorized).unwrap();
    sleep(Duration::from_millis(100)).await;

    // Query pending block after sending the base payload with an empty delta
    let pending_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?
        .expect("pending block expected");

    assert_eq!(pending_block.number(), 0);
    assert_eq!(pending_block.transactions.hashes().len(), 0);

    let next_payload = next_payload(payload_id, 1);
    let authorization = Authorization::new(
        next_payload.payload_id,
        0,
        &authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(
        nodes[0].p2p_handle.builder_sk()?,
        authorization,
        next_payload,
    );
    nodes[0].p2p_handle.start_publishing(authorization)?;
    nodes[0].p2p_handle.publish_new(authorized).unwrap();
    sleep(Duration::from_millis(100)).await;

    // Query pending block after sending the second payload with two transactions
    let block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?
        .expect("pending block expected");

    assert_eq!(block.number(), 0);
    assert_eq!(block.transactions.hashes().len(), 2);

    Ok(())
}

#[tokio::test]
async fn test_peer_reputation() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let (nodes, _authorizer) = setup_nodes(2).await?;

    let mut publish_flashblocks = nodes[0].p2p_handle.ctx.flashblock_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(payload) = publish_flashblocks.recv().await {
            println!("\n////////////////////////////////////////////////////////////////////\n");
            println!(
                "Received flashblock, payload_id: {}, index: {}",
                payload.payload_id, payload.index
            );
            println!("\n////////////////////////////////////////////////////////////////////\n");
        }
    });

    let invalid_authorizer = SigningKey::from_bytes(&[99; 32]);
    let latest_block = nodes[0]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");

    let payload_0 = base_payload(0, PayloadId::new([0; 8]), 0, latest_block.hash());
    info!("Sending bad authorization");
    let authorization = Authorization::new(
        payload_0.payload_id,
        0,
        &invalid_authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );

    let authorized_msg = AuthorizedMsg::StartPublish(StartPublish);
    let authorized_payload = Authorized::new(
        nodes[0].p2p_handle.builder_sk()?,
        authorization,
        authorized_msg,
    );
    let p2p_msg = FlashblocksP2PMsg::Authorized(authorized_payload);
    let peer_msg = PeerMsg::StartPublishing(p2p_msg.encode());

    let peers = nodes[1].network_handle.get_all_peers().await?;
    let peer_0 = &peers[0].remote_id;

    for _ in 0..100 {
        nodes[0].p2p_handle.ctx.peer_tx.send(peer_msg.clone()).ok();
        sleep(Duration::from_millis(10)).await;
        let rep_0 = nodes[1].network_handle.reputation_by_id(*peer_0).await?;
        if let Some(rep) = rep_0 {
            assert!(rep < 0, "Peer reputation should be negative");
        }
    }

    // Assert that the peer is banned
    assert!(nodes[1].network_handle.get_all_peers().await?.is_empty());

    Ok(())
}

#[tokio::test]
#[tracing_test::traced_test]
async fn test_peer_monitoring() -> eyre::Result<()> {
    let authorizer = SigningKey::from_bytes(&[0; 32]);

    // Create a temporary P2P secret key file for node1 to ensure consistent peer ID across restarts
    let mut p2p_key_file = NamedTempFile::new()?;
    p2p_key_file.write_all(b"0101010101010101010101010101010101010101010101010101010101010101")?;
    p2p_key_file.flush()?;
    let p2p_key_path = p2p_key_file.path().to_path_buf();

    let tasks1 = TaskManager::new(tokio::runtime::Handle::current());
    let exec1 = tasks1.executor();

    // Setup node 1 with its own TaskManager (for isolated task cancellation)
    // Node1 needs static port and P2P key so it can be restarted with same identity
    let builder1 = SigningKey::from_bytes(&[1; 32]);
    let node1 = setup_node_extended_cfg(
        exec1,
        authorizer.clone(),
        builder1,
        vec![],                     // No peers initially
        None,                       // Use random port (we'll capture it)
        Some(p2p_key_path.clone()), // Use deterministic P2P key
    )
    .await?;

    let tasks2 = TaskManager::current();
    let exec2 = tasks2.executor();

    // Capture node1's identity for node2 to use as trusted peer
    let peer1_id = node1.local_node_record.id;
    let peer1_addr = node1.local_node_record.tcp_addr();
    let peer1_port = peer1_addr.port(); // Capture port for restart

    // Setup node 2 with leaked TaskManager (lives for entire test)
    // Node2 has node1 configured as trusted peer via CLI-style setup
    let builder2 = SigningKey::from_bytes(&[2; 32]);
    let node2 = setup_node_extended_cfg(
        exec2.clone(),
        authorizer.clone(),
        builder2,
        vec![(peer1_id, peer1_addr)], // Node1 as trusted peer
        None,                         // Use random port
        None,                         // No deterministic P2P key needed
    )
    .await?;

    let start = Instant::now();

    loop {
        let trusted_peers = node2.network_handle.get_trusted_peers().await?;
        if trusted_peers.len() == 1 {
            info!(
                "Connection established in {:.2}s",
                start.elapsed().as_secs_f64()
            );
            break;
        }

        if start.elapsed() > Duration::from_secs(10) {
            panic!("Timeout waiting for connection to establish");
        }

        sleep(Duration::from_millis(100)).await;
    }
    // Wait for PeerMonitor's periodic check to discover the trusted peer
    // Since PeerAdded events are ignored, the monitor relies on periodic polling
    // Wait for at least one full monitor interval + buffer (1s interval + 1s buffer)
    sleep(monitor::PEER_MONITOR_INTERVAL + Duration::from_secs(1)).await;

    // SIMULATE CRASH: Drop node1 and its TaskManager
    // Dropping the TaskManager cancels all tasks spawned on it
    info!("Simulating node 1 crash (dropping node and TaskManager)");
    drop(node1);
    drop(tasks1);

    // Wait for the event listener to process the SessionClosed event
    sleep(Duration::from_millis(500)).await;

    // Check that disconnection was logged by the event listener (immediate detection)
    logs_assert(|logs: &[&str]| {
        let disconnect_log_exists = logs.iter().any(|log| {
            log.contains("trusted peer disconnected") && log.contains(&peer1_id.to_string())
        });

        assert!(
            disconnect_log_exists,
            "Should have logged 'trusted peer disconnected' for peer {} from event listener",
            peer1_id
        );
        Ok(())
    });

    // Wait for PeerMonitor periodic checks to detect the disconnection and emit multiple warning logs
    // Wait for at least 2 periodic ticks to ensure we get multiple log outputs (1s * 3 + 1s buffer for safety)
    sleep(monitor::PEER_MONITOR_INTERVAL * 2 + Duration::from_secs(1)).await;

    // Verify that node 1 is no longer connected
    let trusted_peers_after = node2.network_handle.get_trusted_peers().await?;
    assert_eq!(
        trusted_peers_after.len(),
        0,
        "Node 1 should have disconnected"
    );

    // Wait longer to ensure we accumulate multiple warnings before reconnection
    // This gives the peer monitor more time to detect and log the disconnection
    sleep(Duration::from_secs(2)).await;

    // Test reconnection: restart node 1 and verify it can reconnect
    info!("Testing peer reconnection after restart");
    info!("Restarting node 1 with the same port and P2P key");

    // Restart node 1 with node2 configured as trusted peer (CLI-style)
    let node1_restarted = setup_node_extended_cfg(
        exec2.clone(),
        authorizer.clone(),
        SigningKey::from_bytes(&[1; 32]),
        vec![(
            node2.local_node_record.id,
            node2.local_node_record.tcp_addr(),
        )], // Configure node2 as trusted peer
        Some(peer1_port),           // Reuse the same port
        Some(p2p_key_path.clone()), // Reuse the same P2P key
    )
    .await?;
    let peer1_id_new = node1_restarted.local_node_record.id;
    let peer1_addr_new = node1_restarted.local_node_record.tcp_addr();

    // Verify the address (IP:port) is the same after restart
    assert_eq!(
        peer1_addr, peer1_addr_new,
        "Peer address should remain the same after restart (fixed port)"
    );

    // Verify the peer ID is the same after restart (because we reused the P2P key)
    assert_eq!(
        peer1_id, peer1_id_new,
        "Peer ID should remain the same after restart (reused P2P key)"
    );

    info!(
        "Restarted peer with same ID and address: peer_id={}, addr={}",
        peer1_id_new, peer1_addr_new
    );

    let connection_timeout = Duration::from_secs(10);
    let poll_interval = Duration::from_millis(100);
    let start = Instant::now();

    loop {
        let trusted_peers = node2.network_handle.get_trusted_peers().await?;
        if trusted_peers.len() == 1 {
            info!(
                "Reconnection established in {:.2}s",
                start.elapsed().as_secs_f64()
            );
            break;
        }

        if start.elapsed() > connection_timeout {
            panic!("Timeout waiting for reconnection to establish");
        }

        sleep(poll_interval).await;
    }

    // Assert that the "connection to trusted peer established" log appears for node1
    logs_assert(|logs: &[&str]| {
        let reconnection_log_exists = logs.iter().any(|log| {
            log.contains("connection to trusted peer established")
                && log.contains(&peer1_id.to_string())
        });

        assert!(
            reconnection_log_exists,
            "Should have logged 'connection to trusted peer established' for peer {} after restart",
            peer1_id
        );
        Ok(())
    });

    // Wait for at least one more monitor tick to verify warnings stopped (1s interval + 1s buffer)
    sleep(monitor::PEER_MONITOR_INTERVAL + Duration::from_secs(1)).await;

    // Count the number of warning logs before and after reconnection to ensure they stopped
    logs_assert(|logs: &[&str]| {
        // Find the index where reconnection happened (use rposition to find the LAST occurrence)
        let reconnection_log_idx = logs
            .iter()
            .rposition(|log| log.contains("connection to trusted peer established"))
            .ok_or_else(|| {
                "Could not find 'connection to trusted peer established' log".to_string()
            })?;

        // Split logs at the reconnection point
        let (logs_before_reconnect, logs_after_reconnect) = logs.split_at(reconnection_log_idx);

        // Filter for disconnect warnings in logs before reconnection
        let warnings_before_reconnect: Vec<&str> = logs_before_reconnect
            .iter()
            .filter(|log| {
                log.contains(&peer1_id.to_string())
                    && log.contains("WARN")
                    && log.contains("trusted peer disconnected")
            })
            .copied()
            .collect();

        // We should have seen at least 2 warnings before reconnection
        assert!(
            warnings_before_reconnect.len() >= 2,
            "Should have had at least 2 warnings before reconnection, found {}",
            warnings_before_reconnect.len()
        );

        // Filter for disconnect warnings in logs after reconnection
        let warnings_after_reconnect: Vec<&str> = logs_after_reconnect
            .iter()
            .filter(|log| {
                log.contains(&peer1_id.to_string())
                    && log.contains("WARN")
                    && log.contains("trusted peer disconnected")
            })
            .copied()
            .collect();

        // There should be no warnings after reconnection
        assert!(
            warnings_after_reconnect.is_empty(),
            "Should have no warnings after reconnection, found {}: {:?}",
            warnings_after_reconnect.len(),
            warnings_after_reconnect
        );

        Ok(())
    });

    Ok(())
}
