use alloy_consensus::TxEip1559;
use alloy_eips::{eip2718::Encodable2718, eip2930::AccessList};
use alloy_network::TxSigner;
use alloy_primitives::aliases::U48;
use alloy_primitives::{address, Address, Bytes, ChainId, U256};
use alloy_rlp::Encodable;
use alloy_signer::SignerSync;
use alloy_signer_local::coins_bip39::English;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolValue};
use bon::builder;
use op_alloy_consensus::OpTypedTransaction;
use reth::chainspec::MAINNET;
use reth::transaction_pool::blobstore::InMemoryBlobStore;
use reth::transaction_pool::validate::EthTransactionValidatorBuilder;
use reth_optimism_node::txpool::{OpPooledTransaction, OpTransactionValidator};
use reth_optimism_primitives::OpTransactionSigned;
use reth_primitives::transaction::SignedTransactionIntoRecoveredExt;
use revm_primitives::{bytes, fixed_bytes, keccak256, FixedBytes, TxKind};
use semaphore::identity::Identity;
use semaphore::poseidon_tree::LazyPoseidonTree;
use semaphore::{hash_to_field, Field};
use std::str::FromStr;
use std::sync::LazyLock;
use world_chain_builder_pbh::external_nullifier::ExternalNullifier;
use world_chain_builder_pbh::payload::{PbhPayload, Proof, TREE_DEPTH};

use crate::bindings::IEntryPoint::{self, PackedUserOperation, UserOpsPerAggregator};
use crate::bindings::IMulticall3;
use crate::bindings::IPBHEntryPoint::{self, PBHPayload};
use crate::mock::MockEthProvider;
use crate::root::WorldChainRootValidator;
use crate::tx::WorldChainPooledTransaction;
use crate::validator::WorldChainTransactionValidator;

const MNEMONIC: &str = "test test test test test test test test test test test junk";

pub const TEST_SAFES: [Address; 6] = [
    address!("26479c9A59462f08f663281df6098dF7e7398363"),
    address!("CaD130298688716cDd16511C18DD88CAc327B3cA"),
    address!("4DFE55bC8eEa517efdbb6B2d056135b350b79ca2"),
    address!("e27eA3E8D654a0348DE1B42983F75c9594CdB2a2"),
    address!("220e1F32E1a76093C34388D4EeEB5fc0cAc73Ffb"),
    address!("e98F083C8C5e43593A517dA7b6b83Cb64F93c740"),
];

pub const TEST_MODULES: [Address; 6] = [
    address!("8A791620dd6260079BF849Dc5567aDC3F2FdC318"),
    address!("A51c1fc2f0D1a1b8494Ed1FE312d7C3a78Ed91C0"),
    address!("0B306BF915C4d645ff596e518fAf3F9669b97016"),
    address!("68B1D87F95878fE05B998F19b66F4baba5De1aed"),
    address!("59b670e9fA9D0A427751Af201D676719a970857b"),
    address!("a85233C63b9Ee964Add6F2cffe00Fd84eb32338f"),
];

pub const DEV_CHAIN_ID: u64 = 2151908;

pub const PBH_NONCE_KEY: U256 = U256::from_limbs([1123123123, 0, 0, 0]);

pub const DEVNET_ENTRYPOINT: Address = address!("9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0");

pub const PBH_TEST_SIGNATURE_AGGREGATOR: Address =
    address!("5FC8d32690cc91D4c39d9d3abcBD16989F875707");

pub const PBH_TEST_ENTRYPOINT: Address = address!("Dc64a140Aa3E981100a9becA4E685f962f0cF6C9");

pub const TEST_WORLD_ID: Address = address!("5FbDB2315678afecb367f032d93F642f64180aa3");

sol! {
    struct EncodedSafeOpStruct {
        bytes32 typeHash;
        address safe;
        uint256 nonce;
        bytes32 initCodeHash;
        bytes32 callDataHash;
        uint128 verificationGasLimit;
        uint128 callGasLimit;
        uint256 preVerificationGas;
        uint128 maxPriorityFeePerGas;
        uint128 maxFeePerGas;
        bytes32 paymasterAndDataHash;
        uint48 validAfter;
        uint48 validUntil;
        address entryPoint;
    }
}

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

pub fn tree_inclusion_proof(acc: u32) -> semaphore::poseidon_tree::Proof {
    TREE.proof(acc as usize)
}

pub fn nullifier_hash(acc: u32, external_nullifier: Field) -> Field {
    let identity = identity(acc);

    semaphore::protocol::generate_nullifier_hash(&identity, external_nullifier)
}

pub fn semaphore_proof(
    acc: u32,
    ext_nullifier: Field,
    signal: Field,
) -> semaphore::protocol::Proof {
    let identity = identity(acc);
    let incl_proof = tree_inclusion_proof(acc);

    semaphore::protocol::generate_proof(&identity, &incl_proof, ext_nullifier, signal)
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
    let op_tx: OpTypedTransaction = tx.into();
    let tx_signed = OpTransactionSigned::new(op_tx, signature);
    let pooled = OpPooledTransaction::new(
        tx_signed.clone().into_ecrecovered_unchecked().unwrap(),
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
    let tx_signed = OpTransactionSigned::new(tx.into(), signature);
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
    #[builder(default = U256::from(60232))] pre_verification_gas: U256,
    #[builder(default = fixed_bytes!("00000000000000000000000000000001000000000000000000000000257353A9"))]
    gas_fees: FixedBytes<32>,
    #[builder(default = Bytes::default())] paymaster_and_data: Bytes,
) -> (IEntryPoint::PackedUserOperation, PbhPayload) {
    let mut user_op = PackedUserOperation {
        sender: TEST_SAFES[acc as usize],
        nonce: (PBH_NONCE_KEY << 64) | nonce,
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

    let signal = crate::eip4337::hash_user_op(&user_op);

    let root = TREE.root();
    let proof = semaphore_proof(acc, external_nullifier.to_word(), signal);
    let nullifier_hash = nullifier_hash(acc, external_nullifier.to_word());

    let proof = Proof(proof);

    let payload = PbhPayload {
        external_nullifier,
        nullifier_hash,
        root,
        proof,
    };

    let mut uo_sig = Vec::with_capacity(429);
    uo_sig.extend_from_slice(
        &(
            fixed_bytes!("000000000000000000000000"),
            signature.r(),
            signature.s(),
            fixed_bytes!("1F"), // https://github.com/safe-global/safe-smart-account/blob/21dc82410445637820f600c7399a804ad55841d5/contracts/Safe.sol#L326
        )
            .abi_encode_packed(),
    );
    uo_sig.extend_from_slice(PBHPayload::from(payload.clone()).abi_encode().as_ref());

    user_op.signature = Bytes::from(uo_sig);
    (user_op, payload)
}

pub fn pbh_bundle(
    user_ops: Vec<PackedUserOperation>,
    proofs: Vec<PbhPayload>,
) -> IPBHEntryPoint::handleAggregatedOpsCall {
    let mut signature_buff = Vec::new();
    proofs.encode(&mut signature_buff);

    IPBHEntryPoint::handleAggregatedOpsCall {
        _0: vec![UserOpsPerAggregator {
            userOps: user_ops,
            signature: signature_buff.into(),
            aggregator: PBH_TEST_SIGNATURE_AGGREGATOR,
        }],
        _1: PBH_TEST_ENTRYPOINT,
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
    let proof = semaphore_proof(acc, external_nullifier.to_word(), signal_hash);
    let nullifier_hash = nullifier_hash(acc, external_nullifier.to_word());

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

    // TODO: Switch to ruint in semaphore-rs and remove this
    let proof: [U256; 8] = proof
        .into_iter()
        .map(|x| {
            let mut bytes_repr: [u8; 32] = [0; 32];
            x.to_big_endian(&mut bytes_repr);

            U256::from_be_bytes(bytes_repr)
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();

    let payload = IPBHEntryPoint::PBHPayload {
        root,
        pbhExternalNullifier: external_nullifier.to_word(),
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
        let mut p0 = [0; 32];
        val.proof.0 .0 .0.to_big_endian(&mut p0);
        let mut p1 = [0; 32];
        val.proof.0 .0 .1.to_big_endian(&mut p1);
        let mut p2 = [0; 32];
        val.proof.0 .1 .0[0].to_big_endian(&mut p2);
        let mut p3 = [0; 32];
        val.proof.0 .1 .0[1].to_big_endian(&mut p3);
        let mut p4 = [0; 32];
        val.proof.0 .1 .1[0].to_big_endian(&mut p4);
        let mut p5 = [0; 32];
        val.proof.0 .1 .1[1].to_big_endian(&mut p5);
        let mut p6 = [0; 32];
        val.proof.0 .2 .0.to_big_endian(&mut p6);
        let mut p7 = [0; 32];
        val.proof.0 .2 .1.to_big_endian(&mut p7);

        Self {
            root: val.root,
            pbhExternalNullifier: val.external_nullifier.to_word(),
            nullifierHash: val.nullifier_hash,
            proof: [
                U256::from_be_bytes(p0),
                U256::from_be_bytes(p1),
                U256::from_be_bytes(p2),
                U256::from_be_bytes(p3),
                U256::from_be_bytes(p4),
                U256::from_be_bytes(p5),
                U256::from_be_bytes(p6),
                U256::from_be_bytes(p7),
            ],
        }
    }
}

impl Into<alloy_rpc_types::PackedUserOperation> for PackedUserOperation {
    fn into(self) -> alloy_rpc_types::PackedUserOperation {
        alloy_rpc_types::PackedUserOperation {
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
        }
    }
}

pub fn world_chain_validator(
) -> WorldChainTransactionValidator<MockEthProvider, WorldChainPooledTransaction> {
    let client = MockEthProvider::default();
    let validator = EthTransactionValidatorBuilder::new(MAINNET.clone())
        .no_shanghai()
        .no_cancun()
        .build(client.clone(), InMemoryBlobStore::default());
    let validator = OpTransactionValidator::new(validator).require_l1_data_gas_fee(false);
    let root_validator = WorldChainRootValidator::new(client, TEST_WORLD_ID).unwrap();
    WorldChainTransactionValidator::new(
        validator,
        root_validator,
        30,
        PBH_TEST_ENTRYPOINT,
        PBH_TEST_SIGNATURE_AGGREGATOR,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
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
        println!("Tree Root: {:?}", tree_root());
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
            fixed_bytes!("0x6278d2de0a0ad2a362e7d421434ca04885bd291c965b311de6fb687d4e4c86b1")
        );
    }

    #[test]
    fn test_user_op_signature() {
        let uo = user_op().acc(2).nonce(U256::from(0)).call().0;
        println!("{}", uo.signature.length());
        println!("User Op: {:?}", uo);
    }
}
