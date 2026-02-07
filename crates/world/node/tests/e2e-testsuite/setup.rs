use alloy_eips::{eip2718::Encodable2718, eip7685::EMPTY_REQUESTS_HASH};
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy_primitives::{Address, B64, Sealed, address};
use alloy_rpc_types::TransactionRequest;
use alloy_rpc_types_engine::{
    CancunPayloadFields, ExecutionPayloadV1, ExecutionPayloadV2, ExecutionPayloadV3,
    PraguePayloadFields,
};
use ed25519_dalek::SigningKey;
use eyre::eyre::eyre;
use flashblocks_primitives::{flashblocks::Flashblock, p2p::Authorization};
use op_alloy_consensus::{OpTxEnvelope, TxDeposit, encode_holocene_extra_data};
use op_alloy_rpc_types_engine::{
    OpExecutionData, OpExecutionPayload, OpExecutionPayloadSidecar, OpExecutionPayloadV4,
};
use reth::{
    api::TreeConfig,
    args::PayloadBuilderArgs,
    builder::{EngineNodeLauncher, Node, NodeBuilder, NodeConfig, NodeHandle},
    chainspec::EthChainSpec,
    tasks::TaskManager,
};
use reth_e2e_test_utils::{
    NodeHelperType, TmpDB,
    testsuite::{BlockInfo, Environment, NodeClient, NodeState},
};
use reth_node_api::{NodeTypes, NodeTypesWithDBAdapter, PayloadAttributes, PayloadTypes};
use reth_node_core::args::RpcServerArgs;
use reth_optimism_chainspec::{OpChainSpec, OpChainSpecBuilder};
use reth_optimism_forks::OpHardfork;
use reth_optimism_node::{OpEngineTypes, OpPayloadAttributes};
use reth_optimism_payload_builder::payload_id_optimism;
use reth_provider::providers::BlockchainProvider;
use revm_primitives::{B256, Bytes, TxKind, U256};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tracing::{info, span};
use world_chain_node::{
    FlashblocksOpApi, OpApiExtServer, context::FlashblocksComponentsContext, node::WorldChainNode,
};
use world_chain_test::{
    DEV_WORLD_ID, PBH_DEV_ENTRYPOINT,
    node::{test_config_with_peers_and_gossip, tx},
    utils::{account, signer, tree_root},
};

use world_chain_pool::{
    root::LATEST_ROOT_SLOT,
    validator::{MAX_U16, PBH_GAS_LIMIT_SLOT, PBH_NONCE_LIMIT_SLOT},
};
use world_chain_rpc::{EthApiExtServer, SequencerClient, WorldChainEthApiExt};

use crate::spammer::{TxSpammer, TxType};

const GENESIS: &str = include_str!("../res/genesis.json");

// Optimism protocol constants - these addresses are defined by the Optimism specification
const L1_BLOCK_PREDEPLOY: Address = address!("4200000000000000000000000000000000000015");
const SYSTEM_DEPOSITOR: Address = address!("DeaDDEaDDeAdDeAdDEAdDEaddeAddEAdDEAd0001");

fn create_l1_attributes_deposit_tx() -> Bytes {
    const SELECTOR: [u8; 4] = [0x44, 0x0a, 0x5e, 0x20];
    let mut calldata = SELECTOR.to_vec();
    calldata.extend_from_slice(&[0u8; 32]);
    calldata.extend_from_slice(&[0u8; 32]);
    calldata.extend_from_slice(&[0u8; 32]);
    calldata.extend_from_slice(&[0u8; 32]);
    calldata.extend_from_slice(&[0u8; 32]);

    let deposit = TxDeposit {
        source_hash: revm_primitives::B256::ZERO,
        from: SYSTEM_DEPOSITOR,
        to: TxKind::Call(L1_BLOCK_PREDEPLOY),
        mint: 0u128,
        value: U256::ZERO,
        gas_limit: 1_000_000,
        is_system_transaction: true,
        input: calldata.into(),
    };

    let sealed_deposit = Sealed::new_unchecked(deposit, revm_primitives::B256::ZERO);
    let envelope = OpTxEnvelope::Deposit(sealed_deposit);
    let mut buf = Vec::new();
    envelope.encode_2718(&mut buf);
    buf.into()
}

/// L1 attributes deposit transaction - required as the first transaction in Optimism blocks
pub static TX_SET_L1_BLOCK: LazyLock<Bytes> = LazyLock::new(create_l1_attributes_deposit_tx);

pub struct WorldChainTestingNodeContext {
    pub node: WorldChainNodeTestContext,
    pub ext_context: Option<FlashblocksComponentsContext>,
}

type WorldChainNodeTestContext = NodeHelperType<
    WorldChainNode,
    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainNode, TmpDB>>,
>;

pub async fn setup(
    num_nodes: u8,
    attributes_generator: impl Fn(u64) -> <<WorldChainNode as NodeTypes>::Payload as PayloadTypes>::PayloadBuilderAttributes + Send + Sync + Copy + 'static,
    flashblocks_enabled: bool,
) -> eyre::Result<(
    Range<u8>,
    Vec<WorldChainTestingNodeContext>,
    TaskManager,
    Environment<OpEngineTypes>,
    TxSpammer,
)> {
    setup_with_tx_peers(
        num_nodes,
        attributes_generator,
        false,
        false,
        flashblocks_enabled,
    )
    .await
}

/// Setup multiple nodes with optional transaction propagation peer configuration
pub async fn setup_with_tx_peers(
    num_nodes: u8,
    attributes_generator: impl Fn(u64) -> <<WorldChainNode as NodeTypes>::Payload as PayloadTypes>::PayloadBuilderAttributes + Send + Sync + Copy + 'static,
    enable_tx_peers: bool,
    disable_gossip: bool,
    flashblocks_enabled: bool,
) -> eyre::Result<(
    Range<u8>,
    Vec<WorldChainTestingNodeContext>,
    TaskManager,
    Environment<OpEngineTypes>,
    TxSpammer,
)> {
    unsafe {
        std::env::set_var("PRIVATE_KEY", DEV_WORLD_ID.to_string());
    }
    let op_chain_spec: Arc<OpChainSpec> = Arc::new(CHAIN_SPEC.clone());

    let tasks = TaskManager::current();
    let exec = tasks.executor();

    let mut node_config: NodeConfig<OpChainSpec> = NodeConfig::new(op_chain_spec.clone())
        .with_chain(op_chain_spec.clone())
        .with_rpc(
            RpcServerArgs::default()
                .with_unused_ports()
                .with_auth_unused_port()
                .with_http_unused_port()
                .with_http(),
        )
        .with_payload_builder(PayloadBuilderArgs {
            deadline: Duration::from_secs(12),
            max_payload_tasks: 20,
            gas_limit: Some(30_000_000),
            interval: Duration::from_millis(200),
            ..Default::default()
        })
        .with_unused_ports();

    // discv5 ports seem to be clashing
    node_config.network.discovery.disable_discovery = true;
    node_config.network.discovery.addr = [127, 0, 0, 1].into();

    // is 0.0.0.0 by default
    node_config.network.addr = [127, 0, 0, 1].into();

    let mut environment = Environment::default();
    environment.block_timestamp_increment = 12;

    let mut node_contexts = Vec::<WorldChainTestingNodeContext>::with_capacity(num_nodes as usize);

    let mut spammer = TxSpammer {
        rpc: Vec::new(),
        sequence: vec![TxType::Sstore, TxType::Deploy, TxType::DeployAndDestruct],
    };

    for idx in 0..num_nodes {
        let span = span!(tracing::Level::INFO, "test_node", idx);
        let _enter = span.enter();

        // Configure tx_peers if enabled and this is not the first node
        let config = if enable_tx_peers && idx > 0 {
            // Collect peer IDs from all previously created nodes
            let previous_peer_ids: Vec<reth_network_peers::PeerId> = node_contexts
                .iter()
                .map(|n| n.node.network.record().id)
                .collect();

            test_config_with_peers_and_gossip(
                Some(previous_peer_ids),
                disable_gossip,
                flashblocks_enabled,
            )
        } else {
            test_config_with_peers_and_gossip(None, disable_gossip, flashblocks_enabled)
        };

        let node = WorldChainNode::new(config.args.clone().into_config(&op_chain_spec)?);

        let ext_context = node.ext_context();

        let NodeHandle {
            node,
            node_exit_future: _,
        } =
            NodeBuilder::new(node_config.clone())
                .testing_node(exec.clone())
                .with_types_and_provider::<WorldChainNode, BlockchainProvider<
                    NodeTypesWithDBAdapter<WorldChainNode, TmpDB>,
                >>()
                .with_components(node.components_builder())
                .with_add_ons(node.add_ons())
                .extend_rpc_modules(move |ctx| {
                    let provider = ctx.provider().clone();
                    let pool = ctx.pool().clone();
                    let sequencer_client = config.args.rollup.sequencer.map(SequencerClient::new);
                    let eth_api_ext = WorldChainEthApiExt::new(pool, provider, sequencer_client);
                    ctx.modules.replace_configured(eth_api_ext.into_rpc())?;
                    ctx.modules
                        .replace_configured(FlashblocksOpApi.into_rpc())?;
                    Ok(())
                })
                .launch_with_fn(|builder| {
                    let launcher = EngineNodeLauncher::new(
                        builder.task_executor().clone(),
                        builder.config().datadir(),
                        TreeConfig::default(),
                    );
                    builder.launch_with(launcher)
                })
                .await?;

        let mut node = WorldChainNodeTestContext::new(node, attributes_generator).await?;
        let genesis = node.inner.chain_spec().sealed_genesis_header();

        node.update_forkchoice(genesis.hash(), genesis.hash())
            .await?;

        // Connect each node in a chain.
        if let Some(previous_node) = node_contexts.last_mut() {
            previous_node.node.connect(&mut node).await;
        }

        // Connect last node with the first if there are more than two
        if idx + 1 == num_nodes
            && num_nodes > 2
            && let Some(first_node) = node_contexts.first_mut()
        {
            node.connect(&mut first_node.node).await;
        }

        let world_chain_test_node = WorldChainTestingNodeContext { node, ext_context };

        node_contexts.push(world_chain_test_node);
    }

    for n in &node_contexts {
        let node = &n.node;
        let rpc = node
            .rpc_client()
            .ok_or_else(|| eyre!("Failed to create HTTP RPC client for node"))?;

        let auth = node.auth_server_handle();
        let url = node.rpc_url();
        let client = NodeClient::new(rpc, auth, url);

        environment.node_clients.push(client.clone());

        let node_state = NodeState {
            current_block_info: Some(BlockInfo {
                hash: node.inner.chain_spec().sealed_genesis_header().hash(),
                timestamp: node.inner.chain_spec().sealed_genesis_header().timestamp,
                number: node.inner.chain_spec().sealed_genesis_header().number,
            }),
            ..Default::default()
        };
        environment.node_states.push(node_state);
        spammer.rpc.push(client);
    }

    Ok((0..5, node_contexts, tasks, environment, spammer))
}

pub static CHAIN_SPEC: LazyLock<OpChainSpec> = LazyLock::new(|| {
    let spec: Genesis = serde_json::from_str(GENESIS).expect("genesis should parse");
    OpChainSpecBuilder::base_mainnet()
        .genesis(
            spec.extend_accounts(vec![(
                DEV_WORLD_ID,
                GenesisAccount::default().with_storage(Some(BTreeMap::from_iter(vec![(
                    LATEST_ROOT_SLOT.into(),
                    tree_root().into(),
                )]))),
            )])
            .extend_accounts(vec![(
                PBH_DEV_ENTRYPOINT,
                GenesisAccount::default().with_storage(Some(BTreeMap::from_iter(vec![
                    (PBH_GAS_LIMIT_SLOT.into(), U256::from(15000000).into()),
                    (
                        PBH_NONCE_LIMIT_SLOT.into(),
                        (MAX_U16 << U256::from(160)).into(),
                    ),
                ]))),
            )])
            .extend_accounts(vec![(
                account(0),
                GenesisAccount::default().with_balance(U256::from(100_000_000_000_000_000u64)),
            )]),
        )
        .ecotone_activated()
        .build()
});

// ============================================================================
// Test Helpers
// ============================================================================

/// Get the current Unix timestamp in seconds
pub(crate) fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Create an authorization generator closure for flashblocks tests
pub(crate) fn create_authorization_generator(
    block_hash: B256,
    builder_verifying_key: ed25519_dalek::VerifyingKey,
) -> impl Fn(OpPayloadAttributes) -> Authorization + Clone {
    move |attrs: OpPayloadAttributes| {
        let authorizer_sk = SigningKey::from_bytes(&[0; 32]);
        let payload_id = payload_id_optimism(&block_hash, &attrs, 3);
        Authorization::new(
            payload_id,
            attrs.timestamp(),
            &authorizer_sk,
            builder_verifying_key,
        )
    }
}

/// Build OpPayloadAttributes with common defaults
pub(crate) fn build_payload_attributes(
    timestamp: u64,
    eip1559_params: B64,
    transactions: Option<Vec<Bytes>>,
) -> OpPayloadAttributes {
    OpPayloadAttributes {
        payload_attributes: alloy_rpc_types_engine::PayloadAttributes {
            timestamp,
            prev_randao: B256::random(),
            suggested_fee_recipient: Address::random(),
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(B256::ZERO),
        },
        transactions,
        no_tx_pool: Some(false),
        eip_1559_params: Some(eip1559_params),
        gas_limit: Some(30_000_000),
        min_base_fee: Some(0),
    }
}

/// Encode EIP-1559 parameters for Holocene from a chain spec at a given timestamp
pub(crate) fn encode_eip1559_params<C: EthChainSpec>(
    chain_spec: &C,
    timestamp: u64,
) -> eyre::Result<B64> {
    let eip1559 = encode_holocene_extra_data(
        Default::default(),
        chain_spec.base_fee_params_at_timestamp(timestamp),
    )?;
    let arr: [u8; 8] = eip1559[1..=8].try_into()?;
    Ok(B64::from(arr))
}

/// Sign a transaction request and return the raw encoded bytes
pub(crate) async fn sign_transaction(
    tx_request: TransactionRequest,
    wallet: &EthereumWallet,
) -> Bytes {
    let signed = <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx_request, wallet)
        .await
        .unwrap();
    signed.encoded_2718().into()
}

/// Create and sign a test transaction, returning both raw bytes and tx hash
pub(crate) async fn create_test_transaction(signer_index: u32, nonce: u64) -> (Bytes, B256) {
    let tx_request = tx(
        CHAIN_SPEC.chain.id(),
        None,
        nonce,
        Address::default(),
        210_000,
    );
    let wallet = EthereumWallet::from(signer(signer_index));
    let signed = <TransactionRequest as TransactionBuilder<Ethereum>>::build(tx_request, &wallet)
        .await
        .unwrap();
    (signed.encoded_2718().into(), *signed.tx_hash())
}

pub(crate) fn execution_data_from_from_reduced_flashblock(
    flashblock: Flashblock,
    spec: Arc<OpChainSpec>,
) -> OpExecutionData {
    let base = flashblock.base().unwrap();
    let delta = flashblock.diff();

    let mut op_execution_payload = OpExecutionPayload::v3(ExecutionPayloadV3 {
        payload_inner: ExecutionPayloadV2 {
            payload_inner: ExecutionPayloadV1 {
                parent_hash: base.parent_hash,
                fee_recipient: base.fee_recipient,
                state_root: delta.state_root,
                receipts_root: delta.receipts_root,
                logs_bloom: delta.logs_bloom,
                prev_randao: base.prev_randao,
                block_number: base.block_number,
                gas_limit: base.gas_limit,
                gas_used: delta.gas_used,
                block_hash: delta.block_hash,
                transactions: flashblock.diff().transactions.clone(),
                timestamp: base.timestamp,
                extra_data: base.extra_data.clone(),
                base_fee_per_gas: base.base_fee_per_gas,
            },
            withdrawals: delta.withdrawals.clone(),
        },
        blob_gas_used: 0,
        excess_blob_gas: 0,
    });

    if spec.is_fork_active_at_timestamp(OpHardfork::Isthmus, base.timestamp) {
        info!(
            target: "flashblocks",
            "Upgrading execution payload to V4 for Isthmus fork"
        );
        op_execution_payload =
            OpExecutionPayload::V4(OpExecutionPayloadV4::from_v3_with_withdrawals_root(
                op_execution_payload.as_v3().unwrap().clone(),
                delta.withdrawals_root,
            ))
    }

    let sidecar = match op_execution_payload {
        OpExecutionPayload::V3(_) => OpExecutionPayloadSidecar::v3(CancunPayloadFields::new(
            base.parent_beacon_block_root,
            vec![],
        )),
        OpExecutionPayload::V4(_) => OpExecutionPayloadSidecar::v4(
            CancunPayloadFields::new(base.parent_beacon_block_root, vec![]),
            PraguePayloadFields::new(EMPTY_REQUESTS_HASH),
        ),
        _ => unreachable!(),
    };

    OpExecutionData {
        payload: op_execution_payload.clone(),
        sidecar,
    }
}
