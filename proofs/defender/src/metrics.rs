//! Metrics definitions and helpers for the World Chain defender.

use alloy_primitives::{Address, utils::format_ether};
use alloy_provider::Provider;
use telemetry_batteries::reexports::metrics;
use tracing::warn;

/// Current L1 transaction-sending wallet balance in ETH.
pub const METRICS_WALLET_BALANCE_ETH: &str = "wallet.balance_eth";

/// Registers defender metric metadata with the active recorder.
pub fn describe_metrics() {
    metrics::describe_gauge!(
        METRICS_WALLET_BALANCE_ETH,
        metrics::Unit::Count,
        "Current L1 transaction-sending wallet balance in ETH."
    );
}

/// Refreshes the transaction-sending wallet's L1 balance.
pub async fn refresh_wallet_balance<P>(provider: &P, address: Address)
where
    P: Provider,
{
    let gauge = metrics::gauge!(METRICS_WALLET_BALANCE_ETH, "address" => address.to_string());
    match provider.get_balance(address).await {
        Ok(balance) => match format_ether(balance).parse::<f64>() {
            Ok(balance_eth) => gauge.set(balance_eth),
            Err(error) => warn!(%address, %error, "failed to convert wallet balance to ETH"),
        },
        Err(error) => warn!(%address, %error, "failed to fetch wallet balance"),
    }
}
