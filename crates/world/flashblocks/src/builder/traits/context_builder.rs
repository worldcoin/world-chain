use std::sync::Arc;

use op_alloy_consensus::OpTxEnvelope;
use reth::revm::cancelled::CancelOnDrop;
use reth_basic_payload_builder::PayloadConfig;
use reth_evm::ConfigureEvm;
use reth_optimism_node::{OpBuiltPayload, OpEvmConfig, OpPayloadBuilderAttributes};
use reth_optimism_payload_builder::config::OpDAConfig;
use reth_primitives::NodePrimitives;

use crate::builder::traits::context::PayloadBuilderCtx;

/// Builder trait for creating [`PayloadBuilderCtx`] instances with specific configurations.
///
/// This trait provides a factory pattern for constructing payload builder contexts
/// that are used to execute transactions composing individual flashblocks.
///
/// ```rust,ignore
/// use reth_basic_payload_builder::PayloadConfig;
/// use reth_optimism_node::OpBuiltPayload;
/// use std::sync::Arc;
///
/// // Example implementation
/// #[derive(Clone)]
/// struct MyCtxBuilder {
///     client: Arc<MyClient>,
///     pool: Arc<MyPool>,
///     // ...
/// }
///
/// impl PayloadBuilderCtxBuilder<OpEvmConfig, OpChainSpec, MyTransaction> for MyCtxBuilder {
///     type PayloadBuilderCtx = MyPayloadBuilderCtx;
///
///     fn build<Txs>(
///         &self,
///         evm: OpEvmConfig,
///         da_config: OpDAConfig,
///         chain_spec: Arc<OpChainSpec>,
///         config: PayloadConfig<OpPayloadBuilderAttributes<OpTxEnvelope>, Header>,
///         cancel: &CancelOnDrop,
///         best_payload: Option<OpBuiltPayload>,
///     ) -> Self::PayloadBuilderCtx {
///         MyPayloadBuilderCtx {
///             evm,
///             chain_spec,
///             client: self.client.clone(),
///             // ... configure the context
///         }
///     }
/// }
///
/// // Usage in flashblock builder
/// let flashblock_builder = FlashblocksPayloadBuilder {
///     ctx_builder: MyCtxBuilder::new(client, pool),
///     // ...
/// };
/// ```
pub trait PayloadBuilderCtxBuilder<EvmConfig: ConfigureEvm, ChainSpec, Transaction>:
    Clone + Send + Sync
{
    /// The concrete payload builder context type produced by this builder.
    ///
    /// This context will be used throughout the payload building process to
    /// access configuration, execute transactions, and manage block state.
    type PayloadBuilderCtx: PayloadBuilderCtx<
        Evm = EvmConfig,
        ChainSpec = ChainSpec,
        Transaction = Transaction,
    >;

    /// Constructs a new payload builder context with the given configuration.
    ///
    /// This method is called at the start of each payload building job to create
    /// a fresh context with the appropriate settings. The context encapsulates
    /// all the state and configuration needed for building a single payload.
    ///
    /// # Parameters
    ///
    /// * `evm` - EVM configuration for transaction execution
    /// * `da_config` - Data availability configuration for Optimism L2
    /// * `chain_spec` - Chain specification defining network rules and hardforks
    /// * `config` - Payload configuration including attributes and parent header
    /// * `cancel` - Cancellation token for aborting the payload building job
    /// * `best_payload` - Previous best payload that may be improved upon
    ///
    /// # Returns
    ///
    /// A configured [`PayloadBuilderCtx`] ready for use in payload building.
    fn build<Txs>(
        &self,
        evm: EvmConfig,
        da_config: OpDAConfig,
        chain_spec: Arc<ChainSpec>,
        config: PayloadConfig<
            OpPayloadBuilderAttributes<OpTxEnvelope>,
            <<OpEvmConfig as ConfigureEvm>::Primitives as NodePrimitives>::BlockHeader,
        >,
        cancel: &CancelOnDrop,
        best_payload: Option<OpBuiltPayload>,
    ) -> Self::PayloadBuilderCtx
    where
        Self: Sized;
}
