use std::sync::Arc;

use futures::TryStreamExt;
use reth::api::FullNodeComponents;
use reth_db::Database;
use reth_db::DatabaseEnv;
use reth_exex::{ExExContext, ExExNotification};

use crate::pbh::db::is_pbh_tx;

async fn pbh_exex<Node: FullNodeComponents>(
    mut ctx: ExExContext<Node>,
    pbh_db: Arc<DatabaseEnv>,
) -> eyre::Result<()> {
    while let Some(notification) = ctx.notifications.try_next().await? {
        let db_tx = pbh_db.tx()?;

        match &notification {
            ExExNotification::ChainCommitted { new } => {
                // get all transactions from all blocks
                for (_, sealed_block) in new.blocks() {
                    for tx in sealed_block.transactions() {
                        if is_pbh_tx(tx.hash(), &db_tx)? {}
                    }
                }
            }
            ExExNotification::ChainReorged { old, new } => {}
            ExExNotification::ChainReverted { old } => {}
        };
    }

    Ok(())
}
