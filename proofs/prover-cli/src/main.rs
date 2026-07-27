//! `prover-cli`: submit ad-hoc test `prover_requestProof` calls against a running
//! `prover-service`, for manually exercising a nitro-worker (or sp1-worker) deployment
//! end to end without wiring up a full defender/challenger.
//!
//! Given an L2 RPC URL and an L1 RPC URL, this tool automatically computes:
//! - the pre-state output root (informational; logged for context, not submitted), at
//!   `l2_block_number - block_interval`,
//! - the claimed post-state output root (`root_claim`), via `optimism_outputAtBlock` at
//!   `l2_block_number`, and
//! - the L1 head pinning the witness data, via the same `resolve_l1_head` helper the
//!   workers use.
//!
//! It then submits a `prover_requestProof` call with backend `Nitro` to the given
//! prover-service URL, and can optionally poll `prover_getProof` until the request
//! resolves.
//!
//! # Example
//!
//! ```bash
//! prover-cli \
//!   --l2-rpc-url http://l2-execution:8545 \
//!   --l1-rpc-url http://l1-execution:8545 \
//!   --prover-service-url http://prover-service:8080 \
//!   --poll
//! ```
//!
//! By default the target block is `finalized_block - safety_margin` (see
//! `--safety-margin`); pass `--l2-block-number` to target a specific block instead (for
//! example, to reproduce a known-bad request from a debugging session).

use std::time::Duration;

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result, bail};
use clap::Parser;
use tracing::{info, warn};
use world_chain_proof_kona_host_utils::online::resolve_l1_head;
use world_chain_proofs::{ConsensusProvider, OptimismConsensusClient};
use world_chain_prover_service::{
    ProofBackend, ProofRequest, ProofRequester, ProofResponse, ProofStatus, RpcProverServiceClient,
};

/// Default `WorldChainProofSystemGame` address used when `--game-address` is omitted. The
/// `prover-service` does not validate that a game contract actually exists at this address,
/// so any value works for ad-hoc testing.
const DEFAULT_GAME_ADDRESS: &str = "0x0000000000000000000000000000000000000001";

#[derive(Debug, Parser)]
#[command(
    name = "prover-cli",
    about = "Submit ad-hoc test proof requests to a prover-service, computing pre/post \
             output roots and L1 head automatically from live L1/L2 RPCs."
)]
struct Cli {
    /// World Chain L2 execution RPC URL. Used to resolve the L1 head (`eth_call` against
    /// the `L1Block` predeploy) and, unless `--rollup-rpc-url` is set, also queried for
    /// `optimism_outputAtBlock` / `optimism_syncStatus`.
    #[arg(long, env = "L2_RPC_URL")]
    l2_rpc_url: String,

    /// Ethereum L1 execution RPC URL.
    #[arg(long, env = "L1_RPC_URL")]
    l1_rpc_url: String,

    /// op-node (rollup) JSON-RPC URL serving `optimism_outputAtBlock` /
    /// `optimism_syncStatus`. Defaults to `--l2-rpc-url` when omitted, for setups where a
    /// single endpoint serves both namespaces.
    #[arg(long, env = "ROLLUP_RPC_URL")]
    rollup_rpc_url: Option<String>,

    /// prover-service JSON-RPC URL to submit the request to.
    #[arg(long, env = "PROVER_SERVICE_URL")]
    prover_service_url: String,

    /// Target (claimed / post-state) L2 block number. Defaults to
    /// `finalized_block - safety_margin` when omitted.
    #[arg(long)]
    l2_block_number: Option<u64>,

    /// Blocks subtracted from the latest finalized L2 block when `--l2-block-number` is
    /// not given, so the target block comfortably trails the chain tip.
    #[arg(long, default_value_t = 10)]
    safety_margin: u64,

    /// Blocks between the pre-state and the claimed (post-state) block. Only used to look
    /// up and log the pre-state output root for context; not part of the submitted
    /// request.
    #[arg(long, default_value_t = 1)]
    block_interval: u64,

    /// `WorldChainProofSystemGame` contract address the request nominally defends.
    #[arg(long, default_value = DEFAULT_GAME_ADDRESS)]
    game_address: Address,

    /// Poll `prover_getProof` until the request succeeds or fails.
    #[arg(long)]
    poll: bool,

    /// Seconds between polls when `--poll` is set.
    #[arg(long, default_value_t = 10)]
    poll_interval_seconds: u64,

    /// Give up polling after this many seconds (only used with `--poll`).
    #[arg(long, default_value_t = 3600)]
    poll_timeout_seconds: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    run(cli).await
}

async fn run(cli: Cli) -> Result<()> {
    let rollup_rpc_url = cli
        .rollup_rpc_url
        .clone()
        .unwrap_or_else(|| cli.l2_rpc_url.clone());
    let consensus = OptimismConsensusClient::new(rollup_rpc_url.clone());

    let l2_block_number = match cli.l2_block_number {
        Some(block) => block,
        None => {
            let finalized = consensus
                .latest_l2_finalized_block()
                .await
                .context("query latest finalized L2 block")?;
            finalized.checked_sub(cli.safety_margin).with_context(|| {
                format!(
                    "finalized block {finalized} is below safety_margin {}",
                    cli.safety_margin
                )
            })?
        }
    };

    if let Some(pre_state_block) = l2_block_number.checked_sub(cli.block_interval) {
        match consensus.output_root_at_block(pre_state_block).await {
            Ok(pre_state_root) => {
                info!(pre_state_block, %pre_state_root, "resolved pre-state output root");
            }
            Err(error) => {
                warn!(
                    pre_state_block,
                    %error,
                    "failed to resolve pre-state output root (informational only, continuing)"
                );
            }
        }
    } else {
        warn!(
            l2_block_number,
            block_interval = cli.block_interval,
            "l2_block_number is below block_interval; skipping pre-state lookup"
        );
    }

    let root_claim: B256 = consensus
        .output_root_at_block(l2_block_number)
        .await
        .with_context(|| format!("query claimed output root at block {l2_block_number}"))?;

    let l1_head = resolve_l1_head(
        &reqwest::Client::new(),
        &cli.l2_rpc_url,
        &cli.l1_rpc_url,
        l2_block_number,
    )
    .await
    .context("resolve L1 head")?;

    info!(
        l2_block_number,
        %root_claim,
        %l1_head,
        game = %cli.game_address,
        "submitting prover_requestProof (backend=Nitro)"
    );

    let request = ProofRequest {
        backend: ProofBackend::Nitro,
        game: cli.game_address,
        root_claim,
        l2_block_number,
        l1_head,
    };

    let client = RpcProverServiceClient::new(&cli.prover_service_url)
        .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;

    let id = client
        .request_proof(request)
        .await
        .context("submit proof request")?;
    println!("submitted proof request {id} for L2 block {l2_block_number}");

    if !cli.poll {
        return Ok(());
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(cli.poll_timeout_seconds);
    loop {
        let status = client.proof_status(id).await.context("poll proof status")?;
        println!("status: {status}");
        match status {
            ProofStatus::Succeeded => {
                match client.get_proof(id).await.context("fetch proof")? {
                    ProofResponse::Succeeded(succeeded) => {
                        println!("proof succeeded: {:#?}", succeeded.proof);
                    }
                    other => println!("unexpected response for succeeded proof: {other:?}"),
                }
                return Ok(());
            }
            ProofStatus::Failed => {
                let response = client.get_proof(id).await;
                bail!("proof request {id} failed: {response:?}");
            }
            ProofStatus::Created | ProofStatus::Running => {
                if tokio::time::Instant::now() >= deadline {
                    bail!(
                        "proof request {id} did not complete within {} seconds",
                        cli.poll_timeout_seconds
                    );
                }
                tokio::time::sleep(Duration::from_secs(cli.poll_interval_seconds)).await;
            }
        }
    }
}
