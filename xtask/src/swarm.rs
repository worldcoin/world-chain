//! Local playground — an in-process World Chain node swarm.
//!
//! Spawns N nodes connected via P2P, drives block production with [`EngineDriver`],
//! and optionally runs a [`TxSpammer`] for load generation. RPC endpoints are
//! printed so you can interact with the nodes via `cast` or any JSON-RPC client.
//!
//! ```bash
//! cargo run -p xtask -- playground --nodes 2 --spam
//! ```

use std::{sync::Arc, time::Duration};

use alloy_primitives::B256;
use clap::Parser;
use eyre::Result;
use reth_optimism_node::{OpPayloadAttributes, utils::optimism_payload_attributes};
use reth_optimism_payload_builder::OpPayloadAttrs;
use reth_optimism_payload_builder::payload_id_optimism;
use tracing::info;

use world_chain_node::context::WorldChainDefaultContext;
use world_chain_primitives::p2p::Authorization;
use world_chain_test_utils::e2e_harness::{
    actions::EngineDriver,
    setup::{TX_SET_L1_BLOCK, build_payload_attributes, encode_eip1559_params, setup},
};

/// Launch a local World Chain playground.
///
/// Spawns an in-process node swarm with P2P connectivity, automatic block
/// production, and optional transaction load. Useful for local development,
/// debugging, and demo purposes — no Docker required.
#[derive(Parser, Debug)]
pub struct Args {
    /// Number of nodes to spawn.
    #[arg(long, default_value = "2")]
    pub nodes: u8,

    /// Enable the transaction spammer for continuous load.
    #[arg(long)]
    pub spam: bool,

    /// Block production interval in milliseconds.
    #[arg(long, default_value = "2000")]
    pub block_time_ms: u64,

    /// Number of blocks to produce (0 = unlimited, runs until Ctrl-C).
    #[arg(long, default_value = "0")]
    pub num_blocks: usize,

    /// Enable flashblocks mode.
    #[arg(long, default_value = "true")]
    pub flashblocks: bool,
}

pub async fn run(args: Args) -> Result<()> {
    info!(
        nodes = args.nodes,
        spam = args.spam,
        block_time_ms = args.block_time_ms,
        flashblocks = args.flashblocks,
        "Starting World Chain playground"
    );

    // Spawn node swarm
    let (_, nodes, _tasks, mut env, tx_spammer) = setup::<WorldChainDefaultContext>(
        args.nodes,
        optimism_payload_attributes,
        args.flashblocks,
    )
    .await?;

    // Print RPC endpoints
    for (i, node) in nodes.iter().enumerate() {
        let rpc_url = node.node.rpc_url();
        info!("Node {i}: RPC {rpc_url}");
    }

    let block_hash = nodes[0].node.block_hash(0);
    let chain_spec = nodes[0].node.inner.chain_spec().clone();
    let rpc_url = nodes[0].node.rpc_url();

    // Initialize forkchoice on all nodes to genesis
    for node in &nodes {
        node.node.update_forkchoice(block_hash, block_hash).await?;
    }

    // Start spammer if requested
    if args.spam {
        info!("Starting transaction spammer...");
        tx_spammer.spawn(10, rpc_url);
    }

    let block_interval = Duration::from_millis(args.block_time_ms);
    let num_blocks = if args.num_blocks == 0 {
        usize::MAX
    } else {
        args.num_blocks
    };

    // Build authorization generator for flashblocks.
    // If flashblocks mode is enabled, use the builder's verifying key from the
    // flashblocks context. Otherwise fall back to a dummy key derived from the
    // authorizer secret.
    let maybe_builder_vk = if args.flashblocks {
        let builder_context = nodes[0]
            .ext_context
            .clone()
            .expect("flashblocks context required");
        Some(
            builder_context
                .flashblocks_handle
                .builder_sk()
                .expect("builder signing key required")
                .verifying_key(),
        )
    } else {
        None
    };

    let authorization_gen = move |parent_hash: B256, attrs: OpPayloadAttrs| -> Authorization {
        let authorizer_sk = ed25519_dalek::SigningKey::from_bytes(&[0; 32]);
        let payload_id = payload_id_optimism(&parent_hash, &attrs, 3);
        let vk = maybe_builder_vk.unwrap_or_else(|| authorizer_sk.verifying_key());
        Authorization::new(
            payload_id,
            attrs.payload_attributes.timestamp,
            &authorizer_sk,
            vk,
        )
    };

    // Build follower indices (all nodes except the builder at index 0)
    let follower_idxs: Vec<usize> = (1..nodes.len()).collect();

    let blocks_produced = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let blocks_produced_cb = blocks_produced.clone();

    info!(
        "Producing blocks (interval={}ms, count={})...",
        args.block_time_ms,
        if args.num_blocks == 0 {
            "unlimited".to_string()
        } else {
            args.num_blocks.to_string()
        }
    );

    let mut driver = EngineDriver {
        builder_idx: 0,
        follower_idxs,
        initial_parent_hash: Some(block_hash),
        num_blocks,
        block_interval,
        flashblocks: args.flashblocks,
        authorization_gen,
        attributes_gen: Box::new({
            let chain_spec = chain_spec.clone();
            move |_block_number, timestamp| {
                let eip1559 = encode_eip1559_params(chain_spec.as_ref(), timestamp)?;
                Ok(build_payload_attributes(
                    timestamp,
                    eip1559,
                    Some(vec![TX_SET_L1_BLOCK.clone()]),
                ))
            }
        }),
        during_build: None,
        on_block: Some(Box::new({
            move |block_num, payload| {
                let blocks_produced = blocks_produced_cb.clone();
                let tx_count = payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .payload_inner
                    .transactions
                    .len();

                Box::pin(async move {
                    let total =
                        blocks_produced.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    info!(
                        block = block_num,
                        transactions = tx_count,
                        total_blocks = total,
                        "Block produced"
                    );
                    Ok(())
                })
            }
        })),
    };

    // Run the engine driver — for unlimited mode this runs until the process
    // is killed. We race against Ctrl-C.
    tokio::select! {
        result = driver.execute(&mut env) => {
            result?;
            let total = blocks_produced.load(std::sync::atomic::Ordering::SeqCst);
            info!(total_blocks = total, "Block production complete");
        }
        _ = tokio::signal::ctrl_c() => {
            let total = blocks_produced.load(std::sync::atomic::Ordering::SeqCst);
            info!(total_blocks = total, "Shutting down playground...");
        }
    }

    Ok(())
}
