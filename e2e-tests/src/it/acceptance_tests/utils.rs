use std::time::{Duration, Instant};

use alloy_eips::BlockId;
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder, RootProvider};
use alloy_rpc_types::{TransactionInput, TransactionRequest};
use alloy_rpc_types_eth::TransactionReceipt;
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::{Context, bail, ensure};
use tokio::time::sleep;
use tracing::info;

pub(super) struct L2TxSender {
    provider: DynProvider,
    fee_caps: FeeCaps,
    tx_timeout: Duration,
    tx_poll_interval: Duration,
}

struct FeeCaps {
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
}

impl L2TxSender {
    pub(super) async fn new(
        provider: RootProvider,
        l2_key: PrivateKeySigner,
        tx_timeout: Duration,
        tx_poll_interval: Duration,
    ) -> eyre::Result<Self> {
        let provider = ProviderBuilder::new()
            .wallet(l2_key)
            .connect_provider(provider)
            .erased();
        let max_priority_fee_per_gas = provider
            .get_max_priority_fee_per_gas()
            .await
            .wrap_err("failed to fetch acceptance tx priority fee")?;
        let max_fee_per_gas = provider
            .get_gas_price()
            .await
            .wrap_err("failed to fetch acceptance tx gas price")?
            .max(max_priority_fee_per_gas);

        Ok(Self {
            provider,
            fee_caps: FeeCaps {
                max_fee_per_gas,
                max_priority_fee_per_gas,
            },
            tx_timeout,
            tx_poll_interval,
        })
    }

    pub(super) async fn expect_receipt_status(
        &mut self,
        label: &str,
        to: Option<Address>,
        data: Bytes,
        gas_limit: u64,
        expected_success: bool,
    ) -> eyre::Result<TransactionReceipt> {
        let request = self.transaction_request(to, data, gas_limit);
        let pending_tx = self
            .provider
            .send_transaction(request)
            .await
            .with_context(|| format!("{label}: failed to submit transaction"))?;
        let hash = *pending_tx.tx_hash();

        let receipt = wait_for_transaction_receipt(
            &self.provider,
            label,
            hash,
            self.tx_timeout,
            self.tx_poll_interval,
        )
        .await?;
        ensure!(
            receipt.status() == expected_success,
            "{label}: expected receipt success={expected_success}, got success={} (block={:?}, tx={hash:?})",
            receipt.status(),
            receipt.block_number
        );
        info!(
            block = ?receipt.block_number,
            tx = ?hash,
            success = receipt.status(),
            "{label}"
        );

        Ok(receipt)
    }

    pub(super) async fn expect_submission_rejected(
        &mut self,
        label: &str,
        to: Option<Address>,
        data: Bytes,
        gas_limit: u64,
    ) -> eyre::Result<()> {
        let request = self.transaction_request(to, data, gas_limit);

        match self.provider.send_transaction(request).await {
            Ok(pending_tx) => {
                let hash = *pending_tx.tx_hash();
                let receipt = wait_for_transaction_receipt(
                    &self.provider,
                    label,
                    hash,
                    self.tx_timeout,
                    self.tx_poll_interval,
                )
                .await?;
                bail!(
                    "{label}: expected submission rejection for tx {hash:?}, got receipt success={} in block {:?}",
                    receipt.status(),
                    receipt.block_number
                )
            }
            Err(err) => {
                info!(
                    err = %err,
                    "{label}: transaction rejected as expected"
                );
                Ok(())
            }
        }
    }

    pub(super) async fn code_at_block(
        &self,
        label: &str,
        address: Address,
        block_hash: B256,
    ) -> eyre::Result<Bytes> {
        wait_for_code_at_block(
            &self.provider,
            label,
            address,
            block_hash,
            self.tx_timeout,
            self.tx_poll_interval,
        )
        .await
    }

    fn transaction_request(
        &self,
        to: Option<Address>,
        data: Bytes,
        gas_limit: u64,
    ) -> TransactionRequest {
        TransactionRequest {
            value: Some(U256::ZERO),
            to: Some(to.map_or(TxKind::Create, TxKind::Call)),
            gas: Some(gas_limit),
            max_fee_per_gas: Some(self.fee_caps.max_fee_per_gas),
            max_priority_fee_per_gas: Some(self.fee_caps.max_priority_fee_per_gas),
            input: TransactionInput {
                input: None,
                data: Some(data),
            },
            ..Default::default()
        }
    }
}

async fn wait_for_transaction_receipt(
    provider: &DynProvider,
    label: &str,
    hash: B256,
    timeout: Duration,
    poll_interval: Duration,
) -> eyre::Result<TransactionReceipt> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(receipt) = provider
            .get_transaction_receipt(hash)
            .await
            .with_context(|| format!("{label}: failed to fetch receipt for {hash:?}"))?
        {
            return Ok(receipt);
        }

        ensure!(
            Instant::now() < deadline,
            "{label}: timed out waiting for receipt for {hash:?}"
        );
        sleep(poll_interval).await;
    }
}

async fn wait_for_code_at_block(
    provider: &DynProvider,
    label: &str,
    address: Address,
    block_hash: B256,
    timeout: Duration,
    poll_interval: Duration,
) -> eyre::Result<Bytes> {
    let deadline = Instant::now() + timeout;
    loop {
        // Some RPC backends expose the receipt before block-hash state lookup is ready.
        match provider
            .get_code_at(address)
            .block_id(BlockId::hash(block_hash))
            .await
        {
            Ok(code) => return Ok(code),
            Err(err) => {
                ensure!(
                    Instant::now() < deadline,
                    "{label}: timed out fetching code for {address} at block {block_hash:?}: {err}"
                );
                sleep(poll_interval).await;
            }
        }
    }
}
