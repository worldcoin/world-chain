use alloy_primitives::{Address, Bytes, B256, U128, U256, U64, U8};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RpcUserOperationByHash {
    /// The full user operation
    pub user_operation: RpcUserOperation,
    /// The entry point address this operation was sent to
    pub entry_point: RpcAddress,
    /// The number of the block this operation was included in
    pub block_number: Option<U256>,
    /// The hash of the block this operation was included in
    pub block_hash: Option<B256>,
    /// The hash of the transaction this operation was included in
    pub transaction_hash: Option<B256>,
}

/// authorization tuple for 7702 txn support
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RpcEip7702Auth {
    /// The chain ID of the authorization.
    pub chain_id: U64,
    /// The address of the authorization.
    pub address: Address,
    /// The nonce for the authorization.
    pub nonce: U64,
    /// signed authorizzation tuple.
    pub y_parity: U8,
    /// signed authorizzation tuple.
    pub r: U256,
    /// signed authorizzation tuple.
    pub s: U256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcAddress(Address);

impl Serialize for RpcAddress {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_checksum(None))
    }
}

impl<'de> Deserialize<'de> for RpcAddress {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let address = Address::deserialize(deserializer)?;
        Ok(RpcAddress(address))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum RpcUserOperation {
    V0_6(RpcUserOperationV0_6),
    V0_7(RpcUserOperationV0_7),
}

/// User operation definition for RPC
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcUserOperationV0_6 {
    sender: RpcAddress,
    nonce: U256,
    init_code: Bytes,
    call_data: Bytes,
    call_gas_limit: U128,
    verification_gas_limit: U128,
    pre_verification_gas: U128,
    max_fee_per_gas: U128,
    max_priority_fee_per_gas: U128,
    paymaster_and_data: Bytes,
    signature: Bytes,
    #[serde(skip_serializing_if = "Option::is_none")]
    eip7702_auth: Option<RpcEip7702Auth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aggregator: Option<Address>,
}

/// User operation definition for RPC inputs
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RpcUserOperationV0_7 {
    sender: Address,
    nonce: U256,
    call_data: Bytes,
    call_gas_limit: U128,
    verification_gas_limit: U128,
    pre_verification_gas: U256,
    max_priority_fee_per_gas: U128,
    max_fee_per_gas: U128,
    #[serde(skip_serializing_if = "Option::is_none")]
    factory: Option<Address>,
    #[serde(skip_serializing_if = "Option::is_none")]
    factory_data: Option<Bytes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paymaster: Option<Address>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paymaster_verification_gas_limit: Option<U128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paymaster_post_op_gas_limit: Option<U128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paymaster_data: Option<Bytes>,
    signature: Bytes,
    #[serde(skip_serializing_if = "Option::is_none")]
    eip7702_auth: Option<RpcEip7702Auth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aggregator: Option<Address>,
}
