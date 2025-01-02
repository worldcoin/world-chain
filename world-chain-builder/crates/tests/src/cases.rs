use std::sync::Arc;
use std::time::Duration;

use crate::fixtures::PBHFixture;
use crate::run_command;
use alloy_primitives::hex;
use alloy_provider::PendingTransactionBuilder;
use alloy_provider::Provider;
use alloy_rpc_types_eth::erc4337::ConditionalOptions;
use alloy_transport::Transport;
use eyre::eyre::Result;
use futures::stream;
use futures::StreamExt;
use futures::TryStreamExt;
use tokio::time::sleep;
use tracing::debug;

const CONCURRENCY_LIMIT: usize = 50;

/// Asserts that the world-chain-builder payload is built correctly with a set of PBH transactions.
pub async fn assert_build<T, P>(builder_provider: Arc<P>, fixture: PBHFixture) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    let num_transactions = fixture.fixture.len();
    let half = num_transactions / 2;
    let builder_provider_clone = builder_provider.clone();
    stream::iter(fixture.fixture.chunks(30).enumerate())
        .map(Ok)
        .try_for_each_concurrent(CONCURRENCY_LIMIT, move |(index, transactions)| {
            let builder_provider = builder_provider_clone.clone();
            async move {
                for transaction in transactions {
                    let tx = if index < half {
                        // First half, use eth_sendRawTransaction
                        builder_provider.send_raw_transaction(transaction).await?
                    } else {
                        // Second half, use eth_sendRawTransactionConditional
                        let rlp_hex = hex::encode_prefixed(transaction);
                        let tx_hash = builder_provider
                            .client()
                            .request(
                                "eth_sendRawTransactionConditional",
                                (rlp_hex, ConditionalOptions::default()),
                            )
                            .await?;
                        PendingTransactionBuilder::new(&builder_provider.root(), tx_hash)
                    };
                    let hash = *tx.tx_hash();
                    let receipt = tx.get_receipt().await;
                    assert!(receipt.is_ok());
                    debug!(
                        receipt = ?receipt.unwrap(),
                        hash = ?hash,
                        index = index,
                        "Transaction Receipt Received"
                    );
                }
                Ok::<(), eyre::Report>(())
            }
        })
        .await?;
    Ok(())
}

/// Asserts that the chain continues to advance in the case when the world-chain-builder service is MIA.
pub async fn assert_fallback<T, P>(sequencer_provider: P) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    // Grab the latest block number
    let block_number = sequencer_provider.get_block_number().await?;

    run_command(
        "kurtosis",
        &[
            "service",
            "stop",
            "world-chain",
            "wc-admin-world-chain-builder",
        ],
        env!("CARGO_MANIFEST_DIR"),
    )
    .await?;
    sleep(Duration::from_secs(5)).await;

    let retries = 3;
    let mut tries = 0;
    loop {
        // Assert the chain has progressed
        let new_block_number = sequencer_provider.get_block_number().await?;
        if new_block_number > block_number {
            break;
        }

        if tries >= retries {
            panic!("Chain did not progress after {} retries", retries);
        }

        sleep(Duration::from_secs(2)).await;
        tries += 1;
    }
    Ok(())
}
