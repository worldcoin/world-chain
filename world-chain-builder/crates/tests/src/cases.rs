use std::sync::Arc;

use crate::fixtures::PBHFixture;
use alloy_primitives::Bytes;
use alloy_provider::Provider;
use alloy_transport::Transport;
use eyre::eyre::Result;
use futures::stream;
use serde::{Deserialize, Serialize};

const PBH_FIXTURE: &str = include_str!("../../../../devnet/fixtures/fixture.json");

const CONCURRENCY_LIMIT: usize = 50;

/// Asserts that the world-chain-builder payload is built correctly with a set of PBH transactions.
pub async fn assert_build<T, P>(builder_provider: Arc<P>) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    let fixture = serde_json::from_str::<PBHFixture>(PBH_FIXTURE)?;
    let num_transactions = fixture.fixture.len();
    let half = num_transactions / 2;
    let builder_provider_clone = builder_provider.clone();
    stream::iter(fixture.fixture.iter().enumerate())
        .map(Ok)
        .try_for_each_concurrent(CONCURRENCY_LIMIT, move |(index, transaction)| {
            let builder_provider = builder_provider_clone.clone();
            async move {
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
                    PendingTransactionBuilder::new(builder_provider.root().clone(), tx_hash)
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
    run_command(
        "kurtosis",
        &[
            "service",
            "stop",
            "world-chain",
            "wc-admin-world-chain-builder",
        ],
    )?;
    sleep(Duration::from_secs(5)).await;

    // Grab the latest block number
    let block_number = sequencer_provider.get_block_number().await?;
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
