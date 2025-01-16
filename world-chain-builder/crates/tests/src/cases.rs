use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::hex;
use alloy_primitives::Bytes;
use alloy_provider::PendingTransactionBuilder;
use alloy_provider::Provider;
use alloy_rpc_types_eth::erc4337::TransactionConditional;
use alloy_transport::Transport;
use eyre::eyre::Result;
use futures::stream;
use futures::StreamExt;
use futures::TryStreamExt;
use tokio::time::sleep;
use tracing::debug;

use crate::run_command;

const CONCURRENCY_LIMIT: usize = 50;

/// Asserts that the world-chain-builder payload is built correctly with a set of PBH transactions.
pub async fn ordering_test<T, P>(builder_provider: Arc<P>, fixture: Vec<Bytes>) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    let builder_provider_clone = builder_provider.clone();
    stream::iter(fixture.chunks(100).enumerate())
        .map(Ok)
        .try_for_each_concurrent(CONCURRENCY_LIMIT, move |(index, transactions)| {
            let builder_provider = builder_provider_clone.clone();
            async move {
                for transaction in transactions {
                    let tx = builder_provider.send_raw_transaction(transaction).await?;
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
pub async fn fallback_test<T, P>(sequencer_provider: P) -> Result<()>
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
        env!("CARGO_MANIFEST_DIR"),
    )
    .await?;

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

/// Spams the builder with 4000 transactions at once. 
/// This is to test the builder's ability to handle a large number of transactions.
pub async fn load_test<T, P>(_builder_provider: Arc<P>) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{ todo!() } 

/// `eth_sendRawTransactionConditional` test cases
pub async fn transact_conditional_test<T, P>(builder_provider: Arc<P>) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{ 
    // // Second half, use eth_sendRawTransactionConditional
    // let rlp_hex = hex::encode_prefixed(transaction);
    // let tx_hash = builder_provider
    //     .client()
    //     .request(
    //         "eth_sendRawTransactionConditional",
    //         (rlp_hex, TransactionConditional::default()),
    //     )
    //     .await?;
    // PendingTransactionBuilder::new(builder_provider.root().clone(), tx_hash)
    todo!()
}
