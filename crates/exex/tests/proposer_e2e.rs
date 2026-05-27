//! End-to-end tests for the OP Proposer.
//!
//! Each test spins up a real [`Anvil`] L1 node, deploys a compiled stub
//! `DisputeGameFactory` (see `tests/fixtures/StubDisputeGameFactory.sol`),
//! and exercises the proposer with a mock proposal source.
//!
//! Coverage:
//! 1. `propose_creates_dispute_game` — happy path: the driver loop calls
//!    `factory.create()` via the wallet-equipped provider's filler stack,
//!    observes the receipt, and persists the proposal in MDBX.
//! 2. `dedup_skips_recent_proposal` — `has_proposed_since` returns the most
//!    recent submission and the driver skips a second proposal.
//! 3. `restart_resumes_from_mdbx` — opening the same MDBX dir after the
//!    service has stopped surfaces the persisted proposal.
//! 4. `provider_builds_against_anvil` — sanity check that the multi-backend,
//!    cached-nonce, gas-fallback provider boots and can `eth_chainId` an
//!    Anvil instance.
//!
//! Requires `anvil` on `$PATH` (provided by `foundryup`). Tests are tagged
//! `ignore` by default so `cargo test -p world-chain-exex` stays fast in
//! check-only environments; run with `cargo test -p world-chain-exex --
//! --ignored` to execute.

use std::{path::PathBuf, sync::Arc, time::Duration};

use alloy_eips::BlockNumHash;
use alloy_network::TransactionBuilder;
use alloy_node_bindings::Anvil;
use alloy_primitives::{Address, B256, U256, hex};
use alloy_provider::Provider;
use alloy_sol_types::sol;
use async_trait::async_trait;
use url::Url;
use world_chain_exex::{
    L1ProviderConfig, ProposerConfig, ProposerService, SignerKind,
    provider::L1Provider,
    service::AdminRpcSettings,
    source::{Proposal, ProposalSource, ProposalSourceError, SyncStatus},
};

// Stub DGF Solidity bindings — compiled offline from
// `tests/fixtures/StubDisputeGameFactory.sol` (solc 0.8.34, --optimize).
sol! {
    #[allow(missing_docs)]
    #[sol(rpc)]
    interface StubDgf {
        function setBond(uint256 b) external;
        function gameCount() external view returns (uint256);
        function version() external pure returns (string memory);
    }
}

const STUB_DGF_DEPLOY_BYTECODE: &str = include_str!("fixtures/stub_dgf_bytecode.hex");

// ────────────────────────────────────────────────────────────────────────────
// Mock proposal source — returns a fixed (root, block) pair on each call.

#[derive(Clone)]
struct MockSource {
    root: B256,
    block_number: u64,
}

#[async_trait]
impl ProposalSource for MockSource {
    async fn proposal_at_block(
        &self,
        block_number: u64,
    ) -> Result<Proposal, ProposalSourceError> {
        Ok(Proposal {
            root: self.root,
            block_number,
            block_hash: B256::repeat_byte(0x55),
            current_l1: BlockNumHash::default(),
        })
    }

    async fn sync_status(&self) -> Result<SyncStatus, ProposalSourceError> {
        Ok(SyncStatus {
            current_l1: BlockNumHash::default(),
            safe_l2: self.block_number,
            finalized_l2: self.block_number,
        })
    }

    async fn close(&self) {}
}

// ────────────────────────────────────────────────────────────────────────────

struct Harness {
    anvil: alloy_node_bindings::AnvilInstance,
    l1: L1Provider,
    dgf_address: Address,
    bond: U256,
    pk_hex: String,
}

impl Harness {
    async fn spawn() -> Self {
        let anvil = Anvil::new().block_time(1u64).try_spawn().expect("spawn anvil");
        let endpoint = anvil.endpoint();
        let pk_hex = hex::encode(anvil.first_key().to_bytes());

        let l1 = L1ProviderConfig {
            http_urls: vec![Url::parse(&endpoint).unwrap()],
            timeout: Duration::from_secs(10),
            max_rate_limit_retries: 3,
            initial_backoff_ms: 100,
            compute_units_per_second: 660,
            signer: SignerKind::PrivateKey(pk_hex.clone()),
        }
        .build()
        .expect("build provider");

        // Deploy the stub DGF.
        let runtime: alloy_primitives::Bytes =
            hex::decode(STUB_DGF_DEPLOY_BYTECODE.trim()).expect("hex").into();
        let req = alloy_rpc_types::TransactionRequest::default()
            .with_deploy_code(runtime)
            .with_from(l1.from);
        let pending = l1.provider.send_transaction(req).await.expect("deploy tx");
        let receipt = pending.get_receipt().await.expect("deploy receipt");
        assert!(receipt.status(), "deploy reverted");
        let dgf_address = receipt.contract_address.expect("deployed");

        // Set the initial bond to 1 wei.
        let bond = U256::from(1_u64);
        let dgf = StubDgf::new(dgf_address, l1.provider.clone());
        let pending = dgf.setBond(bond).send().await.expect("setBond");
        let receipt = pending.get_receipt().await.expect("setBond receipt");
        assert!(receipt.status(), "setBond reverted");

        // Sanity.
        let version = dgf.version().call().await.expect("version");
        assert_eq!(version, "stub-1.0");

        Harness { anvil, l1, dgf_address, bond, pk_hex }
    }

    fn config(&self, datadir: PathBuf) -> ProposerConfig {
        ProposerConfig {
            l1_eth_rpcs: vec![self.anvil.endpoint()],
            rollup_rpcs: vec![],
            game_factory_address: self.dgf_address,
            game_type: 0,
            poll_interval: Duration::from_millis(200),
            proposal_interval: Duration::from_secs(1),
            network_timeout: Duration::from_secs(10),
            active_sequencer_check_duration: Duration::from_secs(60),
            balance_poll_interval: Duration::from_millis(100),
            allow_non_finalized: true,
            wait_node_sync: false,
            private_key: Some(self.pk_hex.clone()),
            mnemonic: None,
            hd_path: "m/44'/60'/0'/0/0".into(),
            rpc_max_retries: 3,
            rpc_initial_backoff_ms: 100,
            rpc_compute_units_per_second: 660,
            rpc_addr: "127.0.0.1".into(),
            rpc_port: 0,
            rpc_enable_admin: false,
            datadir,
        }
    }
}

fn no_admin() -> AdminRpcSettings {
    AdminRpcSettings { enable: false, addr: "127.0.0.1".into(), port: 0 }
}

async fn wait_for<F>(timeout: Duration, mut check: F) -> bool
where
    F: AsyncFnMut() -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

// ────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns an anvil subprocess; run with --ignored"]
async fn provider_builds_against_anvil() {
    let h = Harness::spawn().await;
    let chain_id = h.l1.provider.get_chain_id().await.expect("chain_id");
    assert_eq!(chain_id, 31_337);
    assert!(h.bond > U256::ZERO);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns an anvil subprocess; run with --ignored"]
async fn propose_creates_dispute_game() {
    let h = Harness::spawn().await;
    let datadir = tempfile::tempdir().unwrap();
    let cfg = h.config(datadir.path().to_path_buf());

    let source = Arc::new(MockSource { root: B256::repeat_byte(0xaa), block_number: 100 });
    let mut service =
        ProposerService::from_config_with_source(cfg, source).await.expect("service");

    assert_eq!(service.factory.game_count().await.unwrap(), 0);

    service.start(&no_admin()).await.expect("start");

    let factory = service.factory.clone();
    let landed = wait_for(Duration::from_secs(15), async move || {
        factory.game_count().await.unwrap_or(0) >= 1
    })
    .await;
    assert!(landed, "no game submitted within deadline");

    let stored = service.store.last_proposal().unwrap().expect("stored");
    assert_eq!(stored.root, B256::repeat_byte(0xaa));
    assert_eq!(stored.block_number, 100);

    let _ = service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns an anvil subprocess; run with --ignored"]
async fn dedup_skips_recent_proposal() {
    let h = Harness::spawn().await;
    let datadir = tempfile::tempdir().unwrap();
    let mut cfg = h.config(datadir.path().to_path_buf());
    // Long interval: once we land one, the second cycle must skip.
    cfg.proposal_interval = Duration::from_secs(3600);

    let source = Arc::new(MockSource { root: B256::repeat_byte(0x77), block_number: 42 });
    let mut service =
        ProposerService::from_config_with_source(cfg, source).await.expect("service");
    service.start(&no_admin()).await.expect("start");

    let factory = service.factory.clone();
    let landed = wait_for(Duration::from_secs(15), async move || {
        factory.game_count().await.unwrap_or(0) >= 1
    })
    .await;
    assert!(landed, "first proposal did not land");

    // Give the loop time to attempt a second cycle.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(service.factory.game_count().await.unwrap(), 1, "dedup failed");

    let _ = service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns an anvil subprocess; run with --ignored"]
async fn restart_resumes_from_mdbx() {
    let h = Harness::spawn().await;
    let datadir = tempfile::tempdir().unwrap();
    let stored = {
        let cfg = h.config(datadir.path().to_path_buf());
        let source = Arc::new(MockSource { root: B256::repeat_byte(0xbe), block_number: 7 });
        let mut service =
            ProposerService::from_config_with_source(cfg, source).await.expect("service");
        service.start(&no_admin()).await.expect("start");

        let store = service.store.clone();
        let waited = wait_for(Duration::from_secs(15), async move || {
            store.last_proposal().ok().flatten().is_some()
        })
        .await;
        assert!(waited, "proposal not persisted");
        let stored = service.store.last_proposal().unwrap().expect("stored");
        let _ = service.stop().await;
        stored
    };

    // Re-open the same dir with a fresh store handle.
    let store = world_chain_exex::db::ProposerStore::open(datadir.path()).expect("reopen");
    let after = store.last_proposal().expect("read").expect("present");
    assert_eq!(after, stored);
}
