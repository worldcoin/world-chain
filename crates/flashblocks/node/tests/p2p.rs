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
    protocol::{
        connection::ReceiveStatus,
        handler::{FlashblocksHandle, PublishingStatus},
    },
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
use reth_network::{NetworkHandle, Peers, PeersInfo};
use reth_network_peers::{NodeRecord, PeerId, TrustedPeer};
use reth_node_api::{ FullNodeTypesAdapter, NodeTypesWithDBAdapter};
use reth_node_builder::{Node, NodeBuilder, NodeConfig, NodeHandle};
use reth_node_core::{
    args::{DiscoveryArgs, NetworkArgs, RpcServerArgs},
    exit::NodeExitFuture,
};
use reth_optimism_chainspec::OpChainSpecBuilder;
use reth_optimism_node::OpEngineTypes;
use reth_optimism_primitives::{OpPrimitives, OpReceipt};
use reth_provider::providers::BlockchainProvider;
use reth_tasks::TaskExecutor;
use reth_tracing::tracing_subscriber::{self, util::SubscriberInitExt};
use serde::{Deserialize, Serialize};
use std::{
    any::Any,
    collections::HashMap,
    fmt,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
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
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
    utils::{account, eip1559, raw_tx, signer},
};

/// Thread-safe log buffer for capturing tracing output across threads.
#[derive(Clone, Default)]
struct SharedLogBuffer(Arc<Mutex<Vec<String>>>);

impl SharedLogBuffer {
    fn logs(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SharedLogBuffer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = LogVisitor(String::new());
        visitor
            .0
            .push_str(&format!("{} ", event.metadata().level()));
        visitor
            .0
            .push_str(&format!("{}: ", event.metadata().target()));
        event.record(&mut visitor);
        self.0.lock().unwrap().push(visitor.0);
    }
}

struct LogVisitor(String);

impl tracing::field::Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0.push_str(&format!("{:?}", value));
        } else {
            self.0.push_str(&format!(" {}={:?}", field.name(), value));
        }
    }
}

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

const AUTH_TS_BASE: u64 = 1;
const AUTH_TS_NEXT: u64 = 2;

fn test_payload_id(id: u8) -> PayloadId {
    PayloadId::new([id; 8])
}

pub struct NodeContext {
    p2p_handle: FlashblocksHandle,
    pub local_node_record: NodeRecord,
    http_api_addr: SocketAddr,
    _node_exit_future: NodeExitFuture,
    _node: Box<dyn Any + Sync + Send>,
    network_handle: Network,
    #[allow(dead_code)]
    beacon_handle: reth_node_api::ConsensusEngineHandle<OpEngineTypes>,
}

struct NodeTestFixture {
    nodes: Vec<NodeContext>,
    authorizer: SigningKey,
    _exec: TaskExecutor,
}

impl NodeTestFixture {
    fn nodes(&self) -> &[NodeContext] {
        &self.nodes
    }

    fn authorizer(&self) -> &SigningKey {
        &self.authorizer
    }
}

impl NodeContext {
    pub async fn provider(&self) -> eyre::Result<RootProvider> {
        let url = format!("http://{}", self.http_api_addr);
        let client = RpcClient::builder().http(url.parse()?);

        Ok(RootProvider::new(client))
    }

    /// Poll the pending block until it matches the expected number and tx count,
    /// or time out. process_flashblock runs on spawn_blocking so results are
    /// not immediately visible after publish.
    pub async fn wait_for_pending_block(
        &self,
        expected_number: u64,
        expected_txs: usize,
    ) -> eyre::Result<()> {
        let provider = self.provider().await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut last = String::new();

        loop {
            let pending = provider
                .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
                .await?;

            if let Some(ref block) = pending {
                if block.number() == expected_number
                    && block.transactions.hashes().len() == expected_txs
                {
                    return Ok(());
                }
                last = format!(
                    "number={}, txs={}",
                    block.number(),
                    block.transactions.hashes().len()
                );
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(eyre!(
                    "timed out waiting for pending block: expected number={expected_number} txs={expected_txs}, last observed: {last}"
                ));
            }

            sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn wait_for_pending_block(
    node: &NodeContext,
    expected_number: u64,
    expected_txs: usize,
) -> eyre::Result<()> {
    let provider = node.provider().await?;
    let timeout = Duration::from_secs(10);
    let poll_interval = Duration::from_millis(50);
    let start = Instant::now();
    let mut last_observed = "no pending block".to_string();

    loop {
        let pending_block = provider
            .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
            .await?;

        if let Some(pending_block) = pending_block {
            let observed_number = pending_block.number();
            let observed_txs = pending_block.transactions.hashes().len();
            if observed_number == expected_number && observed_txs == expected_txs {
                return Ok(());
            }

            last_observed = format!("number {observed_number}, txs {observed_txs}");
        }

        if start.elapsed() >= timeout {
            return Err(eyre!(
                "timed out waiting for pending block state: expected number {expected_number}, txs {expected_txs}; last observed {last_observed}"
            ));
        }

        sleep(poll_interval).await;
    }
}

async fn wait_for_trusted_peers(
    node: &NodeContext,
    expected_connections: usize,
) -> eyre::Result<()> {
    let timeout = Duration::from_secs(10);
    let poll_interval = Duration::from_millis(100);
    let start = Instant::now();

    loop {
        let trusted_peers = node.network_handle.get_trusted_peers().await?;
        if trusted_peers.len() == expected_connections {
            return Ok(());
        }

        if start.elapsed() >= timeout {
            return Err(eyre!(
                "timed out waiting for trusted peers: expected {expected_connections}, last observed {}",
                trusted_peers.len()
            ));
        }

        sleep(poll_interval).await;
    }
}

async fn wait_for_flashblocks_topology(
    node: &NodeContext,
    expected_connections: usize,
    expected_receive_peers: usize,
) -> eyre::Result<(Vec<PeerId>, Vec<PeerId>)> {
    let timeout = Duration::from_secs(10);
    let poll_interval = Duration::from_millis(100);
    let start = Instant::now();

    loop {
        {
            let state = node.p2p_handle.state.lock();
            if state.peers.len() == expected_connections {
                let receive_peers: Vec<_> = state
                    .peers
                    .iter()
                    .filter_map(|(peer_id, conn)| {
                        matches!(conn.receive_status, ReceiveStatus::Receiving { .. })
                            .then_some(*peer_id)
                    })
                    .collect();
                let candidate_peers: Vec<_> = state
                    .peers
                    .iter()
                    .filter_map(|(peer_id, conn)| {
                        (conn.receive_status == ReceiveStatus::NotReceiving).then_some(*peer_id)
                    })
                    .collect();
                drop(state);

                if receive_peers.len() == expected_receive_peers
                    && receive_peers.len() + candidate_peers.len() == expected_connections
                {
                    return Ok((receive_peers, candidate_peers));
                }
            } else {
                drop(state);
            }

            if start.elapsed() >= timeout {
                return Err(eyre!(
                    "timed out waiting for flashblocks topology: expected {expected_connections} connections with {expected_receive_peers} receive peers"
                ));
            }
        }

        sleep(poll_interval).await;
    }
}

fn receive_peer_score(node: &NodeContext, peer_id: PeerId) -> eyre::Result<Option<i64>> {
    let state = node.p2p_handle.state.lock();
    let peer = state
        .peers
        .get(&peer_id)
        .ok_or_else(|| eyre!("peer {peer_id} not found"))?;

    let ReceiveStatus::Receiving { score } = &peer.receive_status else {
        return Ok(None);
    };

    let debug = format!("{score:?}");
    let value = debug
        .split("value: ")
        .nth(1)
        .and_then(|rest| rest.strip_prefix("Some("))
        .and_then(|rest| rest.split(')').next())
        .ok_or_else(|| eyre!("failed to parse score from {debug}"))?
        .parse::<i64>()?;

    Ok(Some(value))
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

fn test_flashblocks_args(authorizer_sk: &SigningKey, builder_sk: &SigningKey) -> FlashblocksArgs {
    FlashblocksArgs {
        enabled: true,
        authorizer_vk: Some(authorizer_sk.verifying_key()),
        builder_sk: Some(builder_sk.clone()),
        force_publish: false,
        override_authorizer_sk: None,
        flashblocks_interval: 200,
        recommit_interval: 200,
        access_list: true,
        fanout: Default::default(),
    }
}

async fn setup_node_extended_cfg(
    exec: TaskExecutor,
    authorizer_sk: SigningKey,
    builder_sk: SigningKey,
    peers: Vec<(PeerId, SocketAddr)>,
    port: Option<u16>,
    p2p_secret_key: Option<PathBuf>,
    flashblocks_args: Option<FlashblocksArgs>,
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
        block_uncompressed_size_limit: None,
    };

    let world_chain_node_config = WorldChainNodeConfig {
        args: WorldChainArgs {
            rollup: Default::default(),
            builder,
            pbh,
            flashblocks: Some(
                flashblocks_args
                    .unwrap_or_else(|| test_flashblocks_args(&authorizer_sk, &builder_sk)),
            ),
            tx_peers: None,
            disable_bootnodes: true,
        },
        builder_config: Default::default(),
    };

    let node = WorldChainNode::<FlashblocksContext>::new(
        world_chain_node_config
            .args
            .clone()
            .into_config(&mut node_config)?,
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

    // The flashblocks sub-protocol is now registered during build_network
    // (before the network starts), so no manual registration is needed here.

    let http_api_addr = node
        .rpc_server_handle()
        .http_local_addr()
        .ok_or_else(|| eyre!("Failed to get http api address"))?;

    let network_handle = node.network.clone();
    let local_node_record = network_handle.local_node_record();
    let beacon_handle = node.add_ons_handle.beacon_engine_handle.clone();

    Ok(NodeContext {
        p2p_handle,
        local_node_record,
        http_api_addr,
        _node_exit_future: node_exit_future,
        _node: Box::new(node),
        network_handle,
        beacon_handle,
    })
}

fn base_payload(
    block_number: u64,
    payload_id: PayloadId,
    index: u64,
    hash: B256,
    timestamp: u64,
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
            timestamp,
            extra_data: Bytes::new(),
            base_fee_per_gas: U256::ZERO,
        }),
        metadata: FlashblockMetadata::default(),
        diff: ExecutionPayloadFlashblockDeltaV1::default(),
    }
}

async fn next_payload(payload_id: PayloadId, index: u64) -> FlashblocksPayloadV1 {
    let tx1 = raw_tx(
        0,
        eip1559()
            .chain_id(8453)
            .nonce(0)
            .max_fee_per_gas(2_000_000_000u128)
            .max_priority_fee_per_gas(100_000_000u128)
            .to(account(1))
            .call(),
    )
    .await;
    let tx2 = raw_tx(
        0,
        eip1559()
            .chain_id(8453)
            .nonce(1)
            .max_fee_per_gas(2_000_000_000u128)
            .max_priority_fee_per_gas(100_000_000u128)
            .to(account(2))
            .call(),
    )
    .await;

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

async fn publish_flashblock_with_latency(
    sender: &NodeContext,
    authorizer: &SigningKey,
    payload_id: PayloadId,
    authorization_timestamp: u64,
    simulated_latency: Duration,
) -> eyre::Result<()> {
    let latest_block = sender
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    let mut payload = base_payload(
        0,
        payload_id,
        0,
        latest_block.hash(),
        authorization_timestamp,
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos() as i64;
    payload.metadata.flashblock_timestamp = Some(now - simulated_latency.as_nanos() as i64);
    let authorization = Authorization::new(
        payload.payload_id,
        authorization_timestamp,
        authorizer,
        sender.p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized =
        AuthorizedPayload::new(sender.p2p_handle.builder_sk()?, authorization, payload);

    {
        let state = sender.p2p_handle.state.lock();
        state
            .publishing_status
            .send_replace(PublishingStatus::Publishing { authorization });
    }
    sender.p2p_handle.publish_new(authorized)?;
    {
        let state = sender.p2p_handle.state.lock();
        state
            .publishing_status
            .send_replace(PublishingStatus::NotPublishing {
                active_publishers: Vec::new(),
            });
    }

    Ok(())
}

async fn setup_nodes(n: u8) -> eyre::Result<NodeTestFixture> {
    setup_nodes_with_flashblocks_args(n, |_, authorizer, builder| {
        test_flashblocks_args(authorizer, builder)
    })
    .await
}

async fn setup_nodes_with_flashblocks_args<F>(
    n: u8,
    mut make_flashblocks_args: F,
) -> eyre::Result<NodeTestFixture>
where
    F: FnMut(u8, &SigningKey, &SigningKey) -> FlashblocksArgs,
{
    let mut nodes = Vec::new();
    let mut peers = Vec::new();
    let exec = TaskExecutor::default();
    let authorizer = SigningKey::from_bytes(&[0; 32]);

    for i in 0..n {
        let builder = SigningKey::from_bytes(&[(i + 1) % n; 32]);
        let flashblocks_args = make_flashblocks_args(i, &authorizer, &builder);
        let node = setup_node_extended_cfg(
            exec.clone(),
            authorizer.clone(),
            builder,
            peers.clone(),
            None,
            None,
            Some(flashblocks_args),
        )
        .await?;
        if !peers.is_empty() {
            wait_for_trusted_peers(&node, peers.len()).await?;
        }
        let enr = node.local_node_record;
        peers.push((enr.id, enr.tcp_addr()));
        nodes.push(node);
    }

    Ok(NodeTestFixture {
        nodes,
        authorizer,
        _exec: exec,
    })
}

#[tokio::test]
async fn test_double_failover() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let fixture = setup_nodes(3).await?;
    let nodes = fixture.nodes();
    let authorizer = fixture.authorizer();

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

    let payload_0 = base_payload(0, test_payload_id(10), 0, latest_block.hash(), AUTH_TS_BASE);
    let authorization_0 = Authorization::new(
        payload_0.payload_id,
        AUTH_TS_BASE,
        authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let msg = payload_0.clone();
    let authorized_0 =
        AuthorizedPayload::new(nodes[0].p2p_handle.builder_sk()?, authorization_0, msg);
    nodes[0].p2p_handle.start_publishing(authorization_0)?;
    nodes[0].p2p_handle.publish_new(authorized_0).unwrap();
    sleep(Duration::from_millis(100)).await;

    let payload_1 = next_payload(payload_0.payload_id, 1).await;
    let authorization_1 = Authorization::new(
        payload_1.payload_id,
        AUTH_TS_BASE,
        authorizer,
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
    let payload_2 = next_payload(payload_0.payload_id, 2).await;
    let msg = payload_2.clone();
    let authorization_2 = Authorization::new(
        payload_2.payload_id,
        AUTH_TS_BASE,
        authorizer,
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

    drop(fixture);

    Ok(())
}

#[tokio::test]
async fn test_force_race_condition() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let fixture = setup_nodes(3).await?;
    let nodes = fixture.nodes();
    let authorizer = fixture.authorizer();

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
    let expected_pending_number = latest_block.number() + 1;

    // Querying pending block when it does not exists yet
    let pending_block = nodes[0]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?;
    assert_eq!(pending_block.unwrap().number(), latest_block.number());

    let payload_0 = base_payload(0, test_payload_id(20), 0, latest_block.hash(), AUTH_TS_BASE);
    info!("Sending payload 0, index 0");
    let authorization = Authorization::new(
        payload_0.payload_id,
        AUTH_TS_BASE,
        authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let msg = payload_0.clone();
    let authorized = AuthorizedPayload::new(nodes[0].p2p_handle.builder_sk()?, authorization, msg);
    nodes[0].p2p_handle.start_publishing(authorization)?;
    nodes[0].p2p_handle.publish_new(authorized).unwrap();

    // Query pending block after sending the base payload with an empty delta
    wait_for_pending_block(&nodes[0], expected_pending_number, 0).await?;

    info!("Sending payload 0, index 1");
    let payload_1 = next_payload(payload_0.payload_id, 1).await;
    let authorization = Authorization::new(
        payload_1.payload_id,
        AUTH_TS_BASE,
        authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(
        nodes[0].p2p_handle.builder_sk()?,
        authorization,
        payload_1.clone(),
    );
    nodes[0].p2p_handle.publish_new(authorized).unwrap();

    // Query pending block after sending the second payload with two transactions
    wait_for_pending_block(&nodes[0], expected_pending_number, 0).await?;

    // Send a new block, this time from node 1
    let payload_2 = base_payload(1, test_payload_id(21), 0, latest_block.hash(), AUTH_TS_NEXT);
    info!("Sending payload 1, index 0");
    let authorization_1 = Authorization::new(
        payload_2.payload_id,
        AUTH_TS_NEXT,
        authorizer,
        nodes[1].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorization_2 = Authorization::new(
        payload_2.payload_id,
        AUTH_TS_NEXT,
        authorizer,
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

    drop(fixture);

    Ok(())
}

#[tokio::test]
async fn test_receive_peer_latency_scores_are_recorded() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let fixture = setup_nodes_with_flashblocks_args(3, |_, authorizer, builder| {
        let mut args = test_flashblocks_args(authorizer, builder);
        args.fanout.max_receive_peers = 2;
        args.fanout.rotation_interval = 30;
        args.fanout.score_samples = 4;
        args
    })
    .await?;
    let nodes = fixture.nodes();
    let authorizer = fixture.authorizer();

    let (receive_peers, candidate_peers) = wait_for_flashblocks_topology(&nodes[0], 2, 2).await?;
    assert!(
        candidate_peers.is_empty(),
        "expected no spare candidate peer"
    );

    let slow_peer = receive_peers[0];
    let fast_peer = receive_peers[1];

    let peer_map: HashMap<_, _> = nodes
        .iter()
        .skip(1)
        .map(|node| (node.local_node_record.id, node))
        .collect();

    let fast_node = peer_map
        .get(&fast_peer)
        .copied()
        .expect("fast peer should map to a node");
    let slow_node = peer_map
        .get(&slow_peer)
        .copied()
        .expect("slow peer should map to a node");

    for (payload_suffix, authorization_timestamp) in [(41, 41_u64), (42, 42), (43, 43), (44, 44)] {
        publish_flashblock_with_latency(
            fast_node,
            authorizer,
            test_payload_id(payload_suffix),
            authorization_timestamp,
            Duration::from_millis(10),
        )
        .await?;
        sleep(Duration::from_millis(50)).await;

        publish_flashblock_with_latency(
            slow_node,
            authorizer,
            test_payload_id(payload_suffix + 10),
            authorization_timestamp + 10,
            Duration::from_millis(300),
        )
        .await?;
        sleep(Duration::from_millis(50)).await;
    }

    // Give the remote receivers time to apply the final latency samples before
    // we inspect the moving-average scores.
    sleep(Duration::from_millis(250)).await;

    let timeout = Duration::from_secs(15);
    let poll_interval = Duration::from_millis(100);
    let start = Instant::now();

    loop {
        let fast_score = receive_peer_score(&nodes[0], fast_peer)?;
        let slow_score = receive_peer_score(&nodes[0], slow_peer)?;

        if fast_score.is_some() && slow_score.is_some() {
            break;
        }

        if start.elapsed() >= timeout {
            return Err(eyre!(
                "timed out waiting for latency scores to be recorded: fast={fast_peer} fast_score={fast_score:?}, slow={slow_peer} slow_score={slow_score:?}"
            ));
        }

        sleep(poll_interval).await;
    }

    drop(fixture);

    Ok(())
}

#[tokio::test]
async fn test_get_block_by_number_pending() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let fixture = setup_nodes(1).await?;
    let nodes = fixture.nodes();
    let authorizer = fixture.authorizer();

    let provider = nodes[0].provider().await?;

    let latest_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");
    assert_eq!(latest_block.number(), 0);
    let expected_pending_number = latest_block.number() + 1;

    // Querying pending block when it does not exists yet
    let pending_block = provider
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Pending)
        .await?;
    assert_eq!(pending_block.unwrap().number(), latest_block.number());

    let payload_id = test_payload_id(30);
    let base_payload = base_payload(0, payload_id, 0, latest_block.hash(), AUTH_TS_BASE);
    let authorization = Authorization::new(
        base_payload.payload_id,
        AUTH_TS_BASE,
        authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(
        nodes[0].p2p_handle.builder_sk()?,
        authorization,
        base_payload,
    );
    nodes[0].p2p_handle.start_publishing(authorization)?;
    nodes[0].p2p_handle.publish_new(authorized).unwrap();

    nodes[0]
        .wait_for_pending_block(expected_pending_number, 0)
        .await?;

    let next_payload = next_payload(payload_id, 1).await;
    let authorization = Authorization::new(
        next_payload.payload_id,
        AUTH_TS_BASE,
        authorizer,
        nodes[0].p2p_handle.builder_sk()?.verifying_key(),
    );
    let authorized = AuthorizedPayload::new(
        nodes[0].p2p_handle.builder_sk()?,
        authorization,
        next_payload,
    );
    nodes[0].p2p_handle.start_publishing(authorization)?;
    nodes[0].p2p_handle.publish_new(authorized).unwrap();

    nodes[0]
        .wait_for_pending_block(expected_pending_number, 0)
        .await?;

    drop(fixture);

    Ok(())
}

#[tokio::test]
async fn test_peer_reputation() -> eyre::Result<()> {
    let _tracing = init_tracing("warn,flashblocks=trace");

    let fixture = setup_nodes(2).await?;
    let nodes = fixture.nodes();

    let invalid_authorizer = SigningKey::from_bytes(&[99; 32]);
    let latest_block = nodes[0]
        .provider()
        .await?
        .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest)
        .await?
        .expect("latest block expected");

    let payload_0 = base_payload(0, test_payload_id(40), 0, latest_block.hash(), AUTH_TS_BASE);
    info!("Sending bad authorization");
    let authorization = Authorization::new(
        payload_0.payload_id,
        AUTH_TS_BASE,
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
    let bytes = p2p_msg.encode();

    let peers = nodes[1].network_handle.get_all_peers().await?;
    let peer_0 = &peers[0].remote_id;

    let mut reputation_was_negative = false;
    let mut peer_banned = false;
    for _ in 0..100 {
        nodes[0]
            .p2p_handle
            .send_serialized_to_all_peers(bytes.clone());
        sleep(Duration::from_millis(10)).await;
        let rep_0 = nodes[1].network_handle.reputation_by_id(*peer_0).await?;
        if let Some(rep) = rep_0
            && rep < 0
        {
            reputation_was_negative = true;
        }

        if nodes[1].network_handle.get_all_peers().await?.is_empty() {
            peer_banned = true;
            break;
        }
    }

    // Assert that the peer reputation became negative and peer was banned
    assert!(
        reputation_was_negative,
        "Peer reputation should have become negative"
    );
    assert!(peer_banned, "Peer should have been banned");

    drop(fixture);

    Ok(())
}

#[ignore]
#[tokio::test]
async fn test_peer_monitoring() -> eyre::Result<()> {
    use tracing_subscriber::layer::SubscriberExt;

    let log_buffer = SharedLogBuffer::default();
    let subscriber = tracing_subscriber::registry().with(log_buffer.clone());
    tracing::subscriber::set_global_default(subscriber).expect("failed to set global subscriber");

    let authorizer = SigningKey::from_bytes(&[0; 32]);

    // Create a temporary P2P secret key file for node1 to ensure consistent peer ID across restarts
    let mut p2p_key_file = NamedTempFile::new()?;
    p2p_key_file.write_all(b"0101010101010101010101010101010101010101010101010101010101010101")?;
    p2p_key_file.flush()?;
    let p2p_key_path = p2p_key_file.path().to_path_buf();

    let exec1 = TaskExecutor::default();

    // Setup node 1 with its own executor (for isolated task cancellation)
    // Node1 needs static port and P2P key so it can be restarted with same identity
    let builder1 = SigningKey::from_bytes(&[1; 32]);
    let node1 = setup_node_extended_cfg(
        exec1.clone(),
        authorizer.clone(),
        builder1,
        vec![],                     // No peers initially
        None,                       // Use random port (we'll capture it)
        Some(p2p_key_path.clone()), // Use deterministic P2P key
        None,
    )
    .await?;

    let exec2 = TaskExecutor::default();

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
        None,
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

    // Wait for the event listener to process the SessionClosed event
    sleep(Duration::from_millis(500)).await;

    // Check that disconnection was logged by the event listener (immediate detection)
    {
        let logs = log_buffer.logs();
        let disconnect_log_exists = logs.iter().any(|log| {
            log.contains("trusted peer disconnected") && log.contains(&peer1_id.to_string())
        });
        assert!(
            disconnect_log_exists,
            "Should have logged 'trusted peer disconnected' for peer {} from event listener",
            peer1_id
        );
    }

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
        None,
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
    {
        let logs = log_buffer.logs();
        let reconnection_log_exists = logs.iter().any(|log| {
            log.contains("connection to trusted peer established")
                && log.contains(&peer1_id.to_string())
        });
        assert!(
            reconnection_log_exists,
            "Should have logged 'connection to trusted peer established' for peer {} after restart",
            peer1_id
        );
    }

    // Wait for at least one more monitor tick to verify warnings stopped (1s interval + 1s buffer)
    sleep(monitor::PEER_MONITOR_INTERVAL + Duration::from_secs(1)).await;

    // Count the number of warning logs before and after reconnection to ensure they stopped
    {
        let logs = log_buffer.logs();
        let reconnection_log_idx = logs
            .iter()
            .rposition(|log| log.contains("connection to trusted peer established"))
            .expect("Could not find 'connection to trusted peer established' log");

        let (logs_before_reconnect, logs_after_reconnect) = logs.split_at(reconnection_log_idx);

        let warnings_before_reconnect: Vec<&String> = logs_before_reconnect
            .iter()
            .filter(|log| {
                log.contains(&peer1_id.to_string())
                    && log.contains("WARN")
                    && log.contains("trusted peer disconnected")
            })
            .collect();

        assert!(
            warnings_before_reconnect.len() >= 2,
            "Should have had at least 2 warnings before reconnection, found {}",
            warnings_before_reconnect.len()
        );

        let warnings_after_reconnect: Vec<&String> = logs_after_reconnect
            .iter()
            .filter(|log| {
                log.contains(&peer1_id.to_string())
                    && log.contains("WARN")
                    && log.contains("trusted peer disconnected")
            })
            .collect();

        assert!(
            warnings_after_reconnect.is_empty(),
            "Should have no warnings after reconnection, found {}: {:?}",
            warnings_after_reconnect.len(),
            warnings_after_reconnect
        );
    }

    Ok(())
}
