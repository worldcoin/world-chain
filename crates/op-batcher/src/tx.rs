//! Custom transaction filler with a fallback gas limit and a 3/2 safety margin.
//!
//! Copied from the proposer crate (`world-chain-exex::tx`). Used by the L1
//! provider's filler stack. For the batcher's calldata path the driver sets the
//! gas limit directly from the EIP-7623 floor, but this filler covers any other
//! L1 call (e.g. cancellation txs) and keeps the provider stack identical to the
//! proposer.

use alloy_network::{Network, TransactionBuilder};
use alloy_provider::{
    Provider, SendableTx,
    fillers::{FillerControlFlow, TxFiller},
};
use alloy_transport::{RpcError, TransportResult};

/// Fallback gas limit used when `eth_estimateGas` returns an execution error.
pub const GAS_ESTIMATION_FALLBACK: u64 = 500_000;

const GAS_ESTIMATION_MARGIN_NUMERATOR: u64 = 3;
const GAS_ESTIMATION_MARGIN_DENOMINATOR: u64 = 2;

/// A transaction filler that populates missing gas limits via
/// `eth_estimateGas`, with a fallback when estimation fails.
#[derive(Clone, Copy, Debug, Default)]
pub struct GasEstimateWithFallbackFiller;

impl GasEstimateWithFallbackFiller {
    const fn apply_margin(estimate: u64) -> u64 {
        estimate.saturating_mul(GAS_ESTIMATION_MARGIN_NUMERATOR) / GAS_ESTIMATION_MARGIN_DENOMINATOR
    }
}

impl<N> TxFiller<N> for GasEstimateWithFallbackFiller
where
    N: Network,
    N::TransactionRequest: TransactionBuilder,
{
    type Fillable = u64;

    fn status(&self, tx: &N::TransactionRequest) -> FillerControlFlow {
        if tx.gas_limit().is_some() {
            FillerControlFlow::Finished
        } else {
            FillerControlFlow::Ready
        }
    }

    fn fill_sync(&self, _tx: &mut SendableTx<N>) {}

    async fn prepare<P>(
        &self,
        provider: &P,
        tx: &N::TransactionRequest,
    ) -> TransportResult<Self::Fillable>
    where
        P: Provider<N>,
    {
        let gas_limit = match provider.estimate_gas(tx.clone()).await {
            Ok(estimate) => Self::apply_margin(estimate),
            Err(RpcError::ErrorResp(error)) => {
                tracing::warn!(
                    %error,
                    gas_limit = GAS_ESTIMATION_FALLBACK,
                    "eth_estimateGas returned an execution error, \
                     transaction will likely revert — using fallback gas limit"
                );
                GAS_ESTIMATION_FALLBACK
            }
            Err(error) => return Err(error),
        };

        Ok(gas_limit)
    }

    async fn fill(
        &self,
        gas_limit: Self::Fillable,
        mut tx: SendableTx<N>,
    ) -> TransportResult<SendableTx<N>> {
        if let Some(builder) = tx.as_mut_builder() {
            builder.set_gas_limit(gas_limit);
        }
        Ok(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_network::Ethereum;

    #[test]
    fn applies_three_halves_margin() {
        assert_eq!(GasEstimateWithFallbackFiller::apply_margin(100), 150);
    }

    #[test]
    fn status_finished_when_gas_limit_set() {
        use alloy_rpc_types::TransactionRequest;
        let tx = TransactionRequest::default().with_gas_limit(21_000);
        assert_eq!(
            <GasEstimateWithFallbackFiller as TxFiller<Ethereum>>::status(
                &GasEstimateWithFallbackFiller,
                &tx
            ),
            FillerControlFlow::Finished
        );
    }
}
