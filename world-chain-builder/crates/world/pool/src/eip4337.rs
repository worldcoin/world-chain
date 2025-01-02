use alloy_primitives::{keccak256, Address};
use alloy_sol_types::SolValue;
use semaphore::{hash_to_field, Field};

use crate::bindings::{IEntryPoint::PackedUserOperation, IMulticall3};

pub fn hash_user_op(user_op: &PackedUserOperation) -> Field {
    let keccak_hash = keccak256(SolValue::abi_encode_packed(&(
        &user_op.sender,
        &user_op.nonce,
        &user_op.callData,
    )));

    hash_to_field(keccak_hash.as_slice())
}

pub fn hash_pbh_multicall(msg_sender: Address, calls: Vec<IMulticall3::Call3>) -> Field {
    let keccak_hash = keccak256(SolValue::abi_encode_packed(&(&msg_sender, calls)));

    hash_to_field(keccak_hash.as_slice())
}
