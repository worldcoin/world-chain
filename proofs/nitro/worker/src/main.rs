//! `nitro-worker` binary: leases Nitro TEE proof jobs from the `prover-service`, proves
//! them inside a running Nitro Enclave, and submits the signed attestations back.
//!
//! # Architecture
//!
//! ```text
//!  ┌──────────────────────────────────────────────────────────────┐
//!  │                     nitro-worker                             │
//!  │                                                              │
//!  │  poll prover_getNextProof(Nitro)  ← generic ProofWorker     │
//!  │       │                                                      │
//!  │       ▼                                                      │
//!  │  build Kona witness over RPC (same path as bin/proof)        │
//!  │       │                                                      │
//!  │       ▼                                                      │
//!  │  NitroProver::prove_range_async  ──────► Nitro Enclave       │
//!  │       │                                  (vsock / PCR-pinned)│
//!  │       ▼                                                      │
//!  │  prover_submitProof(Nitro { attestation, signature })        │
//!  └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! Uses the generic `ProofWorker` from `world-chain-proof-worker` (introduced in the
//! sp1-worker PR) for the lease→prove→submit loop, and the real `world-chain-prover-service`
//! types for the RPC client and proof data types.
//!
//! # Devnet mode
//!
//! Passing `--dev` (or setting `NITRO_WORKER_DEV=1`) switches the worker into a permissive
//! dev mode used by the native World Chain devnet:
//!
//! - PCR0/PCR1/PCR2 default to [`ExpectedPcrs::PLACEHOLDER`] (all zeros) when none are
//!   supplied, which tells the host-side verifier to skip attestation/signature checks.
//! - All other behaviour (witness generation, vsock framing, submission to the
//!   `prover-service`) is identical to production.
//!
//! See `proofs/nitro/worker/README.md` for a full devnet runbook.

#[cfg(target_os = "linux")]
mod inner {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use alloy_primitives::B256;
    use anyhow::{Context, Result, bail};
    use clap::Parser;
    use tracing::{info, warn};
    use world_chain_chainspec::WorldChainSpec;
    use world_chain_nitro_worker::{
        NitroBackend, NitroBackendConfig, ProofWorker, ProofWorkerConfig,
    };
    use world_chain_proof_kona_host_utils::online::build_online_config;
    use world_chain_proof_nitro::ExpectedPcrs;
    use world_chain_proof_protocol::WorldHardforkConfig as ProtocolHardforkConfig;
    use world_chain_prover_service::RpcProverServiceClient;

    // ──────────────────────────────────────────────────────────────────────────────────────
    // CLI / config
    // ──────────────────────────────────────────────────────────────────────────────────────

    #[derive(Debug, Clone, Copy, clap::ValueEnum)]
    enum Network {
        #[value(name = "worldchain")]
        WorldChain,
        #[value(name = "worldchain-sepolia")]
        WorldChainSepolia,
    }

    impl Network {
        fn chain_id(self) -> u64 {
            match self {
                Self::WorldChain => 480,
                Self::WorldChainSepolia => 4801,
            }
        }

        fn chain_spec(self) -> Arc<WorldChainSpec> {
            match self {
                Self::WorldChain => WorldChainSpec::mainnet(),
                Self::WorldChainSepolia => WorldChainSpec::sepolia(),
            }
        }
    }

    #[derive(Debug, Parser)]
    #[command(
        name = "nitro-worker",
        about = "World Chain Nitro TEE proving worker: leases jobs from the prover-service, \
                 proves them in a Nitro Enclave, and submits the signed attestations back."
    )]
    struct Cli {
        /// prover-service JSON-RPC URL.
        #[arg(long, env = "PROVER_SERVICE_URL")]
        prover_service_url: String,

        /// World Chain L2 execution RPC URL.
        #[arg(long, env = "L2_RPC_URL")]
        l2_rpc: String,

        /// Ethereum L1 execution RPC URL.
        #[arg(long, env = "L1_RPC_URL")]
        l1_rpc: String,

        /// Ethereum L1 beacon API URL.
        #[arg(long, env = "L1_BEACON_RPC_URL")]
        l1_beacon_rpc: String,

        /// World Chain network.
        #[arg(long, env = "NETWORK", default_value = "worldchain")]
        network: Network,

        /// Rollup config JSON file. If omitted, uses the built-in network config.
        #[arg(long, env = "ROLLUP_CONFIG")]
        rollup_config: Option<PathBuf>,

        /// Rollup config hash override (required when --rollup-config is not supplied).
        #[arg(long, env = "ROLLUP_CONFIG_HASH")]
        rollup_config_hash: Option<B256>,

        /// L2 blocks between a proposal's parent and its claimed block (the proof system's
        /// `blockInterval` domain constant).
        #[arg(long, env = "BLOCK_INTERVAL")]
        block_interval: u64,

        /// vsock CID of the running Nitro Enclave.
        #[arg(long, env = "ENCLAVE_CID", default_value_t = 16)]
        enclave_cid: u32,

        /// vsock port the enclave listens on.
        #[arg(long, env = "ENCLAVE_PORT", default_value_t = world_chain_proof_nitro::protocol::DEFAULT_VSOCK_PORT)]
        enclave_port: u32,

        /// PCR0 hex (48 bytes). All three PCRs must be provided for production use.
        #[arg(long, env = "PCR0")]
        pcr0: Option<String>,

        /// PCR1 hex (48 bytes).
        #[arg(long, env = "PCR1")]
        pcr1: Option<String>,

        /// PCR2 hex (48 bytes).
        #[arg(long, env = "PCR2")]
        pcr2: Option<String>,

        /// Run in devnet/development mode: when PCRs are not supplied, fall back to
        /// placeholder zeros (which disables attestation/signature verification). This
        /// flag is required for any production-style run that omits PCRs so the worker
        /// never silently runs with a degraded trust model.
        #[arg(long, env = "NITRO_WORKER_DEV")]
        dev: bool,

        /// Seconds to sleep between job-queue polls when no work is available.
        #[arg(long, env = "POLL_INTERVAL_SECONDS", default_value_t = 10)]
        poll_interval_seconds: u64,

        /// Maximum seconds to spend generating one Kona witness.
        #[arg(long, default_value_t = 900)]
        witness_timeout_seconds: u64,

        /// Maximum number of jobs proved concurrently. TEE attestation is cheaper than ZK
        /// proving, so this can be higher than for SP1 workers.
        #[arg(long, default_value_t = 1)]
        max_concurrent_jobs: usize,
    }

    // ──────────────────────────────────────────────────────────────────────────────────────
    // Entry point
    // ──────────────────────────────────────────────────────────────────────────────────────

    pub fn run() -> Result<()> {
        dotenvy::dotenv().ok();
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();

        let cli = Cli::parse();

        let spec = cli.network.chain_spec();
        let protocol_cfg = ProtocolHardforkConfig::from_chain_spec(spec.as_ref());
        let online = build_online_config(
            cli.rollup_config.clone(),
            cli.rollup_config_hash,
            cli.l1_rpc.clone(),
            cli.l1_beacon_rpc.clone(),
            cli.l2_rpc.clone(),
            cli.network.chain_id(),
            &protocol_cfg,
            Duration::from_secs(cli.witness_timeout_seconds),
        )?;
        let expected_pcrs = build_expected_pcrs(
            cli.pcr0.as_deref(),
            cli.pcr1.as_deref(),
            cli.pcr2.as_deref(),
            cli.dev,
        )?;

        info!(
            prover_service = %cli.prover_service_url,
            enclave_cid = cli.enclave_cid,
            block_interval = cli.block_interval,
            dev = cli.dev,
            "nitro-worker starting"
        );

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("nitro-worker")
            .worker_threads(2)
            .max_blocking_threads(4)
            .build()
            .context("failed to build tokio runtime")?;

        let backend_config = NitroBackendConfig {
            block_interval: cli.block_interval,
            online,
            enclave_cid: cli.enclave_cid,
            enclave_port: cli.enclave_port,
            expected_pcrs,
        };

        let backend = NitroBackend::new(backend_config, runtime.handle().clone());

        let queue = RpcProverServiceClient::new(&cli.prover_service_url)
            .with_context(|| format!("failed to connect to {}", cli.prover_service_url))?;

        let worker = ProofWorker::new(
            queue,
            backend,
            ProofWorkerConfig {
                poll_interval: Duration::from_secs(cli.poll_interval_seconds),
                max_concurrent_jobs: cli.max_concurrent_jobs,
            },
        );

        let token = worker.cancellation_token();
        runtime.spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                info!("received ctrl-c, shutting down");
                token.cancel();
            }
        });

        runtime.block_on(worker);
        Ok(())
    }

    // ──────────────────────────────────────────────────────────────────────────────────────
    // Config helpers
    // ──────────────────────────────────────────────────────────────────────────────────────

    fn build_expected_pcrs(
        pcr0: Option<&str>,
        pcr1: Option<&str>,
        pcr2: Option<&str>,
        dev: bool,
    ) -> Result<ExpectedPcrs> {
        match (pcr0, pcr1, pcr2) {
            (Some(p0), Some(p1), Some(p2)) => Ok(ExpectedPcrs {
                pcr0: hex_to_pcr(p0)?,
                pcr1: hex_to_pcr(p1)?,
                pcr2: hex_to_pcr(p2)?,
            }),
            (None, None, None) if dev => {
                warn!(
                    "PCRs not configured and --dev set: using placeholder zeros. \
                     Attestation and signature verification will be SKIPPED. \
                     This mode is for the local devnet ONLY — production requires \
                     --pcr0/--pcr1/--pcr2."
                );
                Ok(ExpectedPcrs::PLACEHOLDER)
            }
            (None, None, None) => bail!(
                "PCRs are required for production runs; pass --pcr0/--pcr1/--pcr2, \
                 or use --dev (NITRO_WORKER_DEV=1) for local devnet with placeholder \
                 PCRs and attestation verification disabled."
            ),
            _ => bail!("provide all three of --pcr0/--pcr1/--pcr2, or none"),
        }
    }

    fn hex_to_pcr(s: &str) -> Result<[u8; world_chain_proof_nitro::PCR_LEN]> {
        let bytes = hex::decode(s.trim_start_matches("0x"))
            .with_context(|| format!("invalid PCR hex: {s}"))?;
        if bytes.len() != world_chain_proof_nitro::PCR_LEN {
            bail!(
                "PCR must be {} bytes, got {}",
                world_chain_proof_nitro::PCR_LEN,
                bytes.len()
            );
        }
        let mut arr = [0u8; world_chain_proof_nitro::PCR_LEN];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }
}

fn main() {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("nitro-worker requires Linux (AF_VSOCK)");
        std::process::exit(1);
    }
    #[cfg(target_os = "linux")]
    if let Err(e) = inner::run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
