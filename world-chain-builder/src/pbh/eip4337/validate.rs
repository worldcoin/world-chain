use alloy_primitives::Address;

use crate::pool::tx::WorldChainPoolTransaction;

pub struct PbhEip4337ValidationContext {
    entry_point_address: Address,
    aggregator_address: Address,
}

pub fn validate_4337_pbh_tx<Tx: WorldChainPoolTransaction>(
    tx: &Tx,
    context: PbhEip4337ValidationContext,
) {
    todo!()
}
