use super::*;
use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{B256, Bytes, TxKind};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use reth_evm::EvmFactory;
use reth_optimism_primitives::OpPrimitives;
use reth_provider::test_utils::NoopProvider;
use revm::{
    database::InMemoryDB,
    database_interface::ErasedError,
    state::{AccountInfo, Bytecode},
};
use serde::Deserialize;
use world_chain_test_utils::builder::{CHAIN_SPEC, EVM_CONFIG, EVM_ENV, SEALED_HEADER};

type TestContext = WorldChainPayloadBuilderCtx<NoopProvider<WorldChainSpec, OpPrimitives>>;

#[allow(
    dead_code,
    reason = "the shared harness supplies multiple independent signer roles"
)]
#[path = "../../../signer/tests/common/runtime.rs"]
mod runtime;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the reviewed disposable mTLS signer harness"]
async fn production_nullifier_transaction_uses_the_remote_signer_without_changing_fields() {
    let harness = runtime::Harness::start().await;
    let client = world_chain_signer::PbhSigner::new(harness.pbh("pbh")).expect("remote wallet");
    let (mut ctx, cancel) = context(Address::repeat_byte(0x11));
    ctx.builder_wallet =
        world_chain_signer::PbhWallet::Remote(world_chain_signer::bridge::PbhBridge::new(client));
    ctx.signing_deadline = Some(Instant::now() + std::time::Duration::from_secs(3));
    let handle = tokio::runtime::Handle::current();
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../../../signer/testdata/pbh-runtime-v1.json"))
            .expect("independent scalar2 corpus");
    let actual = tokio::task::spawn_blocking(move || {
        handle.block_on(async move {
            let mut db = InMemoryDB::default();
            db.insert_account_info(
                ctx.builder_wallet.address(),
                AccountInfo {
                    nonce: 7,
                    ..Default::default()
                },
            );
            let mut env = EVM_ENV.clone();
            env.cfg_env.chain_id = 480;
            env.block_env.basefee = 1_000_000_000;
            let mut evm = OpEvmFactory::<OpTx>::default().create_evm(&mut db, env);
            cancel.request_finalization();
            let signed = spend_nullifiers_tx(&ctx, &mut evm, HashSet::from([Field::from(1)]))
                .expect("finalization still spends required nullifiers");
            ctx.signing_deadline = None;
            assert!(
                spend_nullifiers_tx(&ctx, &mut evm, HashSet::from([Field::from(1)])).is_err(),
                "no fresh timeout fallback without a payload deadline"
            );
            drop(cancel);
            assert!(
                spend_nullifiers_tx(&ctx, &mut evm, HashSet::from([Field::from(1)])).is_err(),
                "cancelled build cannot sign"
            );
            signed.encoded_2718()
        })
    })
    .await
    .expect("Reth build worker topology");
    assert_eq!(
        Bytes::from(actual),
        expected["cases"][0]["signedTransaction"]
            .as_str()
            .expect("full transaction")
            .parse::<Bytes>()
            .expect("hex")
    );
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vectors {
    version: u32,
    signer: Address,
    cases: Vec<Vector>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vector {
    name: String,
    chain_id: u64,
    account_nonce: Option<u64>,
    base_fee: u64,
    entry_point: Address,
    nullifier: Field,
    unsigned_transaction: Bytes,
    signing_hash: B256,
    signed_transaction: Bytes,
    transaction_hash: B256,
}

fn context(entry_point: Address) -> (TestContext, CancelOnDrop) {
    let cancel = CancelOnDrop::default();
    let ctx = WorldChainPayloadBuilderCtxBuilder {
        verified_blockspace_capacity: 70,
        pbh_entry_point: entry_point,
        pbh_signature_aggregator: Address::ZERO,
        signing_deadlines: None,
        builder_wallet: PrivateKeySigner::from_bytes(&B256::with_last_byte(1))
            .expect("public scalar-one test key")
            .into(),
        block_uncompressed_size_limit: None,
    }
    .build(
        NoopProvider::new(CHAIN_SPEC.clone()),
        EVM_CONFIG.clone(),
        OpBuilderConfig::default(),
        PayloadConfig::new(
            Arc::new(SEALED_HEADER.clone()),
            OpPayloadBuilderAttributes::default(),
            PayloadId::default(),
        ),
        &cancel,
        None,
    );
    (ctx, cancel)
}

#[test]
fn pbh_signing_matches_independent_transaction_vectors() {
    let vectors: Vectors = serde_json::from_str(include_str!("../../testdata/pbh-signing-v1.json"))
        .expect("public PBH signing vectors");
    assert_eq!(vectors.version, 1);
    for vector in vectors.cases {
        let (ctx, _cancel) = context(vector.entry_point);
        assert_eq!(ctx.builder_wallet.address(), vectors.signer);
        let mut db = InMemoryDB::default();
        if let Some(nonce) = vector.account_nonce {
            db.insert_account_info(
                vectors.signer,
                AccountInfo {
                    nonce,
                    ..Default::default()
                },
            );
        }
        let mut env = EVM_ENV.clone();
        env.cfg_env.chain_id = vector.chain_id;
        env.block_env.basefee = vector.base_fee;
        let mut evm = OpEvmFactory::<OpTx>::default().create_evm(&mut db, env);
        let transaction = spend_nullifiers_tx(&ctx, &mut evm, HashSet::from([vector.nullifier]))
            .expect("PBH transaction signs");
        assert_eq!(transaction.signer(), vectors.signer);
        assert_eq!(
            transaction.recover_signer().expect("recover sender"),
            vectors.signer
        );
        assert_eq!(
            transaction.encoded_2718(),
            vector.signed_transaction.as_ref(),
            "{}",
            vector.name
        );

        let signed = transaction.as_eip1559().expect("PBH uses type 2");
        let tx = signed.tx();
        assert_eq!(tx.chain_id, vector.chain_id);
        assert_eq!(tx.nonce, vector.account_nonce.unwrap_or_default());
        assert_eq!(tx.to, TxKind::Call(vector.entry_point));
        assert_eq!(tx.value, U256::ZERO);
        assert_eq!(tx.gas_limit, 120_000);
        assert_eq!(tx.max_fee_per_gas, u128::from(vector.base_fee));
        assert_eq!(tx.max_priority_fee_per_gas, u128::from(vector.base_fee));
        assert!(tx.access_list.is_empty());
        let call = spendNullifierHashesCall::abi_decode_validate(&tx.input)
            .expect("canonical PBH calldata");
        assert_eq!(call._nullifierHashes, vec![vector.nullifier]);
        let mut unsigned = Vec::new();
        tx.encode_for_signing(&mut unsigned);
        assert_eq!(unsigned, vector.unsigned_transaction.as_ref());
        assert_eq!(signed.signature_hash(), vector.signing_hash);
        assert_eq!(*signed.hash(), vector.transaction_hash);
        assert!(signed.signature().normalize_s().is_none());
    }
}

#[test]
fn pbh_signing_uses_pending_nonce_and_preserves_nullifier_set() {
    let (ctx, _cancel) = context(Address::repeat_byte(0x11));
    let address = ctx.builder_wallet.address();
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        address,
        AccountInfo {
            nonce: 7,
            ..Default::default()
        },
    );
    db.insert_account_info(
        Address::repeat_byte(0x22),
        AccountInfo {
            nonce: 99,
            ..Default::default()
        },
    );
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm(&mut db, EVM_ENV.clone());
    let nullifiers = HashSet::from([Field::from(1), Field::from(2)]);
    let first = spend_nullifiers_tx(&ctx, &mut evm, nullifiers.clone()).expect("first transaction");
    assert_eq!(first.nonce(), 7);
    assert_eq!(first.gas_limit(), 140_000);
    let call = spendNullifierHashesCall::abi_decode_validate(first.input()).expect("PBH calldata");
    assert_eq!(
        call._nullifierHashes.into_iter().collect::<HashSet<_>>(),
        nullifiers
    );

    evm.db_mut().insert_account_info(
        address,
        AccountInfo {
            nonce: 8,
            ..Default::default()
        },
    );
    let second = spend_nullifiers_tx(&ctx, &mut evm, nullifiers).expect("next transaction");
    assert_eq!(second.nonce(), 8);
}

#[derive(Debug)]
struct UnavailableDatabase;

impl RevmDatabase for UnavailableDatabase {
    type Error = ErasedError;

    fn basic(&mut self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Err(ErasedError::new(std::io::Error::other(
            "test nonce read unavailable",
        )))
    }

    fn code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        Err(ErasedError::new(std::io::Error::other(
            "test code read unavailable",
        )))
    }

    fn storage(&mut self, _address: Address, _index: U256) -> Result<U256, Self::Error> {
        Err(ErasedError::new(std::io::Error::other(
            "test storage read unavailable",
        )))
    }

    fn block_hash(&mut self, _number: u64) -> Result<B256, Self::Error> {
        Err(ErasedError::new(std::io::Error::other(
            "test block read unavailable",
        )))
    }
}

#[test]
fn pbh_signing_propagates_nonce_read_failure() {
    let (ctx, _cancel) = context(Address::repeat_byte(0x11));
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm(UnavailableDatabase, EVM_ENV.clone());
    let error = spend_nullifiers_tx(&ctx, &mut evm, HashSet::from([Field::from(1)]))
        .expect_err("a failed nonce read must not produce a signed transaction");
    assert_eq!(
        error.root_cause().to_string(),
        "test nonce read unavailable"
    );
}
