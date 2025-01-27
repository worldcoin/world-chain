use alloy_primitives::keccak256;
use alloy_sol_types::SolValue;
use semaphore::{hash_to_field, Field};

use crate::bindings::IEntryPoint::PackedUserOperation;

pub fn hash_user_op(user_op: &PackedUserOperation) -> Field {
    let keccak_hash = keccak256(SolValue::abi_encode_packed(&(
        &user_op.sender,
        &user_op.nonce,
        &user_op.callData,
    )));

    hash_to_field(keccak_hash.as_slice())
}
