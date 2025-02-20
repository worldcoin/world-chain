use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_eips::{eip2718::Encodable2718, eip2930::AccessList};
use alloy_network::TxSigner;
use alloy_primitives::{
    aliases::U48, bytes, fixed_bytes, keccak256, Address, Bytes, ChainId, FixedBytes, TxKind, U256,
};
use alloy_primitives::{B256, U128, U64, U8};
use alloy_signer::SignerSync;
use alloy_signer_local::{coins_bip39::English, PrivateKeySigner};
use alloy_sol_types::SolValue;
use bon::builder;
use op_alloy_consensus::OpTypedTransaction;
use reth_optimism_node::txpool::OpPooledTransaction;
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::transaction::SignedTransactionIntoRecoveredExt;
use semaphore_rs::identity::Identity;
use semaphore_rs::poseidon_tree::LazyPoseidonTree;
use semaphore_rs::{hash_to_field, Field};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use std::{str::FromStr, sync::LazyLock};
use world_chain_builder_pbh::external_nullifier::{EncodedExternalNullifier, ExternalNullifier};
use world_chain_builder_pbh::payload::{PBHPayload as PbhPayload, Proof, TREE_DEPTH};

use crate::bindings::IEntryPoint::{self, PackedUserOperation, UserOpsPerAggregator};
use crate::bindings::IPBHEntryPoint::{self, PBHPayload};
use crate::bindings::{EncodedSafeOpStruct, IMulticall3};
use crate::{
    DEVNET_ENTRYPOINT, DEV_CHAIN_ID, MNEMONIC, PBH_DEV_ENTRYPOINT, PBH_DEV_SIGNATURE_AGGREGATOR,
    PBH_NONCE_KEY, TEST_MODULES, TEST_SAFES, WC_SEPOLIA_CHAIN_ID,
};

pub static TREE: LazyLock<LazyPoseidonTree> = LazyLock::new(|| {
    let mut tree = LazyPoseidonTree::new(TREE_DEPTH, Field::ZERO);
    let mut comms = vec![];
    for acc in 0..=5 {
        let identity = identity(acc);
        let commitment = identity.commitment();
        comms.push(commitment);
        tree = tree.update_with_mutation(acc as usize, &commitment);
    }

    tree.derived()
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InclusionProof {
    pub root: Field,
    pub proof: semaphore_rs::poseidon_tree::Proof,
}

pub fn signer(index: u32) -> PrivateKeySigner {
    alloy_signer_local::MnemonicBuilder::<English>::default()
        .phrase(MNEMONIC)
        .index(index)
        .expect("Failed to set index")
        .build()
        .expect("Failed to create signer")
}

pub fn account(index: u32) -> Address {
    let signer = signer(index);
    signer.address()
}

pub fn identity(index: u32) -> Identity {
    let mut secret = account(index).into_word().0;

    Identity::from_secret(&mut secret as &mut _, None)
}

pub fn tree_root() -> Field {
    TREE.root()
}

pub fn tree_inclusion_proof(acc: u32) -> semaphore_rs::poseidon_tree::Proof {
    TREE.proof(acc as usize)
}

pub fn nullifier_hash(acc: u32, external_nullifier: Field) -> Field {
    let identity = identity(acc);

    semaphore_rs::protocol::generate_nullifier_hash(&identity, external_nullifier)
}

pub fn semaphore_proof(
    acc: u32,
    ext_nullifier: Field,
    signal: Field,
) -> semaphore_rs::protocol::Proof {
    let identity = identity(acc);
    let incl_proof = tree_inclusion_proof(acc);

    semaphore_rs::protocol::generate_proof(&identity, &incl_proof, ext_nullifier, signal)
        .expect("Failed to generate semaphore proof")
}

#[builder]
pub fn eip1559(
    #[builder(default = 1)] chain_id: ChainId,
    #[builder(default = 0)] nonce: u64,
    #[builder(default = 150000)] gas_limit: u64,
    #[builder(default = 10000000)] // 0.1 GWEI
    max_fee_per_gas: u128,
    #[builder(default = 0)] max_priority_fee_per_gas: u128,
    #[builder(into)] to: TxKind,
    #[builder(default = U256::ZERO)] value: U256,
    #[builder(default)] access_list: AccessList,
    #[builder(into, default)] input: Bytes,
) -> TxEip1559 {
    TxEip1559 {
        chain_id,
        nonce,
        gas_limit,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        to,
        value,
        access_list,
        input,
    }
}

pub async fn eth_tx(acc: u32, mut tx: TxEip1559) -> OpPooledTransaction {
    let signer = signer(acc);
    let signature = signer
        .sign_transaction(&mut tx)
        .await
        .expect("Failed to sign transaction");
    let op_tx: OpTypedTransaction = tx.clone().into();
    let tx_signed = OpTransactionSigned::new(op_tx, signature, tx.signature_hash());
    let pooled = OpPooledTransaction::new(
        tx_signed.clone().into_recovered_unchecked().unwrap(),
        tx_signed.eip1559().unwrap().size(),
    );
    pooled
}

pub async fn raw_tx(acc: u32, mut tx: TxEip1559) -> Bytes {
    let signer = signer(acc);
    let signature = signer
        .sign_transaction(&mut tx)
        .await
        .expect("Failed to sign transaction");
    let tx_signed = OpTransactionSigned::new(tx.clone().into(), signature, tx.signature_hash());
    let mut buff = vec![];
    tx_signed.encode_2718(&mut buff);
    buff.into()
}

#[builder]
pub fn user_op(
    acc: u32,
    #[builder(into, default = U256::ZERO)] nonce: U256,
    #[builder(default = ExternalNullifier::v1(12, 2024, 0))] external_nullifier: ExternalNullifier,
    #[builder(default = Bytes::default())] init_code: Bytes,
    #[builder(default = bytes!("7bb3742800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"))]
    // abi.encodeCall(Safe4337Module.executeUserOp, (address(0), 0, new bytes(0), 0))
    calldata: Bytes,
    #[builder(default = fixed_bytes!("000000000000000000000000000fffd30000000000000000000000000000C350"))]
    account_gas_limits: FixedBytes<32>,
    #[builder(default = U256::from(500836))] pre_verification_gas: U256,
    #[builder(default = fixed_bytes!("0000000000000000000000003B9ACA0000000000000000000000000073140B60"))]
    gas_fees: FixedBytes<32>,
    #[builder(default = Bytes::default())] paymaster_and_data: Bytes,
) -> (IEntryPoint::PackedUserOperation, PbhPayload) {
    let rand_key = U256::from_be_bytes(Address::random().into_word().0) << 32;
    let mut user_op = PackedUserOperation {
        sender: TEST_SAFES[acc as usize],
        nonce: ((rand_key | U256::from(PBH_NONCE_KEY)) << 64) | nonce,
        initCode: init_code,
        callData: calldata,
        accountGasLimits: account_gas_limits,
        preVerificationGas: pre_verification_gas,
        gasFees: gas_fees,
        paymasterAndData: paymaster_and_data,
        signature: bytes!("000000000000000000000000"),
    };
    let module = TEST_MODULES[acc as usize];
    let operation_hash = get_operation_hash(user_op.clone(), module, DEV_CHAIN_ID);

    let signer = signer(acc);
    let signature = signer
        .sign_message_sync(&operation_hash.0)
        .expect("Failed to sign operation hash");
    let signal = hash_user_op(&user_op);

    let root = TREE.root();
    let encoded_external_nullifier = EncodedExternalNullifier::from(external_nullifier);
    let proof = semaphore_proof(acc, encoded_external_nullifier.0, signal);
    let nullifier_hash = nullifier_hash(acc, encoded_external_nullifier.0);

    let proof = Proof(proof);

    let payload = PbhPayload {
        external_nullifier,
        nullifier_hash,
        root,
        proof,
    };

    let mut uo_sig = Vec::new();

    // https://github.com/safe-global/safe-smart-account/blob/21dc82410445637820f600c7399a804ad55841d5/contracts/Safe.sol#L323
    let v: FixedBytes<1> = if signature.v() as u8 == 0 {
        fixed_bytes!("1F") // 31
    } else {
        fixed_bytes!("20") // 32
    };

    uo_sig.extend_from_slice(
        &(
            fixed_bytes!("000000000000000000000000"),
            signature.r(),
            signature.s(),
            v,
        )
            .abi_encode_packed(),
    );

    uo_sig.extend_from_slice(PBHPayload::from(payload.clone()).abi_encode().as_ref());

    user_op.signature = Bytes::from(uo_sig);

    (user_op, payload)
}

#[builder]
pub fn user_op_sepolia(
    signer: PrivateKeySigner,
    safe: Address,
    module: Address,
    identity: Identity,
    inclusion_proof: InclusionProof,
    #[builder(default = ExternalNullifier::v1(12, 2024, 0))] external_nullifier: ExternalNullifier,
    #[builder(into, default = U256::ZERO)] nonce: U256,
    #[builder(default = Bytes::default())] init_code: Bytes,
    #[builder(default = bytes!("7bb3742800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"))]
    // abi.encodeCall(Safe4337Module.executeUserOp, (address(0), 0, new bytes(0), 0))
    calldata: Bytes,
    #[builder(default = fixed_bytes!("000000000000000000000000000fffd30000000000000000000000000000C350"))]
    account_gas_limits: FixedBytes<32>,
    #[builder(default = U256::from(500836))] pre_verification_gas: U256,
    #[builder(default = fixed_bytes!("0000000000000000000000003B9ACA0000000000000000000000000073140B60"))]
    gas_fees: FixedBytes<32>,
    #[builder(default = Bytes::default())] paymaster_and_data: Bytes,
) -> IEntryPoint::PackedUserOperation {
    let rand_key = U256::from_be_bytes(Address::random().into_word().0) << 32;
    let mut user_op = PackedUserOperation {
        sender: safe,
        nonce: ((rand_key | U256::from(PBH_NONCE_KEY)) << 64) | nonce,
        initCode: init_code,
        callData: calldata,
        accountGasLimits: account_gas_limits,
        preVerificationGas: pre_verification_gas,
        gasFees: gas_fees,
        paymasterAndData: paymaster_and_data,
        signature: bytes!("000000000000000000000000"),
    };

    let operation_hash = get_operation_hash(user_op.clone(), module, WC_SEPOLIA_CHAIN_ID);

    let signature = signer
        .sign_message_sync(&operation_hash.0)
        .expect("Failed to sign operation hash");
    let signal = hash_user_op(&user_op);

    let encoded_external_nullifier = EncodedExternalNullifier::from(external_nullifier);

    let proof = semaphore_rs::protocol::generate_proof(
        &identity,
        &inclusion_proof.proof,
        encoded_external_nullifier.0,
        signal,
    )
    .expect("Failed to generate semaphore proof");
    let nullifier_hash =
        semaphore_rs::protocol::generate_nullifier_hash(&identity, encoded_external_nullifier.0);

    let proof = Proof(proof);

    let payload = PbhPayload {
        external_nullifier,
        nullifier_hash,
        root: inclusion_proof.root,
        proof,
    };
    let mut uo_sig = Vec::new();

    // https://github.com/safe-global/safe-smart-account/blob/21dc82410445637820f600c7399a804ad55841d5/contracts/Safe.sol#L323
    let v: FixedBytes<1> = if signature.v() as u8 == 0 {
        fixed_bytes!("1F") // 31
    } else {
        fixed_bytes!("20") // 32
    };

    uo_sig.extend_from_slice(
        &(
            fixed_bytes!("000000000000000000000000"),
            signature.r(),
            signature.s(),
            v,
        )
            .abi_encode_packed(),
    );

    uo_sig.extend_from_slice(PBHPayload::from(payload.clone()).abi_encode().as_ref());

    user_op.signature = Bytes::from(uo_sig);

    user_op
}

pub fn pbh_bundle(
    user_ops: Vec<PackedUserOperation>,
    proofs: Vec<PBHPayload>,
) -> IPBHEntryPoint::handleAggregatedOpsCall {
    IPBHEntryPoint::handleAggregatedOpsCall {
        _0: vec![UserOpsPerAggregator {
            userOps: user_ops,
            signature: proofs.abi_encode().into(),
            aggregator: PBH_DEV_SIGNATURE_AGGREGATOR,
        }],
        _1: PBH_DEV_ENTRYPOINT,
    }
}

#[builder]
pub fn pbh_multicall(
    acc: u32,
    #[builder(default = ExternalNullifier::v1(12, 2024, 0))] external_nullifier: ExternalNullifier,
) -> IPBHEntryPoint::pbhMulticallCall {
    let sender = account(acc);

    let call = IMulticall3::Call3::default();
    let calls = vec![call];

    let signal_hash: alloy_primitives::Uint<256, 4> =
        hash_to_field(&SolValue::abi_encode_packed(&(sender, calls.clone())));

    let root = TREE.root();
    let encoded_external_nullifier = EncodedExternalNullifier::from(external_nullifier);
    let proof = semaphore_proof(acc, encoded_external_nullifier.0, signal_hash);
    let nullifier_hash = nullifier_hash(acc, encoded_external_nullifier.0);

    let proof = [
        proof.0 .0,
        proof.0 .1,
        proof.1 .0[0],
        proof.1 .0[1],
        proof.1 .1[0],
        proof.1 .1[1],
        proof.2 .0,
        proof.2 .1,
    ];

    let payload = IPBHEntryPoint::PBHPayload {
        root,
        pbhExternalNullifier: EncodedExternalNullifier::from(external_nullifier).0,
        nullifierHash: nullifier_hash,
        proof,
    };

    IPBHEntryPoint::pbhMulticallCall { calls, payload }
}

pub fn get_operation_hash(
    user_op: PackedUserOperation,
    module: Address,
    chain_id: u64,
) -> FixedBytes<32> {
    let encoded_safe_struct: EncodedSafeOpStruct = user_op.into();
    let safe_struct_hash = keccak256(&encoded_safe_struct.abi_encode());
    let domain_separator = keccak256(
        &(
            fixed_bytes!("47e79534a245952e8b16893a336b85a3d9ea9fa8c573f3d803afb92a79469218"),
            chain_id,
            module,
        )
            .abi_encode(),
    );
    let operation_data = (
        fixed_bytes!("19"),
        fixed_bytes!("01"),
        domain_separator,
        safe_struct_hash,
    )
        .abi_encode_packed();
    keccak256(&operation_data)
}

impl From<PackedUserOperation> for EncodedSafeOpStruct {
    fn from(value: PackedUserOperation) -> Self {
        Self {
            typeHash: fixed_bytes!(
                "c03dfc11d8b10bf9cf703d558958c8c42777f785d998c62060d85a4f0ef6ea7f"
            ),
            safe: value.sender,
            nonce: value.nonce,
            initCodeHash: keccak256(&value.initCode),
            callDataHash: keccak256(&value.callData),
            verificationGasLimit: (U256::from_be_bytes(value.accountGasLimits.into())
                >> U256::from(128))
            .to(),
            callGasLimit: (U256::from_be_bytes(value.accountGasLimits.into())
                & U256::from_str("0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").unwrap())
            .to(),
            preVerificationGas: value.preVerificationGas,
            maxPriorityFeePerGas: (U256::from_be_bytes(value.gasFees.into()) >> U256::from(128))
                .to(),
            maxFeePerGas: (U256::from_be_bytes(value.gasFees.into())
                & U256::from_str("0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").unwrap())
            .to(),
            paymasterAndDataHash: keccak256(&value.paymasterAndData),
            validUntil: U48::ZERO,
            validAfter: U48::ZERO,
            entryPoint: DEVNET_ENTRYPOINT,
        }
    }
}

impl From<PbhPayload> for PBHPayload {
    fn from(val: PbhPayload) -> Self {
        let p0 = val.proof.0 .0 .0;
        let p1 = val.proof.0 .0 .1;
        let p2 = val.proof.0 .1 .0[0];
        let p3 = val.proof.0 .1 .0[1];
        let p4 = val.proof.0 .1 .1[0];
        let p5 = val.proof.0 .1 .1[1];
        let p6 = val.proof.0 .2 .0;
        let p7 = val.proof.0 .2 .1;

        Self {
            root: val.root,
            pbhExternalNullifier: EncodedExternalNullifier::from(val.external_nullifier).0,
            nullifierHash: val.nullifier_hash,
            proof: [p0, p1, p2, p3, p4, p5, p6, p7],
        }
    }
}

impl Into<RpcUserOperationV0_7> for PackedUserOperation {
    fn into(self) -> RpcUserOperationV0_7 {
        RpcUserOperationV0_7 {
            sender: self.sender,
            nonce: self.nonce,
            factory: None,
            call_data: self.callData,
            factory_data: None,
            verification_gas_limit: (U256::from_be_bytes(self.accountGasLimits.into())
                >> U256::from(128))
            .to(),
            call_gas_limit: (U256::from_be_bytes(self.accountGasLimits.into())
                & U256::from_str("0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").unwrap())
            .to(),
            pre_verification_gas: self.preVerificationGas,
            max_priority_fee_per_gas: (U256::from_be_bytes(self.gasFees.into()) >> U256::from(128))
                .to(),
            max_fee_per_gas: (U256::from_be_bytes(self.gasFees.into())
                & U256::from_str("0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").unwrap())
            .to(),
            signature: self.signature,
            paymaster: None,
            paymaster_data: None,
            paymaster_post_op_gas_limit: None,
            paymaster_verification_gas_limit: None,
            aggregator: Some(PBH_DEV_SIGNATURE_AGGREGATOR),
            eip7702_auth: None,
        }
    }
}

pub fn hash_user_op(user_op: &PackedUserOperation) -> Field {
    let hash = SolValue::abi_encode_packed(&(&user_op.sender, &user_op.nonce, &user_op.callData));

    hash_to_field(hash.as_slice())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use test_case::test_case;

    #[test_case(0, "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266")]
    #[test_case(1, "0x70997970C51812dc3A010C7d01b50e0d17dc79C8")]
    #[test_case(2, "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC")]
    #[test_case(3, "0x90F79bf6EB2c4f870365E785982E1f101E93b906")]
    #[test_case(4, "0x15d34AAf54267DB7D7c367839AAf71A00a2C6A65")]
    #[test_case(5, "0x9965507D1a55bcC2695C58ba16FB37d819B0A4dc")]
    #[test_case(6, "0x976EA74026E726554dB657fA54763abd0C3a0aa9")]
    #[test_case(7, "0x14dC79964da2C08b23698B3D3cc7Ca32193d9955")]
    #[test_case(8, "0x23618e81E3f5cdF7f54C3d65f7FBc0aBf5B21E8f")]
    #[test_case(9, "0xa0Ee7A142d267C1f36714E4a8F75612F20a79720")]
    fn mnemonic_accounts(index: u32, exp_address: &str) {
        let exp: Address = exp_address.parse().unwrap();
        assert_eq!(exp, account(index));
    }

    #[test]
    fn treeroot() {
        assert_eq!(
            U256::from_str(
                "2331208655746773478043068354535648778756593025616855591272936751328757051821"
            )
            .unwrap(),
            tree_root()
        )
    }

    #[test]
    fn test_get_safe_op_hash() {
        let uo = PackedUserOperation {
            sender: address!("c96D03586112D52b5738acA1dc6A535E4ff88aA8"),
            nonce: U256::from(1) << 64,
            initCode: Bytes::default(),
            callData: Bytes::default(),
            accountGasLimits: fixed_bytes!(
                "0000000000000000000000000000ffd300000000000000000000000000000000"
            ),
            preVerificationGas: U256::from(21000),
            gasFees: fixed_bytes!(
                "0000000000000000000000000000000100000000000000000000000000000001"
            ),
            paymasterAndData: Bytes::default(),
            signature: bytes!("000000000000000000000000"),
        };

        let safe_op_hash =
            get_operation_hash(uo, address!("f05f1C282f8D16fe0E582e4B7478E50E7201b481"), 1);

        assert_eq!(
            safe_op_hash,
            fixed_bytes!("0x71d71c003eed5782e2f73a49b9e0be207ab8a2f35c138c5fa046c5b1e31c1be7")
        );
    }

    #[test]
    fn test_pbh_nonce_key() {
        let rand_key = U256::from_be_bytes(Address::random().into_word().0) << 32;
        let nonce = ((rand_key | U256::from(PBH_NONCE_KEY)) << 64) | U256::ZERO;
        let mask = U256::from_str("0xffffffff").unwrap();
        assert_eq!((nonce >> 64) & mask, U256::from(PBH_NONCE_KEY));
    }
}
