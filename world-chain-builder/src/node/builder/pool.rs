use std::sync::Arc;

use reth_chainspec::ChainSpec;
use reth_db::create_db;
use reth_db::mdbx::DatabaseArguments;
use reth_node_builder::components::PoolBuilder;
use reth_node_builder::{BuilderContext, FullNodeTypes, NodeTypes};
use reth_node_optimism::txpool::OpTransactionValidator;
use reth_provider::CanonStateSubscriptions as _;
use reth_transaction_pool::blobstore::DiskFileBlobStore;
use reth_transaction_pool::{CoinbaseTipOrdering, TransactionValidationTaskExecutor};
use tracing::{debug, info};

use crate::txpool::{WorldChainTransactionPool, WorldChainTransactionValidator};

/// A basic World Chain transaction pool.
///
/// This contains various settings that can be configured and take precedence over the node's
/// config.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct WorldChainPoolBuilder {
    pub clear_nullifiers: bool,
}

impl<Node> PoolBuilder<Node> for WorldChainPoolBuilder
where
    Node: FullNodeTypes<Types: NodeTypes<ChainSpec = ChainSpec>>,
{
    type Pool = WorldChainTransactionPool<Node::Provider, DiskFileBlobStore>;

    async fn build_pool(self, ctx: &BuilderContext<Node>) -> eyre::Result<Self::Pool> {
        let data_dir = ctx.config().datadir();
        let blob_store = DiskFileBlobStore::open(data_dir.blobstore(), Default::default())?;

        // TODO: couldn't figure out how to get write access to the database from ctx (perhaps not
        // possible) so creating one here as a workaround. Maybe this is actually the best approach?
        //
        let path = data_dir.data_dir().join("semaphore-nullifiers");
        if self.clear_nullifiers {
            info!(?path, "Clearing semaphore-nullifiers database");
            // delete the directory
            std::fs::remove_dir_all(&path)?;
        }
        info!(?path, "Opening semaphore-nullifiers database");
        let db = Arc::new(create_db(
            data_dir.data_dir().join("semaphore-nullifiers"),
            DatabaseArguments::default(),
        )?);

        let validator = TransactionValidationTaskExecutor::eth_builder(ctx.chain_spec())
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
                WorldChainTransactionValidator::new(op_tx_validator, db.clone(), validator)
            });

        let transaction_pool = reth_transaction_pool::Pool::new(
            validator,
            CoinbaseTipOrdering::default(),
            blob_store,
            ctx.pool_config(),
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
            ctx.task_executor().spawn_critical(
                "txpool maintenance task",
                reth_transaction_pool::maintain::maintain_transaction_pool_future(
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
