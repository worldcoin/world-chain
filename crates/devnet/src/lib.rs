//! Native World Chain devnet harness.
//!
//! This crate is the Rust replacement path for the Kurtosis package under
//! `pkg/devnet`. The mapping is intentionally not 1:1: the new default preset
//! keeps the useful local-dev configuration, genesis, and flashblocks setup,
//! while dropping the historical rollup-boost and tx-proxy wiring. PBH
//! contracts are not part of the new deployment scope. The World Chain
//! execution node sequences directly, with lifecycle ownership held by
//! [`WorldDevnet`].

mod component;
mod full_stack;
mod hardforks;
mod l1;
mod observability;
mod op_stack;
mod process_logs;

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use alloy_provider::{Provider, ProviderBuilder};
use eyre::eyre::{Result, WrapErr, bail, eyre};
use reth_chainspec::EthChainSpec;
use reth_node_api::PayloadAttributes;
use reth_optimism_node::{OpEngineTypes, utils::optimism_payload_attributes};
use reth_optimism_payload_builder::OpPayloadAttrs;
use reth_provider::BlockIdReader;
use tokio::sync::watch;
use tracing::{error, info};
use world_chain_chainspec::WorldChainSpec;
use world_chain_node::context::WorldChainDefaultContext;
use world_chain_primitives::{p2p::Authorization, payload_id::force_op_payload_id_v3};
use world_chain_rpc::op::OpApiExtClient;
use world_chain_test_utils::{
    DEV_CHAIN_ID,
    e2e_harness::{
        actions::EngineDriver,
        setup::{
            CHAIN_SPEC, TX_SET_L1_BLOCK, WorldChainTestBuilder, WorldChainTestingNodeContext,
            build_payload_attributes, encode_eip1559_params,
        },
        spammer::TxSpammer,
    },
};

use full_stack::FullStackWorldDevnet;

pub use component::{
    ContainerImage, DevnetComponent, DevnetComponentKind, DevnetComponentStatus, DevnetEndpoint,
};
pub use hardforks::{WORLD_CHAIN_DEVNET_HARDFORK_ORDER, WorldChainHardforkConfig};
pub use l1::{L1DevChain, L1DevChainConfig};
pub use observability::{MetricsTarget, ObservabilityConfig, ObservabilityStack};
pub use op_stack::{
    DEFAULT_OP_CONTRACT_ARTIFACTS_LOCATOR, HaSequencerConfig, HaSequencerTopology,
    OpChallengerConfig, OpConductorConfig, OpContractDeploymentConfig, OpStackImages,
    WorldContractsDeploymentConfig,
};

/// Devnet topology presets.
///
/// `DirectSequencer` is the intended new local-dev shape: one L1 dev chain and
/// one World Chain sequencing execution node, with flashblocks enabled and no
/// rollup-boost or tx-proxy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorldDevnetPreset {
    /// New default topology: direct sequencing World Chain node plus L1 dev chain.
    #[default]
    DirectSequencer,
    /// Fastest useful single-node setup. It still uses the same direct
    /// sequencing path but keeps the component count minimal.
    Minimal,
    /// HA sequencing topology with three direct-sequencing World Chain replicas,
    /// op-node/op-conductor wiring, and observability. `op-challenger` is
    /// opt-in until the native devnet generates matching Cannon prestates.
    HaSequencer,
}

/// Host-port policy for services that can support stable ports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DevnetPortMode {
    /// Let the OS/Docker allocate unused ports.
    #[default]
    Dynamic,
    /// Use stable ports where the underlying component supports them.
    Stable,
}

/// Builder for fresh isolated World Chain devnet systems.
#[derive(Clone, Debug)]
pub struct WorldDevnetBuilder {
    preset: WorldDevnetPreset,
    hardforks: WorldChainHardforkConfig,
    flashblocks: bool,
    access_list: bool,
    nodes: u8,
    block_time: Duration,
    port_mode: DevnetPortMode,
    l1: Option<L1DevChainConfig>,
    observability: ObservabilityConfig,
    ha_sequencer: HaSequencerConfig,
}

impl Default for WorldDevnetBuilder {
    fn default() -> Self {
        Self {
            preset: WorldDevnetPreset::DirectSequencer,
            hardforks: WorldChainHardforkConfig::default(),
            flashblocks: true,
            access_list: false,
            nodes: 1,
            block_time: Duration::from_secs(2),
            port_mode: DevnetPortMode::Dynamic,
            l1: Some(L1DevChainConfig::default()),
            observability: ObservabilityConfig::default(),
            ha_sequencer: HaSequencerConfig::default(),
        }
    }
}

impl WorldDevnetBuilder {
    /// Create a builder for the default direct-sequencer preset.
    pub fn new() -> Self {
        Self::default()
    }

    /// Select a topology preset.
    pub fn preset(mut self, preset: WorldDevnetPreset) -> Self {
        self.preset = preset;
        match preset {
            WorldDevnetPreset::DirectSequencer => {
                self.nodes = self.nodes.max(1);
                self.flashblocks = true;
                self.l1.get_or_insert_with(L1DevChainConfig::default);
            }
            WorldDevnetPreset::Minimal => {
                self.nodes = 1;
                self.flashblocks = true;
                self.l1.get_or_insert_with(L1DevChainConfig::default);
            }
            WorldDevnetPreset::HaSequencer => {
                self.nodes = self.nodes.max(self.ha_sequencer.sequencer_count);
                self.flashblocks = true;
                self.l1.get_or_insert_with(L1DevChainConfig::default);
                self.observability = self.ha_sequencer.observability.clone();
            }
        }
        self
    }

    /// Override the hardfork selection used for the L2 chain spec.
    pub fn hardforks(mut self, hardforks: WorldChainHardforkConfig) -> Self {
        self.hardforks = hardforks;
        self
    }

    /// Enable or disable flashblocks. Presets enable it by default.
    pub fn flashblocks(mut self, enabled: bool) -> Self {
        self.flashblocks = enabled;
        self
    }

    /// Enable or disable flashblocks block access lists (BAL). Disabled by
    /// default; only takes effect when flashblocks is enabled.
    pub fn flashblocks_access_list(mut self, enabled: bool) -> Self {
        self.access_list = enabled;
        self
    }

    /// Set the number of in-process L2 nodes.
    pub fn nodes(mut self, nodes: u8) -> Self {
        self.nodes = nodes.max(1);
        self
    }

    /// Set block-production interval.
    pub fn block_time(mut self, block_time: Duration) -> Self {
        self.block_time = block_time;
        self
    }

    /// Select dynamic or stable ports.
    ///
    /// Stable ports currently apply to the testcontainers L1 service. The
    /// in-process reth test harness still owns dynamic L2 RPC/auth/P2P ports.
    pub fn port_mode(mut self, port_mode: DevnetPortMode) -> Self {
        self.port_mode = port_mode;
        if let Some(l1) = &mut self.l1 {
            l1.stable_port = (port_mode == DevnetPortMode::Stable).then_some(8545);
        }
        self.observability = self.observability.with_port_mode(port_mode);
        self.ha_sequencer.observability = self.ha_sequencer.observability.with_port_mode(port_mode);
        self
    }

    /// Enable or disable the Prometheus/Grafana devnet stack.
    pub fn observability(mut self, mut observability: ObservabilityConfig) -> Self {
        observability = observability.with_port_mode(self.port_mode);
        self.observability = observability.clone();
        self.ha_sequencer.observability = observability;
        self
    }

    /// Override HA sequencer topology configuration.
    pub fn ha_sequencer(mut self, mut ha_sequencer: HaSequencerConfig) -> Self {
        ha_sequencer.observability = ha_sequencer.observability.with_port_mode(self.port_mode);
        if self.preset == WorldDevnetPreset::HaSequencer {
            self.nodes = self.nodes.max(ha_sequencer.sequencer_count);
            self.observability = ha_sequencer.observability.clone();
        }
        self.ha_sequencer = ha_sequencer;
        self
    }

    /// Return the typed HA topology manifest for this builder, if selected.
    pub fn ha_topology(&self) -> Option<HaSequencerTopology> {
        (self.preset == WorldDevnetPreset::HaSequencer)
            .then(|| HaSequencerTopology::from_config(self.ha_sequencer.clone()))
    }

    /// Override L1 configuration.
    pub fn l1(mut self, mut l1: L1DevChainConfig) -> Self {
        if self.port_mode == DevnetPortMode::Stable && l1.stable_port.is_none() {
            l1.stable_port = Some(8545);
        }
        self.l1 = Some(l1);
        self
    }

    /// Disable the containerized L1 dependency.
    ///
    /// This is useful for narrow unit-style harness checks. The direct sequencer
    /// preset keeps L1 enabled by default.
    pub fn without_l1(mut self) -> Self {
        self.l1 = None;
        self
    }

    /// Start a fresh devnet.
    pub async fn build(self) -> Result<WorldDevnet> {
        self.hardforks.validate()?;

        let active_hardforks = self
            .hardforks
            .active()
            .map(|fork| fork.name())
            .collect::<Vec<_>>()
            .join(",");

        info!(
            preset = ?self.preset,
            nodes = self.nodes,
            flashblocks = self.flashblocks,
            access_list = self.access_list,
            block_time_ms = self.block_time.as_millis(),
            hardforks = %active_hardforks,
            l1_enabled = self.l1.is_some(),
            port_mode = ?self.port_mode,
            observability = self.observability.enabled,
            "building native World Chain devnet"
        );

        let ha_topology = self.ha_topology();
        if let Some(topology) = &ha_topology {
            info!(
                sequencers = topology.config.sequencer_count,
                op_challenger = topology.config.op_challenger,
                observability = topology.config.observability.enabled,
                "HA sequencer topology selected"
            );
            info!("starting full OP Stack HA sequencer runtime");
            let full_stack = FullStackWorldDevnet::start(
                topology.config.clone(),
                self.hardforks.clone(),
                self.port_mode,
                self.block_time,
                self.access_list,
            )
            .await?;
            let components = full_stack.components();
            let chain_spec = Arc::new(self.hardforks.apply_to((*CHAIN_SPEC).clone()));

            return Ok(WorldDevnet {
                preset: self.preset,
                hardforks: self.hardforks,
                l1: None,
                observability: None,
                full_stack: Some(full_stack),
                components,
                ha_topology,
                nodes: Vec::new(),
                _tasks: None,
                env: None,
                _tx_spammer: None,
                chain_spec,
                block_time: self.block_time,
                flashblocks: self.flashblocks,
                produced_blocks: Arc::new(AtomicU64::new(0)),
            });
        }

        let l1 = match &self.l1 {
            Some(config) => Some(L1DevChain::start(config.clone()).await?),
            None => None,
        };

        let chain_spec = Arc::new(self.hardforks.apply_to((*CHAIN_SPEC).clone()));
        info!(
            chain_id = chain_spec.chain().id(),
            "starting in-process World Chain node swarm"
        );

        let (_, nodes, tasks, env, tx_spammer) = WorldChainTestBuilder::builder()
            .nodes(self.nodes)
            .flashblocks(self.flashblocks)
            .access_list(self.access_list)
            .chain_spec(chain_spec.clone())
            .build()
            .setup_with::<WorldChainDefaultContext, _>(optimism_payload_attributes)
            .await?;

        for (idx, node) in nodes.iter().enumerate() {
            info!(
                node = idx,
                rpc_url = %node.node.rpc_url(),
                flashblocks_context = node.ext_context.is_some(),
                "World Chain node ready"
            );
        }

        let observability = ObservabilityStack::start(self.observability.clone(), Vec::new())
            .await
            .wrap_err("failed to start Prometheus/Grafana observability stack")?;

        let components = build_component_manifest(
            self.preset,
            &l1,
            &nodes,
            observability.as_ref(),
            ha_topology.as_ref(),
        );

        Ok(WorldDevnet {
            preset: self.preset,
            hardforks: self.hardforks,
            l1,
            observability,
            full_stack: None,
            components,
            ha_topology,
            nodes,
            _tasks: Some(tasks),
            env: Some(env),
            _tx_spammer: Some(tx_spammer),
            chain_spec,
            block_time: self.block_time,
            flashblocks: self.flashblocks,
            produced_blocks: Arc::new(AtomicU64::new(0)),
        })
    }
}

/// Lifecycle-owned running World Chain devnet.
pub struct WorldDevnet {
    preset: WorldDevnetPreset,
    hardforks: WorldChainHardforkConfig,
    l1: Option<L1DevChain>,
    observability: Option<ObservabilityStack>,
    full_stack: Option<FullStackWorldDevnet>,
    components: Vec<DevnetComponent>,
    ha_topology: Option<HaSequencerTopology>,
    nodes: Vec<WorldChainTestingNodeContext<WorldChainDefaultContext>>,
    _tasks: Option<reth_tasks::TaskExecutor>,
    env: Option<reth_e2e_test_utils::testsuite::Environment<OpEngineTypes>>,
    _tx_spammer: Option<TxSpammer<OpEngineTypes>>,
    chain_spec: Arc<WorldChainSpec>,
    block_time: Duration,
    flashblocks: bool,
    produced_blocks: Arc<AtomicU64>,
}

impl WorldDevnet {
    /// Preset used to build this devnet.
    pub const fn preset(&self) -> WorldDevnetPreset {
        self.preset
    }

    /// Hardfork selection used to build this devnet.
    pub const fn hardforks(&self) -> &WorldChainHardforkConfig {
        &self.hardforks
    }

    /// L2 chain spec used by the in-process World Chain node.
    pub fn chain_spec(&self) -> Arc<WorldChainSpec> {
        self.chain_spec.clone()
    }

    /// L1 RPC URL, when the preset started a containerized L1.
    pub fn l1_rpc_url(&self) -> Option<&str> {
        self.full_stack
            .as_ref()
            .map(FullStackWorldDevnet::l1_rpc_url)
            .or_else(|| self.l1.as_ref().map(L1DevChain::rpc_url))
    }

    /// L1 OptimismPortal proxy address, when the preset started a full OP Stack.
    pub fn optimism_portal(&self) -> Option<&str> {
        self.full_stack
            .as_ref()
            .map(FullStackWorldDevnet::optimism_portal)
    }

    /// Stock dispute-game factory in which WIP-1006 is registered, when deployed.
    pub fn proof_system_factory(&self) -> Option<&str> {
        self.full_stack
            .as_ref()
            .and_then(FullStackWorldDevnet::proof_system_factory)
    }

    /// Stock anchor-state registry configured to respect WIP-1006, when deployed.
    pub fn anchor_state_registry(&self) -> Option<&str> {
        self.full_stack
            .as_ref()
            .and_then(FullStackWorldDevnet::anchor_state_registry)
    }

    /// Prometheus UI URL, when observability is enabled.
    pub fn prometheus_url(&self) -> Option<&str> {
        self.full_stack
            .as_ref()
            .and_then(FullStackWorldDevnet::prometheus_url)
            .or_else(|| {
                self.observability
                    .as_ref()
                    .map(ObservabilityStack::prometheus_url)
            })
    }

    /// Grafana UI URL, when observability is enabled.
    pub fn grafana_url(&self) -> Option<&str> {
        self.full_stack
            .as_ref()
            .and_then(FullStackWorldDevnet::grafana_url)
            .or_else(|| {
                self.observability
                    .as_ref()
                    .map(ObservabilityStack::grafana_url)
            })
    }

    /// Typed component manifest for this devnet run.
    pub fn components(&self) -> &[DevnetComponent] {
        &self.components
    }

    /// HA topology manifest, when the HA preset was selected.
    pub const fn ha_topology(&self) -> Option<&HaSequencerTopology> {
        self.ha_topology.as_ref()
    }

    /// Canonical L2 RPC URL.
    pub fn l2_rpc_url(&self) -> String {
        self.sequencer_rpc_url()
    }

    /// Direct sequencer RPC URL.
    pub fn sequencer_rpc_url(&self) -> String {
        if let Some(full_stack) = &self.full_stack {
            full_stack.sequencer_rpc_url().to_string()
        } else {
            self.nodes[0].node.rpc_url().to_string()
        }
    }

    /// Flashblocks endpoint URL.
    ///
    /// The native in-process harness serves the flashblocks capability through
    /// the same HTTP RPC endpoint (`op_supportedCapabilities` and Engine API
    /// extensions), rather than the old Kurtosis sidecar-style WS port.
    pub fn flashblocks_url(&self) -> Option<String> {
        if let Some(full_stack) = &self.full_stack {
            self.flashblocks
                .then(|| full_stack.flashblocks_url().to_string())
        } else {
            self.flashblocks.then(|| self.sequencer_rpc_url())
        }
    }

    /// Number of blocks produced by this [`WorldDevnet`] through its driver.
    pub fn produced_blocks(&self) -> u64 {
        self.produced_blocks.load(Ordering::SeqCst)
    }

    /// Query `eth_chainId` from the sequencer RPC.
    pub async fn chain_id(&self) -> Result<u64> {
        let provider =
            ProviderBuilder::new().connect_http(url::Url::parse(&self.sequencer_rpc_url())?);
        Ok(provider.get_chain_id().await?)
    }

    /// Query the latest L2 block number from the sequencer RPC.
    pub async fn block_number(&self) -> Result<u64> {
        let provider =
            ProviderBuilder::new().connect_http(url::Url::parse(&self.sequencer_rpc_url())?);
        Ok(provider.get_block_number().await?)
    }

    /// Query the current safe L2 block number from the sequencer.
    pub async fn safe_block_number(&self) -> Result<u64> {
        if let Some(full_stack) = &self.full_stack {
            return full_stack.safe_block_number().await;
        }

        self.nodes[0]
            .node
            .inner
            .provider()
            .safe_block_number()
            .wrap_err("failed to query sequencer safe head")?
            .ok_or_else(|| eyre!("sequencer safe head is unavailable"))
    }

    /// Query `op_supportedCapabilities`.
    pub async fn supported_capabilities(&self) -> Result<Vec<String>> {
        if self.full_stack.is_some() {
            return json_rpc_vec(&self.sequencer_rpc_url(), "op_supportedCapabilities").await;
        }

        let client = self.nodes[0]
            .node
            .rpc_client()
            .ok_or_else(|| eyre!("expected sequencer HTTP RPC client"))?;
        Ok(OpApiExtClient::supported_capabilities(&client).await?)
    }

    /// Produce `count` L2 blocks through the local consensus driver.
    pub async fn produce_blocks(&mut self, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }

        if self.full_stack.is_some() {
            let start = self.block_number().await?;
            let target = start + count as u64;
            info!(
                start,
                target,
                sequencer_rpc_url = %self.sequencer_rpc_url(),
                "waiting for full-stack op-node to produce L2 blocks"
            );
            loop {
                let current = self.block_number().await?;
                if current >= target {
                    self.produced_blocks
                        .fetch_add(current.saturating_sub(start), Ordering::SeqCst);
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }

        info!(
            count,
            sequencer_rpc_url = %self.sequencer_rpc_url(),
            flashblocks = self.flashblocks,
            "producing L2 blocks through Engine API"
        );

        let mut driver = self.engine_driver(count, None);
        let env = self
            .env
            .as_mut()
            .ok_or_else(|| eyre!("in-process devnet environment missing"))?;
        driver.execute(env).await.wrap_err_with(|| {
            format!(
                "failed to produce {count} L2 block(s) through Engine API; sequencer_rpc_url={}",
                self.sequencer_rpc_url()
            )
        })
    }

    /// Keep producing blocks until shutdown is requested.
    pub async fn run_until_shutdown(mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        self.print_endpoints();
        self.log_rpc_readiness().await?;

        if let Some(full_stack) = &self.full_stack {
            full_stack.wait_ready().await?;
            info!("full OP Stack devnet is running; press Ctrl-C to stop");
            wait_for_shutdown(&mut shutdown).await;
            info!("World Chain full-stack devnet shutting down");
            return Ok(());
        }

        info!(
            block_time_ms = self.block_time.as_millis(),
            "starting continuous L2 block production; press Ctrl-C to stop"
        );

        let sequencer_rpc_url = self.sequencer_rpc_url();
        let mut env = self
            .env
            .take()
            .ok_or_else(|| eyre!("in-process devnet environment missing"))?;
        let mut driver = self.engine_driver(usize::MAX, None);
        tokio::select! {
            result = driver.execute(&mut env) => {
                if let Err(err) = result.wrap_err_with(|| {
                    format!(
                        "continuous L2 block production failed; sequencer_rpc_url={sequencer_rpc_url}"
                    )
                }) {
                    error!(%err, "continuous L2 block production failed");
                    return Err(err);
                }
            }
            _ = wait_for_shutdown(&mut shutdown) => {}
        }

        info!(
            produced_blocks = self.produced_blocks(),
            "World Chain devnet shutting down"
        );
        Ok(())
    }

    /// Print endpoint URLs for manual use.
    pub fn print_endpoints(&self) {
        info!(preset = ?self.preset, chain_id = DEV_CHAIN_ID, "World Chain devnet started");
        println!("{}", self.endpoint_summary());
        if let Some(url) = self.l1_rpc_url() {
            info!(url, "L1 RPC");
        }
        if let Some(address) = self.optimism_portal() {
            info!(address, "OptimismPortal proxy");
        }
        info!(url = %self.sequencer_rpc_url(), "L2 sequencer RPC");
        if let Some(url) = self.flashblocks_url() {
            info!(url, "Flashblocks RPC capability endpoint");
        }
        if let Some(url) = self.prometheus_url() {
            info!(url, "Prometheus");
        }
        if let Some(url) = self.grafana_url() {
            info!(url, "Grafana");
        }
        for component in &self.components {
            info!(
                id = %component.id,
                kind = component.kind.as_str(),
                status = component.status.as_str(),
                image = component.image.as_ref().map(ContainerImage::reference),
                endpoints = ?component.endpoints,
                notes = ?component.notes,
                "devnet component"
            );
        }
    }

    /// Human-readable endpoint summary for manual devnet runs.
    pub fn endpoint_summary(&self) -> String {
        let mut summary = format!(
            "\nWorld Chain devnet endpoints\n\
             preset: {:?}\n\
             chain id: {DEV_CHAIN_ID}\n\n\
             primary endpoints:\n",
            self.preset
        );

        if let Some(url) = self.l1_rpc_url() {
            summary.push_str(&format!("  L1 RPC:              {url}\n"));
        }
        if let Some(address) = self.optimism_portal() {
            summary.push_str(&format!("  OptimismPortal:      {address}\n"));
        }
        summary.push_str(&format!(
            "  Sequencer RPC:       {}\n",
            self.sequencer_rpc_url()
        ));
        if let Some(url) = self.flashblocks_url() {
            summary.push_str(&format!("  Flashblocks:         {url}\n"));
        }
        if let Some(url) = self.prometheus_url() {
            summary.push_str(&format!("  Prometheus:          {url}\n"));
        }
        if let Some(url) = self.grafana_url() {
            summary.push_str(&format!("  Grafana:             {url}\n"));
        }

        let mut endpoint_rows = Vec::new();
        for component in &self.components {
            for endpoint in &component.endpoints {
                endpoint_rows.push(format!(
                    "  {:<24} {:<10} {}",
                    component.id, endpoint.name, endpoint.url
                ));
            }
        }

        if !endpoint_rows.is_empty() {
            summary.push_str("\ncomponent endpoints:\n");
            for row in endpoint_rows {
                summary.push_str(&row);
                summary.push('\n');
            }
        }

        summary
    }

    async fn log_rpc_readiness(&self) -> Result<()> {
        info!(
            sequencer_rpc_url = %self.sequencer_rpc_url(),
            "checking L2 RPC readiness"
        );

        let chain_id = self.chain_id().await.wrap_err_with(|| {
            format!(
                "failed to call eth_chainId on sequencer RPC {}",
                self.sequencer_rpc_url()
            )
        })?;
        let block_number = self.block_number().await.wrap_err_with(|| {
            format!(
                "failed to call eth_blockNumber on sequencer RPC {}",
                self.sequencer_rpc_url()
            )
        })?;

        info!(chain_id, block_number, "L2 RPC ready");

        if self.flashblocks {
            let capabilities = self.supported_capabilities().await.wrap_err_with(|| {
                format!(
                    "failed to call op_supportedCapabilities on sequencer RPC {}",
                    self.sequencer_rpc_url()
                )
            })?;
            info!(?capabilities, "Flashblocks RPC capability check complete");
        }

        Ok(())
    }

    fn engine_driver(
        &self,
        count: usize,
        on_block: Option<world_chain_test_utils::e2e_harness::actions::BlockCallback>,
    ) -> EngineDriver<
        impl Fn(alloy_primitives::B256, OpPayloadAttrs) -> Authorization + Clone + Send + Sync + 'static,
    > {
        let maybe_builder_vk = if self.flashblocks {
            Some(
                self.nodes[0]
                    .ext_context
                    .clone()
                    .expect("flashblocks context required")
                    .flashblocks_handle
                    .builder_sk()
                    .expect("builder signing key required")
                    .verifying_key(),
            )
        } else {
            None
        };

        let authorization_gen = move |parent_hash, attrs: OpPayloadAttrs| -> Authorization {
            let authorizer_sk = ed25519_dalek::SigningKey::from_bytes(&[0; 32]);
            let payload_id = force_op_payload_id_v3(attrs.payload_id(&parent_hash));
            let vk = maybe_builder_vk.unwrap_or_else(|| authorizer_sk.verifying_key());
            Authorization::new(
                payload_id,
                attrs.payload_attributes.timestamp,
                &authorizer_sk,
                vk,
            )
        };

        let chain_spec = self.chain_spec.clone();
        let produced_blocks = self.produced_blocks.clone();
        let user_on_block = on_block;

        EngineDriver {
            builder_idx: 0,
            follower_idxs: (1..self.nodes.len()).collect(),
            initial_parent_hash: None,
            num_blocks: count,
            block_interval: self.block_time,
            flashblocks: self.flashblocks,
            authorization_gen,
            attributes_gen: Box::new(move |_block_number, timestamp| {
                let eip1559 = encode_eip1559_params(chain_spec.as_ref(), timestamp)?;
                Ok(build_payload_attributes(
                    timestamp,
                    eip1559,
                    Some(vec![TX_SET_L1_BLOCK.clone()]),
                ))
            }),
            during_build: None,
            on_block: Some(Box::new(move |block_num, payload| {
                let total_blocks = produced_blocks.fetch_add(1, Ordering::SeqCst) + 1;
                let tx_count = payload
                    .execution_payload
                    .payload_inner
                    .payload_inner
                    .payload_inner
                    .transactions
                    .len();

                info!(
                    block = block_num + 1,
                    total_blocks, tx_count, "L2 block produced"
                );

                if let Some(ref on_block) = user_on_block {
                    on_block(block_num, payload)
                } else {
                    Box::pin(async { Ok(()) })
                }
            })),
        }
    }
}

fn build_component_manifest(
    preset: WorldDevnetPreset,
    l1: &Option<L1DevChain>,
    nodes: &[WorldChainTestingNodeContext<WorldChainDefaultContext>],
    observability: Option<&ObservabilityStack>,
    ha_topology: Option<&HaSequencerTopology>,
) -> Vec<DevnetComponent> {
    let mut components = Vec::new();

    if let Some(l1) = l1 {
        components.push(
            DevnetComponent::new(
                "l1-dev-chain",
                DevnetComponentKind::L1DevChain,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("rpc", l1.rpc_url()),
        );
    }

    for (idx, node) in nodes.iter().enumerate() {
        components.push(
            DevnetComponent::new(
                format!("world-chain-el-{idx}"),
                DevnetComponentKind::WorldChainExecutionNode,
                DevnetComponentStatus::Running,
            )
            .with_endpoint("rpc", node.node.rpc_url().to_string())
            .with_note("in-process World Chain node driven through the Rust Engine API harness"),
        );

        if node.ext_context.is_some() {
            components.push(
                DevnetComponent::new(
                    format!("flashblocks-{idx}"),
                    DevnetComponentKind::Flashblocks,
                    DevnetComponentStatus::Running,
                )
                .with_endpoint("rpc", node.node.rpc_url().to_string())
                .with_note("flashblocks enabled on the World Chain execution node"),
            );
        }
    }

    if let Some(observability) = observability {
        components.extend(observability.components());
    }

    if let Some(topology) = ha_topology {
        let mut planned = topology.components.clone();
        for component in &mut planned {
            match component.kind {
                DevnetComponentKind::L1DevChain
                | DevnetComponentKind::WorldChainExecutionNode
                | DevnetComponentKind::Prometheus
                | DevnetComponentKind::Grafana
                    if components.iter().any(|running| running.id == component.id) =>
                {
                    continue;
                }
                _ => {}
            }

            component.notes.push(
                "HA component is part of the target topology; runtime startup is gated on OP deployer artifact generation"
                    .to_string(),
            );
            components.push(component.clone());
        }
        components.extend(topology.removed_services.clone());
    } else if matches!(
        preset,
        WorldDevnetPreset::DirectSequencer | WorldDevnetPreset::Minimal
    ) {
        components.push(
            DevnetComponent::new(
                "rollup-boost",
                DevnetComponentKind::RemovedLegacyService,
                DevnetComponentStatus::Removed,
            )
            .with_note("not present in direct-sequencer topology"),
        );
        components.push(
            DevnetComponent::new(
                "tx-proxy",
                DevnetComponentKind::RemovedLegacyService,
                DevnetComponentStatus::Removed,
            )
            .with_note("not present unless a future test independently needs it"),
        );
    }

    components
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }

        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

/// Returns true if an error looks like local Docker is unavailable.
pub fn is_docker_unavailable(err: &eyre::Report) -> bool {
    let message = format!("{err:?}");
    message.contains("Cannot connect to the Docker daemon")
        || message.contains("docker daemon")
        || message.contains("No such file or directory")
        || message.contains("Docker host")
}

/// Validate default local devnet chain ID invariants.
pub fn ensure_dev_chain_id(chain_spec: &WorldChainSpec) -> Result<()> {
    if chain_spec.chain().id() != DEV_CHAIN_ID {
        bail!(
            "World devnet chain id mismatch: expected {}, got {}",
            DEV_CHAIN_ID,
            chain_spec.chain().id()
        );
    }
    Ok(())
}

async fn json_rpc_vec(rpc_url: &str, method: &str) -> Result<Vec<String>> {
    let response: serde_json::Value = reqwest::Client::new()
        .post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": [],
        }))
        .send()
        .await
        .wrap_err_with(|| format!("failed to send {method} to {rpc_url}"))?
        .json()
        .await
        .wrap_err_with(|| format!("invalid JSON-RPC response for {method} from {rpc_url}"))?;

    if let Some(error) = response.get("error") {
        bail!("JSON-RPC {method} failed at {rpc_url}: {error}");
    }

    let values = response
        .get("result")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| eyre!("JSON-RPC {method} response missing string array result"))?;

    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| eyre!("JSON-RPC {method} returned non-string capability: {value}"))
        })
        .collect()
}
