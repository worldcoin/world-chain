use std::sync::Arc;

use reth_basic_payload_builder::{
    BasicPayloadJobGenerator, BasicPayloadJobGeneratorConfig, BuildArguments, BuildOutcome,
    MissingPayloadBehaviour, PayloadBuilder, PayloadConfig,
};
use reth_chainspec::ChainSpec;
use reth_db::DatabaseEnv;
use reth_evm::ConfigureEvm;
use reth_evm_optimism::OptimismEvmConfig;
use reth_node_builder::components::PayloadServiceBuilder;
use reth_node_builder::{BuilderContext, FullNodeTypes, NodeTypesWithEngine, PayloadBuilderConfig};
use reth_node_optimism::{
    OptimismBuiltPayload, OptimismEngineTypes, OptimismPayloadBuilderAttributes,
};
use reth_payload_builder::error::PayloadBuilderError;
use reth_payload_builder::{PayloadBuilderHandle, PayloadBuilderService};
use reth_provider::{CanonStateSubscriptions, StateProviderFactory};
use reth_transaction_pool::TransactionPool;

use crate::node::builder::load_world_chain_db;

/// Priority blockspace for humans builder
#[derive(Debug, Clone)]
pub struct WcPayloadBuilder<EvmConfig> {
    // NOTE: do we need this?
    // compute_pending_block: bool,
    evm_config: EvmConfig,
    database_env: Arc<DatabaseEnv>,
}

impl<EvmConfig> WcPayloadBuilder<EvmConfig> {
    /// `OptimismPayloadBuilder` constructor.
    pub const fn new(evm_config: EvmConfig, database_env: Arc<DatabaseEnv>) -> Self {
        Self {
            evm_config,
            database_env,
        }
    }

    // fn set_validated(
    //     &self,
    //     tx: &Tx,
    //     semaphore_proof: &SemaphoreProof,
    // ) -> Result<(), DatabaseError> {
    //     let db_tx = self.database_env.tx_mut()?;
    //     let mut cursor = db_tx.cursor_write::<ValidatedPbhTransactionTable>()?;
    //     cursor.insert(
    //         *tx.hash(),
    //         semaphore_proof.nullifier_hash.to_be_bytes().into(),
    //     )?;
    //     db_tx.commit()?;
    //     Ok(())
    // }
    //
    // fn set_validated(
    //     &self,
    //     tx: &Tx,
    //     semaphore_proof: &SemaphoreProof,
    // ) -> Result<(), DatabaseError> {
    //     let db_tx = self.database_env.tx_mut()?;
    //     let mut cursor = db_tx.cursor_write::<ValidatedPbhTransactionTable>()?;
    //     cursor.insert(
    //         *tx.hash(),
    //         semaphore_proof.nullifier_hash.to_be_bytes().into(),
    //     )?;
    //     db_tx.commit()?;
    //     Ok(())
    // }
}

/// Implementation of the [`PayloadBuilder`] trait for [`PBHBuilder`].
impl<Pool, Client, EvmConfig> PayloadBuilder<Pool, Client> for WcPayloadBuilder<EvmConfig>
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

#[derive(Debug, Default, Clone)]
pub struct WcPayloadServiceBuilder<EVM = OptimismEvmConfig> {
    /// The EVM configuration to use for the payload builder.
    pub evm_config: EVM,
}

impl<EVM> WcPayloadServiceBuilder<EVM> {
    pub const fn new(evm_config: EVM) -> Self {
        Self { evm_config }
    }
}

impl<Node, EVM, Pool> PayloadServiceBuilder<Node, Pool> for WcPayloadServiceBuilder<EVM>
where
    Node: FullNodeTypes<
        Types: NodeTypesWithEngine<Engine = OptimismEngineTypes, ChainSpec = ChainSpec>,
    >,
    Pool: TransactionPool + Unpin + 'static,
    EVM: ConfigureEvm,
{
    async fn spawn_payload_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<OptimismEngineTypes>> {
        let data_dir = ctx.config().datadir();
        let db = load_world_chain_db(data_dir.data_dir(), false)?;
        let payload_builder = WcPayloadBuilder::new(self.evm_config, db);

        let conf = ctx.payload_builder_config();

        let payload_job_config = BasicPayloadJobGeneratorConfig::default()
            .interval(conf.interval())
            .deadline(conf.deadline())
            .max_payload_tasks(conf.max_payload_tasks())
            // no extradata for OP
            .extradata(Default::default());

        let payload_generator = BasicPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            pool,
            ctx.task_executor().clone(),
            payload_job_config,
            ctx.chain_spec(),
            payload_builder,
        );
        let (payload_service, payload_builder) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("payload builder service", Box::pin(payload_service));

        Ok(payload_builder)
    }
}
