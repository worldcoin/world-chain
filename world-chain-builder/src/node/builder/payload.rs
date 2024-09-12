use reth_basic_payload_builder::{
    BuildArguments, BuildOutcome, MissingPayloadBehaviour, PayloadBuilder, PayloadConfig,
};
use reth_evm::ConfigureEvm;
use reth_node_optimism::{OptimismBuiltPayload, OptimismPayloadBuilderAttributes};
use reth_payload_builder::error::PayloadBuilderError;
use reth_provider::StateProviderFactory;
use reth_transaction_pool::TransactionPool;

/// Priority blockspace for humans builder
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PBHBuilder<EvmConfig> {
    // NOTE: do we need this?
    // compute_pending_block: bool,
    evm_config: EvmConfig,
}

impl<EvmConfig> PBHBuilder<EvmConfig> {
    /// `OptimismPayloadBuilder` constructor.
    pub const fn new(evm_config: EvmConfig) -> Self {
        Self { evm_config }
    }
}

/// Implementation of the [`PayloadBuilder`] trait for [`PBHBuilder`].
impl<Pool, Client, EvmConfig> PayloadBuilder<Pool, Client> for PBHBuilder<EvmConfig>
where
    Client: StateProviderFactory,
    Pool: TransactionPool,
    EvmConfig: ConfigureEvm,
{
    type Attributes = OptimismPayloadBuilderAttributes;
    type BuiltPayload = OptimismBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Pool, Client, OptimismPayloadBuilderAttributes, OptimismBuiltPayload>,
    ) -> Result<BuildOutcome<OptimismBuiltPayload>, PayloadBuilderError> {
        todo!()
    }

    fn on_missing_payload(
        &self,
        _args: BuildArguments<Pool, Client, OptimismPayloadBuilderAttributes, OptimismBuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        MissingPayloadBehaviour::AwaitInProgress
    }

    fn build_empty_payload(
        &self,
        client: &Client,
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<OptimismBuiltPayload, PayloadBuilderError> {
        todo!()
    }
}
