use super::validator::WorldChainTransactionPool;
use crate::pool::ordering::WorldChainOrdering;
use crate::pool::root::WorldChainRootValidator;
use crate::pool::validator::WorldChainTransactionValidator;
use reth::builder::components::PoolBuilder;
use reth::builder::{BuilderContext, FullNodeTypes, NodeTypes};
use reth::transaction_pool::blobstore::DiskFileBlobStore;
use reth::transaction_pool::TransactionValidationTaskExecutor;
use reth_db::DatabaseEnv;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_node::txpool::OpTransactionValidator;
use reth_optimism_primitives::OpPrimitives;
use reth_provider::CanonStateSubscriptions;
use std::sync::Arc;
use tracing::{debug, info};

/// A basic World Chain transaction pool.
///
/// This contains various settings that can be configured and take precedence over the node's
/// config.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WorldChainPoolBuilder {
    pub clear_nullifiers: bool,
    pub num_pbh_txs: u16,
    pub db: Arc<DatabaseEnv>,
}

impl<Node> PoolBuilder<Node> for WorldChainPoolBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = OpChainSpec, Primitives = OpPrimitives>>,
{
    type Pool = WorldChainTransactionPool<Node::Provider, DiskFileBlobStore>;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        let data_dir = ctx.config().datadir();
        let blob_store = DiskFileBlobStore::open(data_dir.blobstore(), Default::default())?;
        let validator = TransactionValidationTaskExecutor::eth_builder(Arc::new(
            ctx.chain_spec().inner.clone(),
        ))
        .with_head_timestamp(ctx.head().timestamp)
        .kzg_settings(ctx.kzg_settings()?)
        .with_additional_tasks(ctx.config().txpool.additional_validation_tasks)
        .build_with_tasks(
            ctx.provider().clone(),
            ctx.task_executor().clone(),
            blob_store.clone(),
        )
        .map(|validator| {
            let op_tx_validator = OpTransactionValidator::new(validator.clone())
                // In --dev mode we can't require gas fees because we're unable to decode the L1
                // block info
                .require_l1_data_gas_fee(!ctx.config().dev.dev);
            let root_validator = WorldChainRootValidator::new(validator.client().clone())
                .expect("failed to initialize root validator");
            WorldChainTransactionValidator::new(
                op_tx_validator,
                root_validator,
                self.db.clone(),
                self.num_pbh_txs,
            )
        });

        let ordering = WorldChainOrdering::default();

        let transaction_pool =
            reth::transaction_pool::Pool::new(validator, ordering, blob_store, ctx.pool_config());
        info!(target: "reth::cli", "Transaction pool initialized");
        let transactions_path = data_dir.txpool_transactions();

        // spawn txpool maintenance task
        {
            let pool = transaction_pool.clone();
            let chain_events = ctx.provider().canonical_state_stream();
            let client = ctx.provider().clone();
            let transactions_backup_config =
                reth::transaction_pool::maintain::LocalTransactionBackupConfig::with_local_txs_backup(transactions_path);

            ctx.task_executor()
                .spawn_critical_with_graceful_shutdown_signal(
                    "local transactions backup task",
                    |shutdown| {
                        reth::transaction_pool::maintain::backup_local_transactions_task(
                            shutdown,
                            pool.clone(),
                            transactions_backup_config,
                        )
                    },
                );

            // spawn the maintenance task
            ctx.task_executor().spawn_critical(
                "txpool maintenance task",
                reth::transaction_pool::maintain::maintain_transaction_pool_future(
                    client,
                    pool,
                    chain_events,
                    ctx.task_executor().clone(),
                    Default::default(),
                ),
            );
            debug!(target: "reth::cli", "Spawned txpool maintenance task");
        }

        Ok(transaction_pool)
    }
}
