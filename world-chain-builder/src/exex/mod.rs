use futures::TryStreamExt;
use reth::api::FullNodeComponents;
use reth_exex::{ExExContext, ExExNotification};

async fn pbh_exex<Node: FullNodeComponents>(mut ctx: ExExContext<Node>) -> eyre::Result<()> {
    while let Some(notification) = ctx.notifications.try_next().await? {
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                todo!()
            }
            ExExNotification::ChainReorged { old, new } => {
                todo!()
            }
            ExExNotification::ChainReverted { old } => {
                todo!()
            }
        };
    }

    Ok(())
}
