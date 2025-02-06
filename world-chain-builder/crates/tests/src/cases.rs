use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use alloy_network::Network;
use alloy_primitives::hex;
use alloy_primitives::Bytes;
use alloy_provider::PendingTransactionBuilder;
use alloy_provider::Provider;
use alloy_rpc_types_eth::erc4337::TransactionConditional;
use alloy_rpc_types_eth::PackedUserOperation;
use alloy_transport::Transport;
use eyre::eyre::Result;
use futures::stream;
use futures::StreamExt;
use futures::TryStreamExt;
use rundler_types::v0_7::UserOperation;
use tokio::time::sleep;
use tracing::debug;
use tracing::info;
use world_chain_builder_pool::test_utils::PBH_TEST_SIGNATURE_AGGREGATOR;

use crate::run_command;

const CONCURRENCY_LIMIT: usize = 50;

// `eth_sendUserOperation` test cases
pub async fn user_ops_test<T, P>(
    bundler_provider: Arc<P>,
    user_operations: Vec<PackedUserOperation>,
) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    let uo = &user_operations[0];

    Ok(())
}

/// Sends a high volume of transactions to the builder concurrently.
pub async fn load_test<T, P>(builder_provider: Arc<P>, transactions: Vec<Bytes>) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    let start = Instant::now();
    let builder_provider_clone = builder_provider.clone();
    stream::iter(transactions.iter().enumerate())
        .map(Ok)
        .try_for_each_concurrent(CONCURRENCY_LIMIT, move |(index, tx)| {
            let builder_provider = builder_provider_clone.clone();

            async move {
                let tx = builder_provider.send_raw_transaction(tx).await?;
                let hash = *tx.tx_hash();
                debug!(hash = ?hash, index = index, "Transaction Sent");
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

    info!(duration = %start.elapsed().as_secs_f64(), total = %transactions.len(), "All PBH Transactions Processed");

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

/// `eth_sendRawTransactionConditional` test cases
pub async fn transact_conditional_test<T, P>(
    builder_provider: Arc<P>,
    transactions: &[Bytes],
) -> Result<()>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    let tx = &transactions[0];
    let latest = builder_provider.get_block_number().await?;
    let conditions = TransactionConditional {
        block_number_max: Some(latest + 10),
        block_number_min: Some(latest),
        ..Default::default()
    };

    info!(?conditions, "Sending Transaction with Conditional");
    let builder =
        send_raw_transaction_conditional(tx.clone(), conditions, builder_provider.clone()).await?;
    let hash = *builder.tx_hash();
    let receipt = builder.get_receipt().await;
    assert!(receipt.is_ok());
    info!(
        block = %receipt.unwrap().block_number.unwrap_or_default(),
        block_number_min = %latest,
        block_number_max = %latest + 2,
        hash = ?hash,
        "Transaction Receipt Received"
    );

    // Fails due to block_number_max
    let tx = &transactions[1];
    let conditions = TransactionConditional {
        block_number_max: Some(latest),
        block_number_min: Some(latest),
        ..Default::default()
    };

    assert!(
        send_raw_transaction_conditional(tx.clone(), conditions, builder_provider.clone())
            .await
            .is_err()
    );
    Ok(())
}

async fn send_raw_transaction_conditional<T, N, P>(
    tx: Bytes,
    conditions: TransactionConditional,
    provider: Arc<P>,
) -> Result<PendingTransactionBuilder<T, N>>
where
    N: Network,
    T: Transport + Clone,
    P: Provider<T, N>,
{
    let rlp_hex = hex::encode_prefixed(tx);
    let tx_hash = provider
        .client()
        .request("eth_sendRawTransactionConditional", (rlp_hex, conditions))
        .await?;

    Ok(PendingTransactionBuilder::new(
        provider.root().clone(),
        tx_hash,
    ))
}
