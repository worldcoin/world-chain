use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use alloy_network::Network;
use alloy_network::ReceiptResponse;
use alloy_primitives::hex;
use alloy_primitives::Bytes;
use alloy_primitives::B256;
use alloy_provider::PendingTransactionBuilder;
use alloy_provider::Provider;
use alloy_rpc_types_eth::erc4337::TransactionConditional;
use eyre::eyre::Result;
use futures::stream;
use futures::StreamExt;
use futures::TryStreamExt;
use tokio::time::interval;
use tokio::time::sleep;
use tracing::debug;
use tracing::info;
use world_chain_test::bindings::IEntryPoint::PackedUserOperation;
use world_chain_test::utils::RpcUserOperationByHash;
use world_chain_test::utils::RpcUserOperationV0_7;
use world_chain_test::DEVNET_ENTRYPOINT;
use world_chain_test::PBH_DEV_SIGNATURE_AGGREGATOR;

use crate::run_command;

const CONCURRENCY_LIMIT: usize = 50;

/// `eth_sendUserOperation` test cases
pub async fn user_ops_test<N, P>(
    bundler_provider: Arc<P>,
    builder_provider: Arc<P>,
    user_operations: Vec<PackedUserOperation>,
) -> Result<()>
where
    N: Network,
    P: Provider<N>,
{
    let start = Instant::now();
    stream::iter(user_operations.iter().enumerate())
        .map(Ok)
        .try_for_each_concurrent(CONCURRENCY_LIMIT, move |(index, uo)| {
            let bundler_provider = bundler_provider.clone();
            let builder_provider = builder_provider.clone();
            async move {
                let uo: RpcUserOperationV0_7 = (uo.clone(), Some(PBH_DEV_SIGNATURE_AGGREGATOR)).into();
                let hash: B256 = bundler_provider.raw_request(
                    Cow::Borrowed("eth_sendUserOperation"),
                    (uo, DEVNET_ENTRYPOINT),
                )
                .await?;

                // Fetch the Transaction by hash
                let max_retries = 100;
                let mut tries = 0;
                loop {
                    if tries >= max_retries {
                        panic!("User Operation not included in a Transaction after {max_retries} retries");
                    }
                    // Check if the User Operation has been included in a Transaction
                    let resp: RpcUserOperationByHash = bundler_provider
                        .raw_request(
                            Cow::Borrowed("eth_getUserOperationByHash"),
                            (hash,),
                        )
                        .await?;

                    if let Some(transaction_hash) = resp.transaction_hash {
                        debug!(target: "tests::user_ops_test", %index, ?transaction_hash, "User Operation Included in Transaction");
                        // Fetch the Transaction Receipt from the builder
                        let receipt = builder_provider.get_transaction_by_hash(transaction_hash).await?;
                        assert!(receipt.is_some_and(|receipt| {
                            debug!(target: "tests::user_ops_test", %index, ?receipt, "Transaction Receipt Received");
                            true
                        }));

                        break;
                    }

                    tries += 1;
                    sleep(Duration::from_secs(2)).await;
                }
                Ok::<(), eyre::Report>(())
            }
        })
        .await?;

    info!(duration = %start.elapsed().as_secs_f64(), total = %user_operations.len(), "All PBH UserOperations Processed");

    Ok(())
}

/// Sends a high volume of transactions to the builder concurrently.
pub async fn load_test<N, P>(
    tx_proxy: Arc<P>,
    builder_provider: Arc<P>,
    transactions: &[Bytes],
) -> Result<()>
where
    N: Network,
    P: Provider<N>,
{
    let start = Instant::now();
    stream::iter(transactions.iter().enumerate())
        .map(Ok)
        .try_for_each_concurrent(CONCURRENCY_LIMIT, move |(index, tx)| {
            let tx_proxy = tx_proxy.clone();
            let builder_provider = builder_provider.clone();
            async move {
                let tx = tx_proxy.send_raw_transaction(tx).await?;
                let hash = *tx.tx_hash();
                let receipt = poll_transaction_receipt(builder_provider, *tx.tx_hash()).await;
                assert!(receipt.is_ok());
                debug!(
                    target: "tests::load_test",
                    receipt = ?receipt.unwrap(),
                    hash = ?hash,
                    index = index,
                    "Transaction Receipt Received"
                );

                Ok::<(), eyre::Report>(())
            }
        })
        .await?;

    info!(target: "tests::load_test", duration = %start.elapsed().as_secs_f64(), total = %transactions.len(), "All PBH Transactions Processed");

    Ok(())
}

/// Asserts that the chain continues to advance in the case when the world-chain service is MIA.
pub async fn fallback_test<N, P>(sequencer_provider: P) -> Result<()>
where
    N: Network,
    P: Provider<N>,
{
    info!("Stopping world chain builder");
    run_command(
        "kurtosis",
        &[
            "service",
            "stop",
            "world-chain",
            "op-el-builder-2151908-1-custom-op-node-op-kurtosis",
        ],
        env!("CARGO_MANIFEST_DIR"),
    )
    .await?;

    sleep(Duration::from_secs(5)).await;

    info!("Getting block number from sequencer node");
    // Grab the latest block number
    let block_number = sequencer_provider.get_block_number().await?;

    let retries = 3;
    let mut tries = 0;
    loop {
        info!("Checking that chain has progressed ({tries}/{retries})");
        // Assert the chain has progressed
        let new_block_number = sequencer_provider.get_block_number().await?;
        if new_block_number > block_number {
            info!("Success!");
            break;
        }

        if tries >= retries {
            panic!("Chain did not progress after {retries} retries");
        }

        sleep(Duration::from_secs(2)).await;
        tries += 1;
    }
    Ok(())
}

/// `eth_sendRawTransactionConditional` test cases
pub async fn transact_conditional_test<N, P>(
    builder_provider: Arc<P>,
    transactions: &[Bytes],
) -> Result<()>
where
    N: Network,
    P: Provider<N>,
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
        block = %receipt.unwrap().block_number().unwrap_or_default(),
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

async fn send_raw_transaction_conditional<N, P>(
    tx: Bytes,
    conditions: TransactionConditional,
    provider: Arc<P>,
) -> Result<PendingTransactionBuilder<N>>
where
    N: Network,
    P: Provider<N>,
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

async fn poll_transaction_receipt<N, P>(
    provider: Arc<P>,
    tx_hash: B256,
) -> Result<<N as Network>::ReceiptResponse>
where
    N: Network,
    P: Provider<N>,
{
    let mut interval = interval(provider.client().poll_interval());

    loop {
        // try to fetch the receipt
        let receipt = provider.get_transaction_receipt(tx_hash).await?;
        if let Some(receipt) = receipt {
            return Ok(receipt);
        }

        interval.tick().await;
    }
}
