//! Payload related types

use std::{fmt::Debug, sync::Arc};

use alloy_consensus::{Block, BlockHeader};
use alloy_eips::{
    eip1559::BaseFeeParams, eip2718::Decodable2718, eip4895::Withdrawals, eip7685::Requests,
};
use alloy_primitives::{Address, B64, B256, Bytes, U256, keccak256};
use alloy_rlp::Encodable;
use alloy_rpc_types_engine::{
    BlobsBundleV1, ExecutionPayloadEnvelopeV2, ExecutionPayloadFieldV2, ExecutionPayloadV1,
    ExecutionPayloadV3, PayloadId,
};
use op_alloy_consensus::{EIP1559ParamError, encode_holocene_extra_data, encode_jovian_extra_data};
use op_alloy_rpc_types_engine::{
    OpExecutionPayloadEnvelopeV3, OpExecutionPayloadEnvelopeV4, OpExecutionPayloadV4,
};
use reth_chainspec::EthChainSpec;
use reth_optimism_evm::OpNextBlockEnvAttributes;
use reth_optimism_forks::OpHardforks;
use reth_payload_builder_primitives::PayloadBuilderError;
use reth_payload_primitives::{BuildNextEnv, BuiltPayload, BuiltPayloadExecutedBlock};
use reth_primitives_traits::{
    NodePrimitives, SealedBlock, SealedHeader, SignedTransaction, WithEncoded,
};

/// Re-export for use in downstream arguments.
pub use op_alloy_rpc_types_engine::{OpExecutionData, OpPayloadAttributes};
use reth_optimism_primitives::OpPrimitives;
use reth_payload_primitives::EngineApiMessageVersion;

/// Newtype wrapper around [`OpExecutionData`] to implement
/// [`reth_payload_primitives::ExecutionPayload`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct OpExecData(pub OpExecutionData);

impl core::ops::Deref for OpExecData {
    type Target = OpExecutionData;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<OpExecutionData> for OpExecData {
    fn from(data: OpExecutionData) -> Self {
        Self(data)
    }
}

impl reth_payload_primitives::ExecutionPayload for OpExecData {
    fn parent_hash(&self) -> B256 {
        self.0.payload.parent_hash()
    }

    fn block_hash(&self) -> B256 {
        self.0.payload.block_hash()
    }

    fn block_number(&self) -> u64 {
        self.0.payload.block_number()
    }

    fn withdrawals(&self) -> Option<&Vec<alloy_eips::eip4895::Withdrawal>> {
        self.0.withdrawals()
    }

    fn block_access_list(&self) -> Option<&Bytes> {
        None
    }

    fn parent_beacon_block_root(&self) -> Option<B256> {
        self.0.sidecar.parent_beacon_block_root()
    }

    fn timestamp(&self) -> u64 {
        self.0.payload.timestamp()
    }

    fn gas_used(&self) -> u64 {
        self.0.payload.as_v1().gas_used
    }

    fn transaction_count(&self) -> usize {
        self.0.payload.as_v1().transactions.len()
    }
}

/// Newtype wrapper around [`OpPayloadAttributes`] to implement
/// [`reth_payload_primitives::PayloadAttributes`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct OpPayloadAttrs(pub OpPayloadAttributes);

impl core::ops::Deref for OpPayloadAttrs {
    type Target = OpPayloadAttributes;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<OpPayloadAttributes> for OpPayloadAttrs {
    fn from(attrs: OpPayloadAttributes) -> Self {
        Self(attrs)
    }
}

impl From<alloy_rpc_types_engine::PayloadAttributes> for OpPayloadAttrs {
    fn from(attrs: alloy_rpc_types_engine::PayloadAttributes) -> Self {
        Self(OpPayloadAttributes {
            payload_attributes: attrs,
            transactions: None,
            no_tx_pool: None,
            gas_limit: None,
            eip_1559_params: None,
            min_base_fee: None,
        })
    }
}

impl reth_payload_primitives::PayloadAttributes for OpPayloadAttrs {
    fn payload_id(&self, parent_hash: &B256) -> PayloadId {
        // Use the default engine API message version for computing the payload id.
        payload_id_optimism(parent_hash, &self.0, EngineApiMessageVersion::default() as u8)
    }

    fn timestamp(&self) -> u64 {
        self.0.payload_attributes.timestamp
    }

    fn withdrawals(&self) -> Option<&Vec<alloy_eips::eip4895::Withdrawal>> {
        self.0.payload_attributes.withdrawals.as_ref()
    }

    fn parent_beacon_block_root(&self) -> Option<B256> {
        self.0.payload_attributes.parent_beacon_block_root
    }
}

/// Optimism Payload Builder Attributes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpPayloadBuilderAttributes<T> {
    /// Id of the payload
    pub id: PayloadId,
    /// Parent block to build the payload on top
    pub parent: B256,
    /// Unix timestamp for the generated payload
    pub timestamp: u64,
    /// Address of the recipient for collecting transaction fee
    pub suggested_fee_recipient: Address,
    /// Randomness value for the generated payload
    pub prev_randao: B256,
    /// Withdrawals for the generated payload
    pub withdrawals: Withdrawals,
    /// Root of the parent beacon block
    pub parent_beacon_block_root: Option<B256>,
    /// `NoTxPool` option for the generated payload
    pub no_tx_pool: bool,
    /// Decoded transactions and the original EIP-2718 encoded bytes as received in the payload
    /// attributes.
    pub transactions: Vec<WithEncoded<T>>,
    /// The gas limit for the generated payload
    pub gas_limit: Option<u64>,
    /// EIP-1559 parameters for the generated payload
    pub eip_1559_params: Option<B64>,
    /// Min base fee for the generated payload (only available post-Jovian)
    pub min_base_fee: Option<u64>,
}

// Manual serde implementations required because:
// - Serialize: the `transactions` field must serialize as encoded bytes (Vec<&Bytes>), not as the
//   inner `WithEncoded<T>` type, so #[derive(Serialize)] can't be used.
// - Deserialize: intentionally errors out since this builder type is never deserialized from the
//   wire (the engine API uses OpPayloadAttributes, not this builder type).
impl<T> serde::Serialize for OpPayloadBuilderAttributes<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("OpPayloadBuilderAttributes", 11)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("parent", &self.parent)?;
        s.serialize_field("timestamp", &self.timestamp)?;
        s.serialize_field("suggestedFeeRecipient", &self.suggested_fee_recipient)?;
        s.serialize_field("prevRandao", &self.prev_randao)?;
        s.serialize_field("withdrawals", &self.withdrawals)?;
        s.serialize_field("parentBeaconBlockRoot", &self.parent_beacon_block_root)?;
        s.serialize_field("noTxPool", &self.no_tx_pool)?;
        // Serialize transactions as encoded bytes
        let tx_bytes: Vec<&alloy_primitives::Bytes> =
            self.transactions.iter().map(|t| t.encoded_bytes()).collect();
        s.serialize_field("transactions", &tx_bytes)?;
        s.serialize_field("gasLimit", &self.gas_limit)?;
        s.serialize_field("eip1559Params", &self.eip_1559_params)?;
        s.serialize_field("minBaseFee", &self.min_base_fee)?;
        s.end()
    }
}

impl<'de, T: Decodable2718 + Send + Sync + Debug + Unpin + 'static> serde::Deserialize<'de>
    for OpPayloadBuilderAttributes<T>
{
    fn deserialize<D: serde::Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
        // This type is never deserialized in practice; the PayloadAttributes trait bound
        // requires DeserializeOwned but the engine API uses OpPayloadAttributes for
        // deserialization, not OpPayloadBuilderAttributes.
        Err(serde::de::Error::custom(
            "OpPayloadBuilderAttributes cannot be deserialized directly; \
             use OpPayloadAttributes instead",
        ))
    }
}

impl<T: Decodable2718 + Send + Sync + Debug + Clone + Unpin + 'static>
    reth_payload_primitives::PayloadAttributes for OpPayloadBuilderAttributes<T>
{
    fn payload_id(&self, _parent_hash: &B256) -> PayloadId {
        // The payload ID was already computed during construction via try_new.
        self.id
    }

    fn timestamp(&self) -> u64 {
        self.timestamp
    }

    fn withdrawals(&self) -> Option<&Vec<alloy_eips::eip4895::Withdrawal>> {
        Some(&self.withdrawals.0)
    }

    fn parent_beacon_block_root(&self) -> Option<B256> {
        self.parent_beacon_block_root
    }
}

impl<T> Default for OpPayloadBuilderAttributes<T> {
    fn default() -> Self {
        Self {
            id: PayloadId::default(),
            parent: B256::default(),
            timestamp: 0,
            suggested_fee_recipient: Address::default(),
            prev_randao: B256::default(),
            withdrawals: Default::default(),
            parent_beacon_block_root: None,
            no_tx_pool: false,
            transactions: Vec::new(),
            gas_limit: None,
            eip_1559_params: None,
            min_base_fee: None,
        }
    }
}

impl<T> OpPayloadBuilderAttributes<T> {
    /// Extracts the extra data parameters post-Holocene hardfork.
    /// In Holocene, those parameters are the EIP-1559 base fee parameters.
    pub fn get_holocene_extra_data(
        &self,
        default_base_fee_params: BaseFeeParams,
    ) -> Result<Bytes, EIP1559ParamError> {
        self.eip_1559_params
            .map(|params| encode_holocene_extra_data(params, default_base_fee_params))
            .ok_or(EIP1559ParamError::NoEIP1559Params)?
    }

    /// Extracts the extra data parameters post-Jovian hardfork.
    /// Those parameters are the EIP-1559 parameters from Holocene and the minimum base fee.
    pub fn get_jovian_extra_data(
        &self,
        default_base_fee_params: BaseFeeParams,
    ) -> Result<Bytes, EIP1559ParamError> {
        let min_base_fee = self.min_base_fee.ok_or(EIP1559ParamError::MinBaseFeeNotSet)?;
        self.eip_1559_params
            .map(|params| encode_jovian_extra_data(params, default_base_fee_params, min_base_fee))
            .ok_or(EIP1559ParamError::NoEIP1559Params)?
    }
}

impl<T: Decodable2718 + Send + Sync + Debug + Unpin + 'static> OpPayloadBuilderAttributes<T> {
    /// Creates a new payload builder for the given parent block and the attributes.
    ///
    /// Derives the unique [`PayloadId`] for the given parent and attributes
    pub fn try_new(
        parent: B256,
        attributes: OpPayloadAttributes,
        version: u8,
    ) -> Result<Self, alloy_rlp::Error> {
        let id = payload_id_optimism(&parent, &attributes, version);

        let transactions = attributes
            .transactions
            .unwrap_or_default()
            .into_iter()
            .map(|data| {
                Decodable2718::decode_2718_exact(data.as_ref()).map(|tx| WithEncoded::new(data, tx))
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            id,
            parent,
            timestamp: attributes.payload_attributes.timestamp,
            suggested_fee_recipient: attributes.payload_attributes.suggested_fee_recipient,
            prev_randao: attributes.payload_attributes.prev_randao,
            withdrawals: attributes.payload_attributes.withdrawals.unwrap_or_default().into(),
            parent_beacon_block_root: attributes.payload_attributes.parent_beacon_block_root,
            no_tx_pool: attributes.no_tx_pool.unwrap_or_default(),
            transactions,
            gas_limit: attributes.gas_limit,
            eip_1559_params: attributes.eip_1559_params,
            min_base_fee: attributes.min_base_fee,
        })
    }

    /// Creates a new payload builder from RPC attributes with a pre-computed payload ID.
    ///
    /// This is used when the payload ID has already been computed (e.g., by the payload service)
    /// and we just need to decode the transaction bytes.
    pub fn from_rpc_attrs(
        parent: B256,
        id: PayloadId,
        attributes: OpPayloadAttributes,
    ) -> Result<Self, alloy_rlp::Error> {
        let transactions = attributes
            .transactions
            .unwrap_or_default()
            .into_iter()
            .map(|data| {
                Decodable2718::decode_2718_exact(data.as_ref()).map(|tx| WithEncoded::new(data, tx))
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            id,
            parent,
            timestamp: attributes.payload_attributes.timestamp,
            suggested_fee_recipient: attributes.payload_attributes.suggested_fee_recipient,
            prev_randao: attributes.payload_attributes.prev_randao,
            withdrawals: attributes.payload_attributes.withdrawals.unwrap_or_default().into(),
            parent_beacon_block_root: attributes.payload_attributes.parent_beacon_block_root,
            no_tx_pool: attributes.no_tx_pool.unwrap_or_default(),
            transactions,
            gas_limit: attributes.gas_limit,
            eip_1559_params: attributes.eip_1559_params,
            min_base_fee: attributes.min_base_fee,
        })
    }

    /// Returns the identifier of the payload.
    pub const fn payload_id(&self) -> PayloadId {
        self.id
    }

    /// Returns the parent block hash.
    pub const fn parent(&self) -> B256 {
        self.parent
    }

    /// Returns the timestamp for the payload.
    pub const fn timestamp(&self) -> u64 {
        self.timestamp
    }

    /// Returns the parent beacon block root.
    pub const fn parent_beacon_block_root(&self) -> Option<B256> {
        self.parent_beacon_block_root
    }

    /// Returns the suggested fee recipient.
    pub const fn suggested_fee_recipient(&self) -> Address {
        self.suggested_fee_recipient
    }

    /// Returns the prev randao value.
    pub const fn prev_randao(&self) -> B256 {
        self.prev_randao
    }

    /// Returns the withdrawals.
    pub const fn withdrawals(&self) -> &Withdrawals {
        &self.withdrawals
    }
}

/// Contains the built payload.
#[derive(Debug, Clone)]
pub struct OpBuiltPayload<N: NodePrimitives = OpPrimitives> {
    /// Identifier of the payload
    pub(crate) id: PayloadId,
    /// Sealed block
    pub(crate) block: Arc<SealedBlock<N::Block>>,
    /// Block execution data for the payload, if any.
    pub(crate) executed_block: Option<BuiltPayloadExecutedBlock<N>>,
    /// The fees of the block
    pub(crate) fees: U256,
}

// === impl BuiltPayload ===

impl<N: NodePrimitives> OpBuiltPayload<N> {
    /// Initializes the payload with the given initial block.
    pub const fn new(
        id: PayloadId,
        block: Arc<SealedBlock<N::Block>>,
        fees: U256,
        executed_block: Option<BuiltPayloadExecutedBlock<N>>,
    ) -> Self {
        Self { id, block, fees, executed_block }
    }

    /// Returns the identifier of the payload.
    pub const fn id(&self) -> PayloadId {
        self.id
    }

    /// Returns the built block(sealed)
    pub fn block(&self) -> &SealedBlock<N::Block> {
        &self.block
    }

    /// Fees of the block
    pub const fn fees(&self) -> U256 {
        self.fees
    }

    /// Converts the value into [`SealedBlock`].
    pub fn into_sealed_block(self) -> SealedBlock<N::Block> {
        Arc::unwrap_or_clone(self.block)
    }
}

impl<N: NodePrimitives> BuiltPayload for OpBuiltPayload<N> {
    type Primitives = N;

    fn block(&self) -> &SealedBlock<N::Block> {
        self.block()
    }

    fn fees(&self) -> U256 {
        self.fees
    }

    fn executed_block(&self) -> Option<BuiltPayloadExecutedBlock<N>> {
        self.executed_block.clone()
    }

    fn requests(&self) -> Option<Requests> {
        None
    }
}

// V1 engine_getPayloadV1 response
impl<T, N> From<OpBuiltPayload<N>> for ExecutionPayloadV1
where
    T: SignedTransaction,
    N: NodePrimitives<Block = Block<T>>,
{
    fn from(value: OpBuiltPayload<N>) -> Self {
        Self::from_block_unchecked(
            value.block().hash(),
            &Arc::unwrap_or_clone(value.block).into_block(),
        )
    }
}

// V2 engine_getPayloadV2 response
impl<T, N> From<OpBuiltPayload<N>> for ExecutionPayloadEnvelopeV2
where
    T: SignedTransaction,
    N: NodePrimitives<Block = Block<T>>,
{
    fn from(value: OpBuiltPayload<N>) -> Self {
        let OpBuiltPayload { block, fees, .. } = value;

        Self {
            block_value: fees,
            execution_payload: ExecutionPayloadFieldV2::from_block_unchecked(
                block.hash(),
                &Arc::unwrap_or_clone(block).into_block(),
            ),
        }
    }
}

impl<T, N> From<OpBuiltPayload<N>> for OpExecutionPayloadEnvelopeV3
where
    T: SignedTransaction,
    N: NodePrimitives<Block = Block<T>>,
{
    fn from(value: OpBuiltPayload<N>) -> Self {
        let OpBuiltPayload { block, fees, .. } = value;

        let parent_beacon_block_root = block.parent_beacon_block_root.unwrap_or_default();

        Self {
            execution_payload: ExecutionPayloadV3::from_block_unchecked(
                block.hash(),
                &Arc::unwrap_or_clone(block).into_block(),
            ),
            block_value: fees,
            // From the engine API spec:
            //
            // > Client software **MAY** use any heuristics to decide whether to set
            // `shouldOverrideBuilder` flag or not. If client software does not implement any
            // heuristic this flag **SHOULD** be set to `false`.
            //
            // Spec:
            // <https://github.com/ethereum/execution-apis/blob/fe8e13c288c592ec154ce25c534e26cb7ce0530d/src/engine/cancun.md#specification-2>
            should_override_builder: false,
            // No blobs for OP.
            blobs_bundle: BlobsBundleV1 { blobs: vec![], commitments: vec![], proofs: vec![] },
            parent_beacon_block_root,
        }
    }
}

impl<T, N> From<OpBuiltPayload<N>> for OpExecutionPayloadEnvelopeV4
where
    T: SignedTransaction,
    N: NodePrimitives<Block = Block<T>>,
{
    fn from(value: OpBuiltPayload<N>) -> Self {
        let OpBuiltPayload { block, fees, .. } = value;

        let parent_beacon_block_root = block.parent_beacon_block_root.unwrap_or_default();

        let l2_withdrawals_root = block.withdrawals_root.unwrap_or_default();
        let payload_v3 = ExecutionPayloadV3::from_block_unchecked(
            block.hash(),
            &Arc::unwrap_or_clone(block).into_block(),
        );

        Self {
            execution_payload: OpExecutionPayloadV4::from_v3_with_withdrawals_root(
                payload_v3,
                l2_withdrawals_root,
            ),
            block_value: fees,
            // From the engine API spec:
            //
            // > Client software **MAY** use any heuristics to decide whether to set
            // `shouldOverrideBuilder` flag or not. If client software does not implement any
            // heuristic this flag **SHOULD** be set to `false`.
            //
            // Spec:
            // <https://github.com/ethereum/execution-apis/blob/fe8e13c288c592ec154ce25c534e26cb7ce0530d/src/engine/cancun.md#specification-2>
            should_override_builder: false,
            // No blobs for OP.
            blobs_bundle: BlobsBundleV1 { blobs: vec![], commitments: vec![], proofs: vec![] },
            parent_beacon_block_root,
            execution_requests: vec![],
        }
    }
}

/// Generates the payload id for the configured payload from the [`OpPayloadAttributes`].
///
/// Returns an 8-byte identifier by hashing the payload components with sha256 hash.
///
/// Note: This must be updated whenever the [`OpPayloadAttributes`] changes for a hardfork.
/// See also <https://github.com/ethereum-optimism/op-geth/blob/d401af16f2dd94b010a72eaef10e07ac10b31931/miner/payload_building.go#L59-L59>
pub fn payload_id_optimism(
    parent: &B256,
    attributes: &OpPayloadAttributes,
    payload_version: u8,
) -> PayloadId {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(parent.as_slice());
    hasher.update(&attributes.payload_attributes.timestamp.to_be_bytes()[..]);
    hasher.update(attributes.payload_attributes.prev_randao.as_slice());
    hasher.update(attributes.payload_attributes.suggested_fee_recipient.as_slice());
    if let Some(withdrawals) = &attributes.payload_attributes.withdrawals {
        let mut buf = Vec::new();
        withdrawals.encode(&mut buf);
        hasher.update(buf);
    }

    if let Some(parent_beacon_block) = attributes.payload_attributes.parent_beacon_block_root {
        hasher.update(parent_beacon_block);
    }

    let no_tx_pool = attributes.no_tx_pool.unwrap_or_default();
    if no_tx_pool || attributes.transactions.as_ref().is_some_and(|txs| !txs.is_empty()) {
        hasher.update([no_tx_pool as u8]);
        let txs_len = attributes.transactions.as_ref().map(|txs| txs.len()).unwrap_or_default();
        hasher.update(&txs_len.to_be_bytes()[..]);
        if let Some(txs) = &attributes.transactions {
            for tx in txs {
                // we have to just hash the bytes here because otherwise we would need to decode
                // the transactions here which really isn't ideal
                let tx_hash = keccak256(tx);
                // maybe we can try just taking the hash and not decoding
                hasher.update(tx_hash)
            }
        }
    }

    if let Some(gas_limit) = attributes.gas_limit {
        hasher.update(gas_limit.to_be_bytes());
    }

    if let Some(eip_1559_params) = attributes.eip_1559_params {
        hasher.update(eip_1559_params.as_slice());
    }

    if let Some(min_base_fee) = attributes.min_base_fee {
        hasher.update(min_base_fee.to_be_bytes());
    }

    let mut out = hasher.finalize();
    out[0] = payload_version;

    #[allow(deprecated)] // generic-array 0.14 deprecated
    PayloadId::new(out.as_slice()[..8].try_into().expect("sufficient length"))
}

impl<H, T, ChainSpec> BuildNextEnv<OpPayloadBuilderAttributes<T>, H, ChainSpec>
    for OpNextBlockEnvAttributes
where
    H: BlockHeader,
    T: SignedTransaction,
    ChainSpec: EthChainSpec + OpHardforks,
{
    fn build_next_env(
        attributes: &OpPayloadBuilderAttributes<T>,
        parent: &SealedHeader<H>,
        chain_spec: &ChainSpec,
    ) -> Result<Self, PayloadBuilderError> {
        let extra_data = if chain_spec.is_jovian_active_at_timestamp(attributes.timestamp) {
            attributes
                .get_jovian_extra_data(
                    chain_spec.base_fee_params_at_timestamp(attributes.timestamp),
                )
                .map_err(PayloadBuilderError::other)?
        } else if chain_spec.is_holocene_active_at_timestamp(attributes.timestamp) {
            attributes
                .get_holocene_extra_data(
                    chain_spec.base_fee_params_at_timestamp(attributes.timestamp),
                )
                .map_err(PayloadBuilderError::other)?
        } else {
            Default::default()
        };

        Ok(Self {
            timestamp: attributes.timestamp,
            suggested_fee_recipient: attributes.suggested_fee_recipient,
            prev_randao: attributes.prev_randao,
            gas_limit: attributes.gas_limit.unwrap_or_else(|| parent.gas_limit()),
            parent_beacon_block_root: attributes.parent_beacon_block_root,
            extra_data,
        })
    }
}
