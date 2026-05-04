use std::marker::PhantomData;

use alloy_primitives::Address;
use reth_evm::ConfigureEvm;
use reth_node_api::{FullNodeTypes, NodePrimitives, NodeTypes, PrimitivesTy, TxTy};
use reth_node_builder::{
    BuilderContext,
    components::{PoolBuilder, PoolBuilderConfigOverrides},
};
use reth_optimism_forks::OpHardforks;
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_provider::{BlockReaderIdExt, CanonStateSubscriptions};
use reth_transaction_pool::{
    TransactionValidationTaskExecutor, TransactionValidator, blobstore::DiskFileBlobStore,
};
use tracing::{debug, info};
use world_chain_pool::{
    WorldChainTransactionPool,
    ordering::WorldChainOrdering,
    root::WorldChainRootValidator,
    tx::{WorldChainPoolTransaction, WorldChainPooledTransaction},
    validator::WorldChainTransactionValidator,
};
/// A basic World Chain transaction pool.
///
/// This contains various settings that can be configured and take precedence over the node's
/// config.
#[derive(Debug, Clone)]
pub struct WorldChainPoolBuilder<T = WorldChainPooledTransaction> {
    pub pbh_entrypoint: Address,
    pub pbh_signature_aggregator: Address,
    pub world_id: Address,
    /// Enforced overrides that are applied to the pool config.
    pub pool_config_overrides: PoolBuilderConfigOverrides,
    _pd: PhantomData<fn() -> T>,
}

impl<T> WorldChainPoolBuilder<T> {
    pub fn new(
        pbh_entrypoint: Address,
        pbh_signature_aggregator: Address,
        world_id: Address,
    ) -> Self {
        Self {
            pbh_entrypoint,
            pbh_signature_aggregator,
            world_id,
            pool_config_overrides: Default::default(),
            _pd: PhantomData,
        }
    }
}

impl<T> WorldChainPoolBuilder<T> {
    /// Sets the [`PoolBuilderConfigOverrides`] on the pool builder.
    pub fn with_pool_config_overrides(
        mut self,
        pool_config_overrides: PoolBuilderConfigOverrides,
    ) -> Self {
        self.pool_config_overrides = pool_config_overrides;
        self
    }
}

impl<Node, T, Evm> PoolBuilder<Node, Evm> for WorldChainPoolBuilder<T>
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec: OpHardforks>>,
    Node::Provider: BlockReaderIdExt<Block = <PrimitivesTy<Node::Types> as NodePrimitives>::Block>,
    T: WorldChainPoolTransaction<Consensus = TxTy<Node::Types>>,
    Evm: ConfigureEvm<Primitives = PrimitivesTy<Node::Types>> + Clone + 'static,
    WorldChainTransactionValidator<Node::Provider, T, Evm>: TransactionValidator<
            Transaction = T,
            Block = <PrimitivesTy<Node::Types> as NodePrimitives>::Block,
        >,
{
    type Pool = WorldChainTransactionPool<Node::Provider, DiskFileBlobStore, T, Evm>;

    async fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
        evm_config: Evm,
    ) -> eyre::Result<Self::Pool> {
        let Self {
            pbh_entrypoint,
            pbh_signature_aggregator,
            world_id,
            pool_config_overrides,
            ..
        } = self;

        let data_dir = ctx.config().datadir();
        let blob_store = DiskFileBlobStore::open(data_dir.blobstore(), Default::default())?;

        let validator =
            TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone(), evm_config)
                .no_eip4844()
                .kzg_settings(ctx.kzg_settings()?)
                .with_additional_tasks(
                    pool_config_overrides
                        .additional_validation_tasks
                        .unwrap_or_else(|| ctx.config().txpool.additional_validation_tasks),
                )
                .build_with_tasks(ctx.task_executor().clone(), blob_store.clone())
                .map(|validator| {
                    let client = validator.client().clone();
                    let op_tx_validator = OpTransactionValidator::new(validator)
                        // In --dev mode we can't require gas fees because we're unable to decode the L1
                        // block info
                        .require_l1_data_gas_fee(!ctx.config().dev.dev);
                    let root_validator = WorldChainRootValidator::new(client, world_id)
                        .expect("failed to initialize root validator");

                    WorldChainTransactionValidator::new(
                        op_tx_validator,
                        root_validator,
                        pbh_entrypoint,
                        pbh_signature_aggregator,
                    )
                    .expect("failed to create world chain validator")
                });

        let transaction_pool = reth_transaction_pool::Pool::new(
            validator,
            WorldChainOrdering::default(),
            blob_store,
            pool_config_overrides.apply(ctx.pool_config()),
        );

        info!(target: "reth::cli", "Transaction pool initialized");
        let transactions_path = data_dir.txpool_transactions();

        // spawn txpool maintenance task
        {
            let pool = transaction_pool.clone();
            let chain_events = ctx.provider().canonical_state_stream();
            let client = ctx.provider().clone();
            let transactions_backup_config =
                    reth_transaction_pool::maintain::LocalTransactionBackupConfig::with_local_txs_backup(transactions_path);

            ctx.task_executor()
                .spawn_critical_with_graceful_shutdown_signal(
                    "local transactions backup task",
                    |shutdown| {
                        reth_transaction_pool::maintain::backup_local_transactions_task(
                            shutdown,
                            pool.clone(),
                            transactions_backup_config,
                        )
                    },
                );

            // spawn the maintenance task
            ctx.task_executor().spawn_critical_task(
                "txpool maintenance task",
                reth_transaction_pool::maintain::maintain_transaction_pool_future(
                    client,
                    pool.clone(),
                    chain_events,
                    ctx.task_executor().clone(),
                    reth_transaction_pool::maintain::MaintainPoolConfig {
                        max_tx_lifetime: pool.config().max_queued_lifetime,
                        no_local_exemptions: transaction_pool
                            .config()
                            .local_transactions_config
                            .no_exemptions,
                        ..Default::default()
                    },
                ),
            );
            debug!(target: "reth::cli", "Spawned txpool maintenance task");
        }

        Ok(transaction_pool)
    }
}
