use alloy_primitives::Address;
use alloy_sol_types::SolCall;

use super::bindings::IEntryPoint;
use crate::pool::tx::WorldChainPoolTransaction;

/// Checks if a given transaction is an EIP-4337 PBH bundle
///
/// The conditions for that are:
/// 1. Tx is sent to the EntryPoint contract
/// 2. It's an `handleAggregatedOps` function call
/// 3. All aggregated ops are using the pbh aggregator
///
/// On success returns
pub fn is_eip4337_pbh_bundle<Tx: WorldChainPoolTransaction>(
    tx: &Tx,
    entry_point_address: Address,
    pbh_aggregator_address: Address,
) -> Option<IEntryPoint::handleAggregatedOpsCall> {
    let Some(to_address) = tx.to() else {
        return None;
    };

    if to_address != entry_point_address {
        return None;
    }

    if tx.input().len() <= 3 {
        return None;
    }

    if !tx
        .input()
        .starts_with(&IEntryPoint::handleAggregatedOpsCall::SELECTOR)
    {
        return None;
    }

    // TODO: Boolean args is `validate`. Can it be `false`?
    let Ok(decoded) = IEntryPoint::handleAggregatedOpsCall::abi_decode(tx.input(), true) else {
        return None;
    };

    let are_aggregators_valid = decoded
        ._0
        .iter()
        // TODO: Why do I have to double deref?!?
        .all(|per_aggregator| **per_aggregator.aggregator == **pbh_aggregator_address);

    if are_aggregators_valid {
        Some(decoded)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
}
