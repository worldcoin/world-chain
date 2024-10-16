use std::sync::Arc;

use futures::TryStreamExt;
use reth::api::FullNodeComponents;
use reth_db::transaction::DbTx;
use reth_db::Database;
use reth_db::DatabaseEnv;
use reth_exex::{ExExContext, ExExNotification};

use crate::pbh::db::get_validated_nullifier;
use crate::pbh::db::remove_executed_nullifier;
use crate::pbh::db::set_executed_nullifier;

pub async fn pbh_exex<Node: FullNodeComponents>(
    mut ctx: ExExContext<Node>,
    pbh_db: Arc<DatabaseEnv>,
) -> eyre::Result<()> {
    while let Some(notification) = ctx.notifications.try_next().await? {
        let db_tx = pbh_db.tx_mut()?;

        match &notification {
            ExExNotification::ChainCommitted { new } => {
                // Insert executed nullifiers for the new block
                for (_, sealed_block) in new.blocks() {
                    for tx in sealed_block.transactions() {
                        if let Some(nullifier) = get_validated_nullifier(&db_tx, tx.hash())? {
                            set_executed_nullifier(&db_tx, nullifier)?;
                        }
                    }
                }
            }
            ExExNotification::ChainReorged { old, new } => {
                // Remove old nullifiers from reorged chain
                for (_, sealed_block) in old.blocks() {
                    for tx in sealed_block.transactions() {
                        if let Some(nullifier) = get_validated_nullifier(&db_tx, tx.hash())? {
                            remove_executed_nullifier(&db_tx, nullifier)?;
                        }
                    }
                }

                // Insert new nullifiers from updated chain
                for (_, sealed_block) in new.blocks() {
                    for tx in sealed_block.transactions() {
                        if let Some(nullifier) = get_validated_nullifier(&db_tx, tx.hash())? {
                            set_executed_nullifier(&db_tx, nullifier)?;
                        }
                    }
                }
            }
            ExExNotification::ChainReverted { old } => {
                // Remove old nullifiers from reverted chain
                for (_, sealed_block) in old.blocks() {
                    for tx in sealed_block.transactions() {
                        if let Some(nullifier) = get_validated_nullifier(&db_tx, tx.hash())? {
                            remove_executed_nullifier(&db_tx, nullifier)?;
                        }
                    }
                }
            }
        };

        // Commit the pbh nullifiers to the db
        db_tx.commit()?;
    }

    Ok(())
}
