use std::collections::BTreeMap;

use crate::{
    component::{ContainerImage, DevnetComponent, DevnetComponentKind, DevnetComponentStatus},
    observability::ObservabilityConfig,
};

/// OP contract artifacts supported by the pinned op-deployer image.
pub const DEFAULT_OP_CONTRACT_ARTIFACTS_LOCATOR: &str = "tag://op-contracts/v7.0.0-rc.4";

const DEFAULT_DEVNET_PRIVATE_KEY: &str =
    "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

/// OP Stack container images used by the HA topology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpStackImages {
    /// OP Node-compatible rollup node image.
    pub op_node: ContainerImage,
    /// OP Deployer image.
    pub op_deployer: ContainerImage,
    /// OP Batcher image.
    pub op_batcher: ContainerImage,
    /// OP Proposer image.
    pub op_proposer: ContainerImage,
    /// OP Challenger image.
    pub op_challenger: ContainerImage,
    /// OP Conductor image.
    pub op_conductor: ContainerImage,
}

impl Default for OpStackImages {
    fn default() -> Self {
        Self {
            op_node: ContainerImage::new(
                "us-docker.pkg.dev/oplabs-tools-artifacts/images/kona-node",
                "v1.6.0",
            ),
            op_deployer: ContainerImage::new(
                "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-deployer",
                "v0.7.0-rc.1",
            ),
            op_batcher: ContainerImage::new(
                "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-batcher",
                "develop",
            ),
            op_proposer: ContainerImage::new(
                "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-proposer",
                "develop",
            ),
            op_challenger: ContainerImage::new(
                "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-challenger",
                "v1.9.3",
            ),
            op_conductor: ContainerImage::new(
                "us-docker.pkg.dev/oplabs-tools-artifacts/images/op-conductor",
                "develop",
            ),
        }
    }
}

/// OP contract deployment configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpContractDeploymentConfig {
    /// L1 artifacts locator passed to op-deployer.
    pub l1_artifacts_locator: String,
    /// L2 artifacts locator passed to op-deployer.
    pub l2_artifacts_locator: String,
}

impl Default for OpContractDeploymentConfig {
    fn default() -> Self {
        Self {
            l1_artifacts_locator: DEFAULT_OP_CONTRACT_ARTIFACTS_LOCATOR.to_string(),
            l2_artifacts_locator: DEFAULT_OP_CONTRACT_ARTIFACTS_LOCATOR.to_string(),
        }
    }
}

/// World Chain contract deployment scope for the native devnet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldContractsDeploymentConfig {
    /// Deploy WIP-1006 proof-system contracts on the local L1.
    pub proof_system: bool,
    /// Deploy fee-vault contracts with mock price oracles.
    ///
    /// This is disabled for the native devnet. The fee vault contracts are not
    /// required for the OP Stack sequencing path and their production
    /// constructor dependencies should not be patched for local startup.
    pub fee_vaults: bool,
    /// Include PBH contracts.
    ///
    /// PBH is intentionally disabled in the native devnet: those contracts are
    /// deprecated and should not be part of the new default topology.
    pub pbh_contracts: bool,
}

impl Default for WorldContractsDeploymentConfig {
    fn default() -> Self {
        Self {
            proof_system: true,
            fee_vaults: false,
            pbh_contracts: false,
        }
    }
}

impl WorldContractsDeploymentConfig {
    /// Contracts intentionally covered by the native devnet deployment scope.
    pub fn planned_contracts(&self) -> Vec<&'static str> {
        let mut contracts = Vec::new();
        if self.proof_system {
            contracts.extend([
                "WorldChainAnchorStateRegistry",
                "WorldChainProofSystemFactory",
                "MockRootIdVerifier(VALIDITY_PROOF)",
                "MockRootIdVerifier(TEE_ATTESTATION)",
                "MockRootIdVerifier(SECURITY_COUNCIL)",
                "MockStakingRegistry",
            ]);
        }
        if self.fee_vaults {
            contracts.extend([
                "MockChainLinkPriceFeed(WLD/USD)",
                "MockChainLinkPriceFeed(ETH/USD)",
                "FeeEscrow",
                "FeeRecipient",
            ]);
        }
        contracts
    }

    /// Deprecated PBH contracts intentionally omitted from the native devnet.
    pub const fn omitted_pbh_contracts() -> &'static [&'static str] {
        &["PBHEntryPoint", "PBHSignatureAggregator", "PBH4337Module"]
    }
}

/// Configuration for the op-conductor-backed HA sequencing topology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HaSequencerConfig {
    /// Number of sequencer replicas.
    pub sequencer_count: u8,
    /// Whether to include op-challenger in the topology.
    ///
    /// Disabled by default because the native devnet does not yet generate the
    /// Cannon prestates required to play local permissioned dispute games.
    pub op_challenger: bool,
    /// Whether to include Prometheus and Grafana.
    pub observability: ObservabilityConfig,
    /// OP Stack image choices.
    pub images: OpStackImages,
    /// OP contract deployment configuration.
    pub op_contracts: OpContractDeploymentConfig,
    /// World Chain contract deployment configuration.
    pub world_contracts: WorldContractsDeploymentConfig,
}

impl Default for HaSequencerConfig {
    fn default() -> Self {
        Self {
            sequencer_count: 3,
            op_challenger: false,
            observability: ObservabilityConfig::enabled(),
            images: OpStackImages::default(),
            op_contracts: OpContractDeploymentConfig::default(),
            world_contracts: WorldContractsDeploymentConfig::default(),
        }
    }
}

impl HaSequencerConfig {
    /// Set the number of HA sequencer replicas.
    pub fn with_sequencer_count(mut self, sequencer_count: u8) -> Self {
        self.sequencer_count = sequencer_count.max(1);
        self
    }

    /// Enable or disable op-challenger in the topology.
    pub fn with_op_challenger(mut self, enabled: bool) -> Self {
        self.op_challenger = enabled;
        self
    }

    /// Enable or disable observability in the topology.
    pub fn with_observability(mut self, observability: ObservabilityConfig) -> Self {
        self.observability = observability;
        self
    }
}

/// Per-sequencer op-conductor runtime configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpConductorConfig {
    /// Stable raft server ID.
    pub server_id: String,
    /// Consensus listen address.
    pub consensus_addr: String,
    /// Consensus address advertised to peer conductors.
    pub consensus_advertised: String,
    /// OP Node RPC endpoint controlled by this conductor.
    pub node_rpc: String,
    /// Execution RPC endpoint controlled by this conductor.
    pub execution_rpc: String,
    /// Persistent raft storage directory.
    pub raft_storage_dir: String,
    /// Whether this conductor bootstraps the raft cluster.
    pub raft_bootstrap: bool,
    /// Whether the conductor starts paused.
    pub paused: bool,
    /// Whether conductor should proxy RPC to the active sequencer services.
    pub rpc_enable_proxy: bool,
    /// Minimum op-node peer count for health.
    pub healthcheck_min_peer_count: u8,
}

impl OpConductorConfig {
    /// Build a local HA conductor config for a sequencer index.
    pub fn local(index: u8, sequencer_count: u8) -> Self {
        let suffix = index + 1;
        Self {
            server_id: format!("sequencer-{index}"),
            consensus_addr: "0.0.0.0".to_string(),
            consensus_advertised: format!("op-conductor-{index}:50050"),
            node_rpc: format!("http://op-node-{index}:8547"),
            execution_rpc: format!("http://world-chain-el-{index}:8545"),
            raft_storage_dir: "/data/op-conductor".to_string(),
            raft_bootstrap: index == 0,
            paused: index != 0,
            rpc_enable_proxy: true,
            healthcheck_min_peer_count: sequencer_count.saturating_sub(1),
        }
        .with_server_suffix(suffix)
    }

    fn with_server_suffix(mut self, suffix: u8) -> Self {
        self.server_id = format!("sequencer-{suffix}");
        self
    }

    /// Environment variables accepted by op-conductor.
    pub fn env(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from_iter([
            ("CONSENSUS_ADDR", self.consensus_addr.clone()),
            ("CONSENSUS_ADVERTISED", self.consensus_advertised.clone()),
            ("RAFT_SERVER_ID", self.server_id.clone()),
            ("RAFT_STORAGE_DIR", self.raft_storage_dir.clone()),
            ("RAFT_BOOTSTRAP", self.raft_bootstrap.to_string()),
            ("NODE_RPC", self.node_rpc.clone()),
            ("EXECUTION_RPC", self.execution_rpc.clone()),
            ("PAUSED", self.paused.to_string()),
            ("RPC_ENABLE_PROXY", self.rpc_enable_proxy.to_string()),
            (
                "HEALTHCHECK_MIN_PEER_COUNT",
                self.healthcheck_min_peer_count.to_string(),
            ),
        ])
    }
}

/// OP Challenger runtime configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpChallengerConfig {
    /// L1 execution RPC URL.
    pub l1_eth_rpc: String,
    /// L1 beacon/blob endpoint.
    pub l1_beacon: String,
    /// Archive L2 execution RPC URL.
    pub l2_eth_rpc: String,
    /// Archive rollup RPC URL.
    pub rollup_rpc: String,
    /// DisputeGameFactory proxy address.
    pub game_factory_address: Option<String>,
    /// Challenger writable data directory.
    pub datadir: String,
    /// Transaction signing private key.
    pub private_key: String,
    /// Cannon rollup config path for custom networks.
    pub cannon_rollup_config: String,
    /// Cannon L2 genesis path for custom networks.
    pub cannon_l2_genesis: String,
}

impl OpChallengerConfig {
    /// Local placeholder config. The game factory address is filled after
    /// op-deployer produces L1 deployment outputs.
    pub fn local() -> Self {
        Self {
            l1_eth_rpc: "http://l1-dev-chain:8545".to_string(),
            l1_beacon: "http://l1-dev-chain:5052".to_string(),
            l2_eth_rpc: "http://world-chain-el-0:8545".to_string(),
            rollup_rpc: "http://op-node-0:8547".to_string(),
            game_factory_address: None,
            datadir: "/data/op-challenger".to_string(),
            private_key: DEFAULT_DEVNET_PRIVATE_KEY.to_string(),
            cannon_rollup_config: "/workspace/rollup.json".to_string(),
            cannon_l2_genesis: "/workspace/genesis-l2.json".to_string(),
        }
    }

    /// Command-line flags for op-challenger.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--trace-type=permissioned,cannon".to_string(),
            format!("--l1-eth-rpc={}", self.l1_eth_rpc),
            format!("--l1-beacon={}", self.l1_beacon),
            format!("--l2-eth-rpc={}", self.l2_eth_rpc),
            format!("--rollup-rpc={}", self.rollup_rpc),
            format!("--datadir={}", self.datadir),
            format!("--private-key={}", self.private_key),
            format!("--cannon-rollup-config={}", self.cannon_rollup_config),
            format!("--cannon-l2-genesis={}", self.cannon_l2_genesis),
        ];

        if let Some(game_factory_address) = &self.game_factory_address {
            args.push(format!("--game-factory-address={game_factory_address}"));
        }

        args
    }
}

/// HA topology manifest inspired by op-devstack's explicit runtime graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HaSequencerTopology {
    /// User-selected config.
    pub config: HaSequencerConfig,
    /// Planned component graph.
    pub components: Vec<DevnetComponent>,
    /// Explicitly removed legacy services.
    pub removed_services: Vec<DevnetComponent>,
    /// op-conductor runtime configs, one per sequencer.
    pub conductor_configs: Vec<OpConductorConfig>,
    /// op-challenger runtime config.
    pub challenger_config: Option<OpChallengerConfig>,
}

impl HaSequencerTopology {
    /// Build a typed HA sequencer topology manifest.
    pub fn from_config(config: HaSequencerConfig) -> Self {
        let mut components = Vec::new();

        components.push(
            DevnetComponent::new(
                "l1-dev-chain",
                DevnetComponentKind::L1DevChain,
                DevnetComponentStatus::Planned,
            )
            .with_note("Anvil-backed local L1 dependency"),
        );
        components.push(
            DevnetComponent::new(
                "op-contract-deployer",
                DevnetComponentKind::OpContractDeployer,
                DevnetComponentStatus::Planned,
            )
            .with_image(config.images.op_deployer.clone())
            .with_note(format!(
                "deploys OP Stack L1/L2 contracts from {}",
                config.op_contracts.l1_artifacts_locator
            )),
        );

        let mut conductor_configs = Vec::with_capacity(config.sequencer_count as usize);
        for index in 0..config.sequencer_count {
            components.push(
                DevnetComponent::new(
                    format!("world-chain-el-{index}"),
                    DevnetComponentKind::WorldChainExecutionNode,
                    DevnetComponentStatus::Planned,
                )
                .with_note("native direct-sequencing World Chain execution/client process"),
            );
            components.push(
                DevnetComponent::new(
                    format!("op-node-{index}"),
                    DevnetComponentKind::OpNode,
                    DevnetComponentStatus::Planned,
                )
                .with_image(config.images.op_node.clone())
                .with_note("rollup derivation and Engine API driver for one execution node"),
            );

            let conductor = OpConductorConfig::local(index, config.sequencer_count);
            components.push(
                DevnetComponent::new(
                    format!("op-conductor-{index}"),
                    DevnetComponentKind::OpConductor,
                    DevnetComponentStatus::Planned,
                )
                .with_image(config.images.op_conductor.clone())
                .with_note(format!(
                    "raft_server_id={}, bootstrap={}, paused={}",
                    conductor.server_id, conductor.raft_bootstrap, conductor.paused
                )),
            );
            conductor_configs.push(conductor);
        }

        components.push(
            DevnetComponent::new(
                "op-batcher",
                DevnetComponentKind::OpBatcher,
                DevnetComponentStatus::Planned,
            )
            .with_image(config.images.op_batcher.clone())
            .with_note("submits L2 batches through the active sequencer/conductor"),
        );
        components.push(
            DevnetComponent::new(
                "op-proposer",
                DevnetComponentKind::OpProposer,
                DevnetComponentStatus::Planned,
            )
            .with_image(config.images.op_proposer.clone())
            .with_note("posts L2 output roots / dispute-game proposals to L1"),
        );

        if config.world_contracts.proof_system {
            components.push(
                DevnetComponent::new(
                    "world-proof-system",
                    DevnetComponentKind::WorldProofSystem,
                    DevnetComponentStatus::Planned,
                )
                .with_note("deploys WIP-1006 proof-system contracts to the local L1"),
            );
            components.push(
                DevnetComponent::new(
                    "world-chain-proposer",
                    DevnetComponentKind::WorldChainProposer,
                    DevnetComponentStatus::Planned,
                )
                .with_note("native proposer posting OP output roots to the WIP-1006 proof system"),
            );
            components.push(
                DevnetComponent::new(
                    "world-chain-challenger",
                    DevnetComponentKind::WorldChainChallenger,
                    DevnetComponentStatus::Planned,
                )
                .with_note("native challenger disputing invalid WIP-1006 proof-system games"),
            );
        }

        let challenger_config = config.op_challenger.then(OpChallengerConfig::local);
        if config.op_challenger {
            components.push(
                DevnetComponent::new(
                    "op-challenger",
                    DevnetComponentKind::OpChallenger,
                    DevnetComponentStatus::Planned,
                )
                .with_image(config.images.op_challenger.clone())
                .with_note("uses op-deployer DisputeGameFactory output and generated rollup/genesis artifacts"),
            );
        }

        components.push(
            DevnetComponent::new(
                "world-contracts-deployer",
                DevnetComponentKind::WorldContractsDeployer,
                if config.world_contracts.proof_system {
                    DevnetComponentStatus::Planned
                } else {
                    DevnetComponentStatus::Deferred
                },
            )
            .with_note(if config.world_contracts.proof_system {
                "deploys the WIP-1006 proof-system suite with Foundry"
            } else {
                "not part of native devnet startup; FeeEscrow and FeeRecipient are intentionally not deployed"
            })
            .with_note("PBH contracts are deprecated and intentionally omitted"),
        );

        if config.observability.enabled {
            components.push(
                DevnetComponent::new(
                    "prometheus",
                    DevnetComponentKind::Prometheus,
                    DevnetComponentStatus::Planned,
                )
                .with_image(config.observability.prometheus_image.clone()),
            );
            components.push(
                DevnetComponent::new(
                    "grafana",
                    DevnetComponentKind::Grafana,
                    DevnetComponentStatus::Planned,
                )
                .with_image(config.observability.grafana_image.clone()),
            );
        }

        let removed_services = vec![
            DevnetComponent::new(
                "rollup-boost",
                DevnetComponentKind::RemovedLegacyService,
                DevnetComponentStatus::Removed,
            )
            .with_note("removed from the new default topology; World Chain sequences directly"),
            DevnetComponent::new(
                "tx-proxy",
                DevnetComponentKind::RemovedLegacyService,
                DevnetComponentStatus::Removed,
            )
            .with_note("removed unless a future test independently requires it"),
            DevnetComponent::new(
                "rundler",
                DevnetComponentKind::RemovedLegacyService,
                DevnetComponentStatus::Removed,
            )
            .with_note("deferred with PBH/4337 local-dev flow; PBH contracts are deprecated"),
        ];

        Self {
            config,
            components,
            removed_services,
            conductor_configs,
            challenger_config,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ha_topology_lists_expected_components() {
        let topology = HaSequencerTopology::from_config(HaSequencerConfig::default());

        let count = |kind| {
            topology
                .components
                .iter()
                .filter(|component| component.kind == kind)
                .count()
        };

        assert_eq!(count(DevnetComponentKind::L1DevChain), 1);
        assert_eq!(count(DevnetComponentKind::OpContractDeployer), 1);
        assert_eq!(count(DevnetComponentKind::WorldChainExecutionNode), 3);
        assert_eq!(count(DevnetComponentKind::OpNode), 3);
        assert_eq!(count(DevnetComponentKind::OpConductor), 3);
        assert_eq!(count(DevnetComponentKind::OpBatcher), 1);
        assert_eq!(count(DevnetComponentKind::OpProposer), 1);
        assert_eq!(count(DevnetComponentKind::OpChallenger), 0);
        assert_eq!(count(DevnetComponentKind::Prometheus), 1);
        assert_eq!(count(DevnetComponentKind::Grafana), 1);
        assert_eq!(count(DevnetComponentKind::WorldContractsDeployer), 1);
        assert_eq!(count(DevnetComponentKind::WorldProofSystem), 1);
        assert_eq!(count(DevnetComponentKind::WorldChainProposer), 1);
        assert_eq!(count(DevnetComponentKind::WorldChainChallenger), 1);
        assert!(topology.components.iter().any(|component| component.kind
            == DevnetComponentKind::WorldContractsDeployer
            && component.status == DevnetComponentStatus::Planned));
        assert_eq!(topology.removed_services.len(), 3);
        assert!(topology.config.world_contracts.proof_system);
        assert!(!topology.config.world_contracts.fee_vaults);
        assert!(!topology.config.world_contracts.pbh_contracts);
    }

    #[test]
    fn default_op_contract_locator_uses_supported_tag_scheme() {
        assert_eq!(
            OpContractDeploymentConfig::default().l1_artifacts_locator,
            "tag://op-contracts/v7.0.0-rc.4"
        );
        assert_eq!(
            OpContractDeploymentConfig::default().l2_artifacts_locator,
            "tag://op-contracts/v7.0.0-rc.4"
        );
    }

    #[test]
    fn conductor_configs_model_a_three_node_raft_cluster() {
        let topology = HaSequencerTopology::from_config(HaSequencerConfig::default());

        assert_eq!(topology.conductor_configs.len(), 3);
        assert!(topology.conductor_configs[0].raft_bootstrap);
        assert!(!topology.conductor_configs[1].raft_bootstrap);
        assert_eq!(
            topology.conductor_configs[2].env()["HEALTHCHECK_MIN_PEER_COUNT"],
            "2"
        );
    }

    #[test]
    fn challenger_args_wait_for_deployed_game_factory() {
        let challenger = OpChallengerConfig::local();
        let args = challenger.args();

        assert!(
            args.iter()
                .any(|arg| arg == "--trace-type=permissioned,cannon")
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("--game-factory-address"))
        );
    }
}
