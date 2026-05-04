//! Node contexts and type adapters used by the e2e harness.
//!
//! The generic `setup<T>` harness builds a `WorldChainNode<T>` through reth's
//! normal `NodeBuilder::with_types_and_provider` path. That means the
//! transaction envelope must be present at the `NodeTypes::Primitives` layer,
//! not hidden behind a transaction-pool alias. The native-AA context in this
//! file provides that type surface by making
//! [`WorldChainTxEnvelope`] the signed transaction type for the node.
//!
//! Pool transaction and pool alias changes are intentionally not defined here.
//! Those aliases still need their own generic native-AA wiring before
//! `WorldChainTestContext` can launch the complete production component graph.

use alloy_consensus::{Block, BlockBody, Header};
use reth_e2e_test_utils::TmpDB;
use reth_node_api::{FullNodeTypesAdapter, NodePrimitives, NodeTypesWithDBAdapter};
use reth_optimism_node::{OpEngineTypes, OpStorage};
use reth_optimism_payload_builder::OpPayloadTypes;
use reth_optimism_primitives::OpReceipt;
use reth_provider::providers::BlockchainProvider;
use world_chain_cli::WorldChainNodeConfig;
use world_chain_node::node::{WorldChainNode, WorldChainNodeTypes};
use world_chain_primitives::transaction::WorldChainTxEnvelope;

/// Block type used by the native-AA e2e node context.
///
/// The block structure remains the standard alloy consensus block. Only the
/// transaction envelope changes, allowing WIP-1001 transactions to be admitted
/// at the primitive layer.
pub type WorldChainTestBlock = Block<WorldChainTxEnvelope>;

/// Block body type used by the native-AA e2e node context.
///
/// This is the body associated with [`WorldChainTestBlock`], with
/// [`WorldChainTxEnvelope`] as the transaction type and the standard Ethereum
/// header type as the ommer header.
pub type WorldChainTestBlockBody = BlockBody<WorldChainTxEnvelope>;

/// Node primitives for native account-abstraction e2e tests.
///
/// The production World Chain node currently inherits Optimism primitives,
/// whose signed transaction type is `OpTransactionSigned`. Native-AA tests need
/// the node itself to expose [`WorldChainTxEnvelope`] so that WIP-1001
/// transactions flow through the same primitive path as legacy, EIP-1559,
/// deposit, and post-exec transactions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorldChainTestPrimitives;

impl NodePrimitives for WorldChainTestPrimitives {
    type Block = WorldChainTestBlock;
    type BlockHeader = Header;
    type BlockBody = WorldChainTestBlockBody;
    type SignedTx = WorldChainTxEnvelope;
    type Receipt = OpReceipt;
}

/// OP payload type wrapper parameterized by [`WorldChainTestPrimitives`].
///
/// Reusing the OP payload wrapper preserves the existing Optimism payload
/// attributes while tying built payloads back to the native-AA primitive set.
pub type WorldChainTestPayload = OpEngineTypes<OpPayloadTypes<WorldChainTestPrimitives>>;

/// Storage adapter for native-AA e2e nodes.
///
/// This mirrors Optimism storage while replacing the persisted transaction type
/// with [`WorldChainTxEnvelope`].
pub type WorldChainTestStorage = OpStorage<WorldChainTxEnvelope>;

/// World Chain node marker for native-AA e2e tests.
///
/// `WorldChainNode<T>` now takes its primitive types from `T`, so this alias is
/// the concrete node type whose [`NodePrimitives::SignedTx`] is
/// [`WorldChainTxEnvelope`].
pub type WorldChainTestNode = WorldChainNode<WorldChainTestContext>;

/// Provider used by the traditional reth e2e harness for native-AA nodes.
///
/// This is the provider argument passed to
/// `NodeBuilder::with_types_and_provider::<WorldChainTestNode, WorldChainTestProvider>()`.
pub type WorldChainTestProvider =
    BlockchainProvider<NodeTypesWithDBAdapter<WorldChainTestNode, TmpDB>>;

/// Full node type adapter used by the traditional e2e harness.
///
/// This is the `FullNodeTypesAdapter` form used by the existing `setup<T>`
/// harness once the context's component and pool aliases are wired for
/// [`WorldChainTxEnvelope`].
pub type WorldChainTestNodeTypes =
    FullNodeTypesAdapter<WorldChainTestNode, TmpDB, WorldChainTestProvider>;

/// Native account-abstraction e2e context.
///
/// The context stores the same [`WorldChainNodeConfig`] as the production
/// context but supplies [`WorldChainTestPrimitives`] as the node primitive set.
/// Component construction remains intentionally out of this file until the
/// transaction pool can be parameterized over [`WorldChainTxEnvelope`].
#[derive(Clone, Debug)]
pub struct WorldChainTestContext {
    config: WorldChainNodeConfig,
}

impl WorldChainTestContext {
    /// Returns the node configuration captured when the context was created.
    pub fn config(&self) -> &WorldChainNodeConfig {
        &self.config
    }
}

impl From<WorldChainNodeConfig> for WorldChainTestContext {
    fn from(config: WorldChainNodeConfig) -> Self {
        Self { config }
    }
}

impl WorldChainNodeTypes for WorldChainTestContext {
    type Primitives = WorldChainTestPrimitives;
}
