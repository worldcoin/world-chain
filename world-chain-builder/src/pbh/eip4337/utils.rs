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

#[cfg(test)]
mod tests {
    use super::*;
}
