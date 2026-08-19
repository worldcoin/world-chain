// Module defining World Chain Node Preset contexts for components & add-ons.

use std::{collections::HashSet, sync::Arc, time::Duration};

use crate::{
    add_ons::WorldChainAddOns,
    engine::FlashblocksEngineApiBuilder,
    node::{
        WorldChainNode, WorldChainNodeComponentBuilder, WorldChainNodeContext,
        WorldChainNodePrimitiveTypes,
    },
    payload::FlashblocksPayloadBuilderBuilder,
    payload_service::FlashblocksPayloadServiceBuilder,
    pool::WorldChainPoolBuilder,
};
use alloy_primitives::keccak256;
use ed25519_dalek::VerifyingKey;
use hex::ToHex;
use reth_network::protocol::IntoRlpxSubProtocol;
use reth_node_api::{FullNodeTypes, NodeTypes, TxTy};
use reth_node_builder::{
    NodeAdapter, NodeComponentsBuilder,
    components::{ComponentsBuilder, PayloadServiceBuilder},
    rpc::{BasicEngineValidatorBuilder, RpcAddOns},
};
use reth_node_core::primitives::Hardforks;
use reth_optimism_node::{
    OpConsensusBuilder, OpEngineTypes, OpEngineValidatorBuilder, OpNetworkBuilder, args::RollupArgs,
};
use reth_optimism_primitives::OpPrimitives;
use reth_optimism_rpc::OpEthApiBuilder;
use world_chain_chainspec::WorldChainSpec;
use world_chain_cli::{KonaArgs, WorldChainArgs, WorldChainNodeConfig};
use world_chain_kona::{AuthorizerKeys, FlashblocksAuthorizationNotifier, KonaConfig};
use world_chain_p2p::{
    monitor::PeerMonitor,
    protocol::{
        handler::{FlashblocksHandle, FlashblocksP2PProtocol},
        recorder::FlashblocksRecorderConfig,
    },
};
use world_chain_primitives::p2p::Authorization;
use world_chain_rpc::eth::FlashblocksEthApiBuilder;

use crossbeam_channel::{Receiver, Sender};
use tracing::{debug, info};
use world_chain_builder::WorldChainPayloadBuilderCtxBuilder;
use world_chain_evm::{
    BlockExecutionWitness, ExecutionWitnessHandle, WitnessCache, WorldChainEvmConfig,
    WorldChainExecutorBuilder,
};
use world_chain_pool::BasicWorldChainPool;
use world_chain_validator::coordinator::FlashblocksExecutionCoordinator;

use crate::tx_propagation::WorldChainTransactionPropagationPolicy;
use reth_network::PeersInfo;
use reth_network_peers::{PeerId, TrustedPeer};
use reth_node_builder::{BuilderContext, components::NetworkBuilder};
use reth_transaction_pool::{PoolTransaction, TransactionPool};

/// Network builder for World Chain that optionally applies custom transaction propagation policy
/// and registers the flashblocks P2P sub-protocol.
///
/// Extends OpNetworkBuilder to support restricting transaction gossip to specific peers
/// and ensures the flashblocks "flblk" capability is registered on the NetworkManager
/// before the network starts connecting to peers.
#[derive(Debug, Clone)]
pub struct WorldChainNetworkBuilder {
    op_network_builder: OpNetworkBuilder,
    tx_peers: Option<Vec<PeerId>>,
    p2p_handle: Option<FlashblocksHandle>,
    flashblock_sentries: Vec<TrustedPeer>,
    max_sentry_connections: usize,
}

impl WorldChainNetworkBuilder {
    pub fn new(
        disable_txpool_gossip: bool,
        disable_discovery_v4: bool,
        tx_peers: Option<Vec<PeerId>>,
        p2p_handle: Option<FlashblocksHandle>,
    ) -> Self {
        let op_network_builder = OpNetworkBuilder {
            disable_txpool_gossip,
            disable_discovery_v4,
        };

        Self {
            op_network_builder,
            tx_peers,
            p2p_handle,
            flashblock_sentries: Vec::new(),
            max_sentry_connections: 0,
        }
    }

    /// Configures the candidate flashblocks sentry pool and the number of sentries this node
    /// should maintain as trusted RLPx peers.
    pub fn with_flashblock_sentries(
        mut self,
        sentries: Vec<TrustedPeer>,
        max_connections: usize,
    ) -> Self {
        self.flashblock_sentries = sentries;
        self.max_sentry_connections = max_connections;
        self
    }
}

const FLASHBLOCKS_SENTRY_SELECTION_DOMAIN: &[u8] = b"worldchain-flashblocks-sentry-v1";

/// Ranks sentries using highest-random-weight (rendezvous) hashing.
///
/// A persisted local P2P identity produces a stable selection. Adding or removing a sentry only
/// remaps clients whose highest-ranked set changes.
fn rank_flashblock_sentries(local_peer_id: PeerId, sentries: &[TrustedPeer]) -> Vec<PeerId> {
    let mut ranked = sentries
        .iter()
        .filter(|sentry| sentry.id != local_peer_id)
        .map(|sentry| {
            let mut input = Vec::with_capacity(
                FLASHBLOCKS_SENTRY_SELECTION_DOMAIN.len() + local_peer_id.len() + sentry.id.len(),
            );
            input.extend_from_slice(FLASHBLOCKS_SENTRY_SELECTION_DOMAIN);
            input.extend_from_slice(local_peer_id.as_slice());
            input.extend_from_slice(sentry.id.as_slice());
            (keccak256(input), sentry.id)
        })
        .collect::<Vec<_>>();

    ranked.sort_unstable_by(|(left_score, left_id), (right_score, right_id)| {
        right_score
            .cmp(left_score)
            .then_with(|| left_id.cmp(right_id))
    });
    ranked.dedup_by_key(|(_, peer_id)| *peer_id);
    ranked.into_iter().map(|(_, peer_id)| peer_id).collect()
}

/// Adds flashblocks sentries to the already-resolved discovery bootnodes.
fn add_flashblock_sentry_bootnodes(bootnodes: &mut HashSet<TrustedPeer>, sentries: &[TrustedPeer]) {
    for sentry in sentries {
        if !bootnodes.iter().any(|bootnode| bootnode.id == sentry.id) {
            bootnodes.insert(sentry.clone());
        }
    }
}

/// Adds only the selected sentries to the trusted set and prevents RLPx connections to the rest.
///
/// The peer-manager ban list is intentionally separate from the discovery ban lists, so an
/// excluded combined bootnode/sentry can still be used for UDP discovery.
fn apply_flashblock_sentry_policy(
    peers_config: &mut reth_network::PeersConfig,
    local_peer_id: PeerId,
    sentries: &[TrustedPeer],
    max_connections: usize,
) -> Vec<PeerId> {
    let ranked = rank_flashblock_sentries(local_peer_id, sentries);
    let selected = ranked
        .iter()
        .copied()
        .take(max_connections)
        .collect::<Vec<_>>();
    let selected_set = selected.iter().copied().collect::<HashSet<_>>();
    let all_sentry_ids = sentries
        .iter()
        .map(|sentry| sentry.id)
        .collect::<HashSet<_>>();

    // Preserve unrelated operator-supplied trusted peers, while enforcing the selection for every
    // peer that belongs to the configured sentry pool.
    peers_config
        .trusted_nodes
        .retain(|peer| !all_sentry_ids.contains(&peer.id) || selected_set.contains(&peer.id));

    // A sentry may have been restored from the persisted peers file or supplied as a basic node.
    // Remove every candidate from those sets so selected sentries are re-added only as trusted
    // peers and excluded sentries cannot bypass the peer-manager ban during outbound slot refill.
    peers_config
        .basic_nodes
        .retain(|peer| !all_sentry_ids.contains(&peer.id));
    peers_config
        .persisted_peers
        .retain(|peer| !all_sentry_ids.contains(&peer.record.id));

    let mut trusted_ids = peers_config
        .trusted_nodes
        .iter()
        .map(|peer| peer.id)
        .collect::<HashSet<_>>();
    for sentry in sentries {
        if selected_set.contains(&sentry.id) && trusted_ids.insert(sentry.id) {
            peers_config.trusted_nodes.push(sentry.clone());
        }
    }

    for peer_id in all_sentry_ids.difference(&selected_set) {
        if *peer_id != local_peer_id {
            peers_config.ban_list.ban_peer(*peer_id);
        }
    }

    selected
}

impl<Node, Pool> NetworkBuilder<Node, Pool> for WorldChainNetworkBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: Hardforks>>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
{
    type Network = <OpNetworkBuilder as NetworkBuilder<Node, Pool>>::Network;

    async fn build_network(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<Self::Network> {
        let Self {
            op_network_builder,
            tx_peers,
            p2p_handle,
            flashblock_sentries,
            max_sentry_connections,
        } = self;

        let mut network_config = op_network_builder.network_config(ctx)?;
        add_flashblock_sentry_bootnodes(&mut network_config.boot_nodes, &flashblock_sentries);
        let local_peer_id = network_config.hello_message.id;
        network_config
            .peers_config
            .trusted_nodes
            .retain(|peer| peer.id != local_peer_id);

        let selected_sentries = apply_flashblock_sentry_policy(
            &mut network_config.peers_config,
            local_peer_id,
            &flashblock_sentries,
            max_sentry_connections,
        );
        if !flashblock_sentries.is_empty() {
            debug!(
                target: "world_chain::network",
                sentries = ?selected_sentries,
                "connecting to flashblocks sentries"
            );
        }

        let trusted_peer_ids: Vec<_> = network_config
            .peers_config
            .trusted_nodes
            .iter()
            .map(|peer| peer.id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let mut network = reth_network::NetworkManager::builder(network_config).await?;

        // Register flashblocks sub-protocol BEFORE starting the network.
        // This ensures the "flblk" capability is included in the RLPx handshake
        // for all peer connections, including the very first trusted peer connections.
        if let Some(ref flashblocks_handle) = p2p_handle {
            let network_handle = network.handle();
            let flashblocks_rlpx = FlashblocksP2PProtocol {
                network: network_handle,
                handle: flashblocks_handle.clone(),
            };
            network
                .network_mut()
                .add_rlpx_sub_protocol(flashblocks_rlpx.into_rlpx_sub_protocol());
        }

        // Start network with custom policy if specified, otherwise use default
        let handle = if let Some(peers) = tx_peers {
            tracing::info!(
                target: "world_chain::network",
                "Applying peer white listing transaction policy. Number of peers: {}",
                peers.len()
            );
            let policy = WorldChainTransactionPropagationPolicy::new(peers);
            let tx_config = ctx.config().network.transactions_manager_config();
            ctx.start_network_with(network, pool, tx_config, policy)
        } else {
            tracing::info!(
                target: "world_chain::network",
                "Starting network with default propagation policy"
            );
            ctx.start_network(network, pool)
        };

        tracing::info!(
            target: "world_chain::network",
            enode = %handle.local_node_record(),
            "World Chain P2P networking initialized"
        );

        // Set up peer monitor for flashblocks trusted peers
        if p2p_handle.is_some() {
            PeerMonitor::new(handle.clone())
                .with_initial_peers(trusted_peer_ids)
                .run_on_task_executor(ctx.task_executor());
        }

        Ok(handle)
    }
}

/// Shared witness-oracle plumbing, created once when `--witness.collect` is set.
///
/// The `sender` is handed to the capturing EVM config built in [`components`](WorldChainNodeContext::components),
/// while the `cache` and `receiver` are carried into the add-ons, where the collector thread is
/// spawned and the RPC oracle is installed.
#[derive(Clone, Debug)]
struct WitnessChannels {
    cache: ExecutionWitnessHandle,
    sender: Sender<BlockExecutionWitness>,
    receiver: Receiver<BlockExecutionWitness>,
}

#[derive(Clone, Debug)]
pub struct WorldChainDefaultContext {
    config: WorldChainNodeConfig,
    components_context: Option<FlashblocksComponentsContext>,
    witness: Option<WitnessChannels>,
}

impl WorldChainNodePrimitiveTypes for WorldChainDefaultContext {
    type Primitives = OpPrimitives;
    type Payload = OpEngineTypes;
    type ChainSpec = WorldChainSpec;
}

impl<N: FullNodeTypes<Types = WorldChainNode<WorldChainDefaultContext>>> WorldChainNodeContext<N>
    for WorldChainDefaultContext
where
    FlashblocksPayloadServiceBuilder<
        FlashblocksPayloadBuilderBuilder<WorldChainPayloadBuilderCtxBuilder>,
    >: PayloadServiceBuilder<N, BasicWorldChainPool<N>, WorldChainEvmConfig>,
{
    type Pool = BasicWorldChainPool<N>;
    type Net = WorldChainNetworkBuilder;
    type Evm = WorldChainEvmConfig;
    type PayloadServiceBuilder = FlashblocksPayloadServiceBuilder<
        FlashblocksPayloadBuilderBuilder<WorldChainPayloadBuilderCtxBuilder>,
    >;

    type ComponentsBuilder = WorldChainNodeComponentBuilder<N, Self>;

    type AddOns = WorldChainAddOns<
        NodeAdapter<N, <Self::ComponentsBuilder as NodeComponentsBuilder<N>>::Components>,
        FlashblocksEthApiBuilder,
        OpEngineValidatorBuilder,
        FlashblocksEngineApiBuilder<OpEngineValidatorBuilder>,
        BasicEngineValidatorBuilder<OpEngineValidatorBuilder>,
    >;

    type ExtContext = Option<FlashblocksComponentsContext>;

    fn components(&self) -> Self::ComponentsBuilder {
        let Self {
            config:
                WorldChainNodeConfig {
                    args:
                        WorldChainArgs {
                            rollup,
                            builder,
                            pbh,
                            flashblocks,
                            tx_peers,
                            ..
                        },
                    builder_config,
                    ..
                },
            components_context,
            witness,
        } = self.clone();

        let RollupArgs {
            disable_txpool_gossip,
            compute_pending_block: _,
            discovery_v4,
            ..
        } = rollup;

        let mut wc_network_builder = WorldChainNetworkBuilder::new(
            disable_txpool_gossip,
            !discovery_v4,
            tx_peers,
            components_context
                .as_ref()
                .map(|flashblocks_components_ctx| {
                    flashblocks_components_ctx.flashblocks_handle.clone()
                }),
        );
        if let Some(flashblocks) = flashblocks {
            wc_network_builder = wc_network_builder.with_flashblock_sentries(
                flashblocks.sentry_peers,
                flashblocks.max_sentry_connections,
            );
        }

        let (
            flashblocks_interval,
            flashblocks_recommit_interval,
            override_authorizer_sk,
            force_publish,
        ) = if let Some(flashblocks_args) = self.config.args.flashblocks.as_ref() {
            (
                flashblocks_args.flashblocks_interval,
                flashblocks_args.recommit_interval,
                flashblocks_args.override_authorizer_sk.clone(),
                flashblocks_args.force_publish,
            )
        } else {
            // Not important if flashblocks is not enabled. Put some numbers just to make
            // the compiler work fine.
            (200, 200, None, false)
        };

        let ctx_builder = WorldChainPayloadBuilderCtxBuilder {
            verified_blockspace_capacity: pbh.verified_blockspace_capacity,
            pbh_entry_point: pbh.entrypoint,
            pbh_signature_aggregator: pbh.signature_aggregator,
            builder_private_key: builder.private_key,
            block_uncompressed_size_limit: builder.block_uncompressed_size_limit,
        };

        ComponentsBuilder::default()
            .node_types::<N>()
            .pool(WorldChainPoolBuilder::new(
                pbh.entrypoint,
                pbh.signature_aggregator,
                pbh.world_id,
            ))
            .executor(WorldChainExecutorBuilder::new(
                witness.as_ref().map(|w| w.sender.clone()),
            ))
            .payload(FlashblocksPayloadServiceBuilder::new(
                FlashblocksPayloadBuilderBuilder::new(
                    ctx_builder,
                    components_context
                        .as_ref()
                        .map(|flashblocks_component_ctx| {
                            flashblocks_component_ctx.flashblocks_state.clone()
                        }),
                    builder_config,
                ),
                components_context
                    .as_ref()
                    .map(|flashblocks_components_ctx| {
                        flashblocks_components_ctx.flashblocks_handle.clone()
                    }),
                components_context
                    .as_ref()
                    .map(|flashblocks_components_ctx| {
                        flashblocks_components_ctx.flashblocks_state.clone()
                    }),
                components_context
                    .as_ref()
                    .map(|flashblocks_components_ctx| {
                        flashblocks_components_ctx
                            .to_jobs_generator
                            .clone()
                            .subscribe()
                    }),
                override_authorizer_sk,
                force_publish,
                Duration::from_millis(flashblocks_interval),
                Duration::from_millis(flashblocks_recommit_interval),
            ))
            .network(wc_network_builder)
            .consensus(OpConsensusBuilder::default())
    }

    fn add_ons(&self) -> Self::AddOns {
        let engine_api_builder = FlashblocksEngineApiBuilder {
            engine_validator_builder: Default::default(),
            flashblocks_handle: self.components_context.as_ref().map(
                |flashblocks_components_ctx| flashblocks_components_ctx.flashblocks_handle.clone(),
            ),
            to_jobs_generator: self
                .components_context
                .as_ref()
                .map(|flashblocks_components_ctx| {
                    flashblocks_components_ctx.to_jobs_generator.clone()
                }),
            authorizer_vk: self
                .components_context
                .as_ref()
                .map(|flashblocks_components_ctx| flashblocks_components_ctx.authorizer_vk),
            flashblocks_state: self
                .components_context
                .as_ref()
                .map(|flashblocks_components_ctx| {
                    flashblocks_components_ctx.flashblocks_state.clone()
                }),
        };
        let op_eth_api_builder =
            OpEthApiBuilder::default().with_sequencer(self.config.args.rollup.sequencer.clone());

        let maybe_pending_block =
            self.components_context
                .as_ref()
                .map(|flashblocks_components_ctx| {
                    flashblocks_components_ctx.flashblocks_state.pending_block()
                });
        let flashblocks_eth_api_builder =
            FlashblocksEthApiBuilder::new(op_eth_api_builder, maybe_pending_block);

        let engine_validator_builder =
            BasicEngineValidatorBuilder::<OpEngineValidatorBuilder>::default();

        let rpc_add_ons = RpcAddOns::new(
            flashblocks_eth_api_builder,
            Default::default(),
            engine_api_builder,
            engine_validator_builder,
            Default::default(),
            Default::default(),
        );

        WorldChainAddOns::new(
            rpc_add_ons,
            self.config.builder_config.inner.da_config.clone(),
            self.config.builder_config.inner.gas_limit_config.clone(),
            self.config.args.rollup.sequencer.clone(),
            Default::default(),
            Default::default(),
            false,
            1_000_000,
            self.config.args.simulate_enabled,
            self.witness
                .as_ref()
                .map(|w| (w.cache.clone(), w.receiver.clone())),
        )
        .with_kona_args(self.config.args.kona.clone())
        .with_flashblocks_authorizer(self.components_context.as_ref().map(
            |flashblocks_components_ctx| {
                // Mirror rollup-boost: when self-authorization keys are configured
                // (`--flashblocks.override-authorizer-sk` + `--flashblocks.builder-sk`), the
                // in-process Kona node mints full authorizations for the payloads it builds.
                let keys = self
                    .config
                    .args
                    .flashblocks
                    .as_ref()
                    .and_then(|flashblocks| {
                        let authorizer_sk = flashblocks.override_authorizer_sk.clone()?;
                        let builder_sk = flashblocks.builder_sk.as_ref()?;
                        Some(AuthorizerKeys {
                            authorizer_sk,
                            builder_vk: builder_sk.verifying_key(),
                        })
                    });
                FlashblocksAuthorizationNotifier {
                    to_jobs_generator: flashblocks_components_ctx.to_jobs_generator.clone(),
                    keys,
                }
            },
        ))
    }

    fn ext_context(&self) -> Self::ExtContext {
        self.components_context.clone()
    }
}

/// Builds a [`KonaConfig`](world_chain_kona::KonaConfig) from the parsed `--kona.*` CLI arguments.
///
/// The rollup configuration is loaded from the JSON file referenced by `--kona.rollup-config`,
/// which is required when Kona is enabled. Returns an error (which the caller propagates to fail
/// node startup) if the rollup config is missing, unreadable, or unparsable.
pub(crate) fn build_kona_config(
    kona_args: &KonaArgs,
) -> eyre::Result<world_chain_kona::KonaConfig> {
    let l1_rpc_url = kona_args.l1_rpc_url.parse()?;
    let l1_beacon_url = kona_args.l1_beacon_url.parse()?;

    let rollup_config_path = kona_args.rollup_config_path.as_ref().ok_or_else(|| {
        eyre::Report::msg("--kona.rollup-config is required when --kona.enabled is set")
    })?;
    let config_json = std::fs::read_to_string(rollup_config_path).map_err(|e| {
        eyre::Report::msg(format!(
            "failed to read rollup config from {}: {e}",
            rollup_config_path.display()
        ))
    })?;

    let rollup_config: kona_genesis::RollupConfig = serde_json::from_str(&config_json)
        .map_err(|e| eyre::Report::msg(format!("failed to parse rollup config: {e}")))?;

    Ok(KonaConfig {
        rollup_config: Arc::new(rollup_config),
        l1_rpc_url,
        l1_beacon_url,
        l1_trust_rpc: kona_args.l1_trust_rpc,
        sequencer_mode: kona_args.sequencer,
        sequencer_stopped: kona_args.sequencer_stopped,
        sequencer_recovery_mode: kona_args.sequencer_recovery_mode,
        conductor_rpc_url: kona_args.conductor_rpc.clone(),
        l1_confs: kona_args.l1_confs,
        p2p: kona_args.p2p.clone(),
        rpc_addr: kona_args.rpc_addr,
        rpc_port: kona_args.rpc_port,
        rpc_enable_admin: kona_args.rpc_enable_admin,
        rpc_enabled: !kona_args.rpc_disabled,
        l1_slot_duration_override: kona_args.l1_slot_duration_override,
    })
}

#[derive(Clone, Debug)]
pub struct FlashblocksComponentsContext {
    pub flashblocks_handle: FlashblocksHandle,
    pub flashblocks_state: FlashblocksExecutionCoordinator,
    pub to_jobs_generator: tokio::sync::watch::Sender<Option<Authorization>>,
    pub authorizer_vk: VerifyingKey,
}

impl From<WorldChainNodeConfig> for WorldChainDefaultContext {
    fn from(value: WorldChainNodeConfig) -> Self {
        let components_context = value
            .args
            .flashblocks
            .as_ref()
            .map(|_flashblocks_args| value.clone().into());

        let witness = value.args.witness.collect.then(|| {
            let cache = Arc::new(WitnessCache::with_depth(value.args.witness.depth));
            // Bounded so a slow collector applies backpressure (dropped witnesses) instead of growing
            // memory without limit; sized to the same retention as the cache.
            let (sender, receiver) = crossbeam_channel::bounded(value.args.witness.depth);
            WitnessChannels {
                cache,
                sender,
                receiver,
            }
        });

        Self {
            config: value,
            components_context,
            witness,
        }
    }
}

impl From<WorldChainNodeConfig> for FlashblocksComponentsContext {
    fn from(value: WorldChainNodeConfig) -> Self {
        let flashblocks = value
            .args
            .flashblocks
            .expect("Flashblocks args must be present");
        let recorder_config = value
            .flashblocks_store
            .map(|store| FlashblocksRecorderConfig::new(store.path));

        let authorizer_vk = flashblocks.authorizer_vk.unwrap_or_else(|| {
            flashblocks
                .override_authorizer_sk
                .as_ref()
                .expect("flashblocks authorizer_vk or override_authorizer_sk required")
                .verifying_key()
        });

        info!(
            "Flashblocks authorizer_vk: {}",
            authorizer_vk.as_bytes().encode_hex::<String>()
        );

        let flashblocks_handle = FlashblocksHandle::with_fanout_args_and_recorder(
            authorizer_vk,
            flashblocks.builder_sk.clone(),
            flashblocks.fanout.clone(),
            recorder_config,
        );

        let (pending_block, _) = tokio::sync::watch::channel(None);

        let flashblocks_state =
            FlashblocksExecutionCoordinator::new(flashblocks_handle.clone(), pending_block);

        let (to_jobs_generator, _) = tokio::sync::watch::channel(None);

        Self {
            flashblocks_state,
            flashblocks_handle,
            to_jobs_generator,
            authorizer_vk,
        }
    }
}

#[cfg(test)]
mod sentry_policy_tests {
    use super::*;
    use world_chain_cli::cli::FLASHBLOCKS_MAINNET_SENTRIES;

    fn sentries() -> Vec<TrustedPeer> {
        FLASHBLOCKS_MAINNET_SENTRIES
            .split(',')
            .map(|sentry| sentry.parse().expect("valid default sentry"))
            .collect()
    }

    #[test]
    fn rendezvous_selection_is_stable_and_order_independent() {
        let local_peer_id = PeerId::random();
        let sentries = sentries();
        let selected = rank_flashblock_sentries(local_peer_id, &sentries);

        let mut reversed = sentries.clone();
        reversed.reverse();

        assert_eq!(selected, rank_flashblock_sentries(local_peer_id, &reversed));
        assert_eq!(selected.len(), sentries.len());
    }

    #[test]
    fn sentries_are_added_without_replacing_chain_bootnodes() {
        let sentries = sentries();
        let mut chain_bootnode = sentries[0].clone();
        chain_bootnode.id = PeerId::random();
        let mut bootnodes = HashSet::from([chain_bootnode.clone()]);

        add_flashblock_sentry_bootnodes(&mut bootnodes, &sentries);

        assert!(bootnodes.contains(&chain_bootnode));
        assert_eq!(bootnodes.len(), sentries.len() + 1);
        for sentry in sentries {
            assert!(bootnodes.contains(&sentry));
        }
    }

    #[test]
    fn policy_trusts_two_and_bans_other_sentries() {
        let local_peer_id = PeerId::random();
        let sentries = sentries();
        let mut unrelated_peer = sentries[0].clone();
        unrelated_peer.id = PeerId::random();
        let mut peers_config = reth_network::PeersConfig::default();
        peers_config.trusted_nodes.push(unrelated_peer.clone());

        for sentry in &sentries {
            let record = sentry.resolve_blocking().expect("resolvable sentry");
            peers_config.basic_nodes.insert(record);
            peers_config.persisted_peers.push(
                reth_network::types::PersistedPeerInfo::from_node_record(record),
            );
        }

        let selected =
            apply_flashblock_sentry_policy(&mut peers_config, local_peer_id, &sentries, 2);
        let selected_set = selected.iter().copied().collect::<HashSet<_>>();

        assert_eq!(selected.len(), 2);
        assert!(peers_config.trusted_nodes.contains(&unrelated_peer));
        assert!(peers_config.basic_nodes.is_empty());
        assert!(peers_config.persisted_peers.is_empty());
        assert_eq!(
            peers_config
                .trusted_nodes
                .iter()
                .filter(|peer| selected_set.contains(&peer.id))
                .count(),
            2
        );

        let excluded = sentries
            .iter()
            .find(|sentry| !selected_set.contains(&sentry.id))
            .expect("one sentry should be excluded");
        assert!(peers_config.ban_list.is_banned_peer(&excluded.id));
    }

    #[test]
    fn max_at_least_pool_size_trusts_every_sentry() {
        let local_peer_id = PeerId::random();
        let sentries = sentries();
        let mut peers_config = reth_network::PeersConfig::default();

        let selected = apply_flashblock_sentry_policy(
            &mut peers_config,
            local_peer_id,
            &sentries,
            sentries.len(),
        );

        assert_eq!(selected.len(), sentries.len());
        for sentry in sentries {
            assert!(
                peers_config
                    .trusted_nodes
                    .iter()
                    .any(|trusted| trusted.id == sentry.id)
            );
            assert!(!peers_config.ban_list.is_banned_peer(&sentry.id));
        }
    }
}
