use std::future::Future;

use criterion::{criterion_group, criterion_main, Criterion};
use reth::transaction_pool::blobstore::InMemoryBlobStore;
use reth::transaction_pool::{Pool, PoolTransaction as _, TransactionPool, TransactionValidator};
use reth_primitives::{BlockBody, SealedBlock, SealedHeader};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use semaphore::Field;
use tokio::runtime::{Builder, Runtime};
use world_chain_builder::pool::ordering::WorldChainOrdering;
use world_chain_builder::pool::root::{LATEST_ROOT_SLOT, OP_WORLD_ID};
use world_chain_builder::pool::tx::WorldChainPooledTransaction;
use world_chain_builder::pool::validator::WorldChainTransactionValidator;
use world_chain_builder::test::{
    get_non_pbh_transaction, get_pbh_transaction, world_chain_validator,
};

type PoolType = Pool<
    WorldChainTransactionValidator<MockEthProvider, WorldChainPooledTransaction>,
    WorldChainOrdering<WorldChainPooledTransaction>,
    InMemoryBlobStore,
>;

#[derive(Clone)]
struct Setup {
    pool: PoolType,
    transaction: WorldChainPooledTransaction,
}

fn async_setup<F>(rt: &Runtime, f: F) -> F::Output
where
    F: Future,
{
    rt.block_on(f)
}

fn validator_setup() -> WorldChainTransactionValidator<MockEthProvider, WorldChainPooledTransaction>
{
    let validator = world_chain_validator();
    let transaction = get_pbh_transaction();
    validator.inner().client().add_account(
        transaction.sender(),
        ExtendedAccount::new(transaction.nonce(), alloy_primitives::U256::MAX),
    );
    // Insert a world id root into the OpWorldId Account
    validator.inner().client().add_account(
        OP_WORLD_ID,
        ExtendedAccount::new(0, alloy_primitives::U256::ZERO).extend_storage(vec![(
            LATEST_ROOT_SLOT.into(),
            transaction.pbh_payload.clone().unwrap().root,
        )]),
    );

    let header = SealedHeader::default();
    let body = BlockBody::default();
    let block = SealedBlock::new(header, body);

    // Propogate the block to the root validator
    validator.on_new_head_block(&block);

    validator
}

fn pool_setup() -> PoolType {
    let validator = validator_setup();
    let ordering = WorldChainOrdering::default();

    Pool::new(
        validator.clone(),
        ordering,
        InMemoryBlobStore::default(),
        Default::default(),
    )
}

fn non_pbh_setup() -> Setup {
    let pool = pool_setup();
    let transaction = get_non_pbh_transaction();

    Setup { pool, transaction }
}

fn pbh_setup() -> Setup {
    let pool = pool_setup();
    let transaction = get_pbh_transaction();

    Setup { pool, transaction }
}

fn spoofed_nullifier_setup() -> Setup {
    let pool = pool_setup();
    let mut transaction = get_pbh_transaction();
    let pbh_payload = transaction.pbh_payload.as_mut().unwrap();
    pbh_payload.nullifier_hash = Field::default();
    Setup { pool, transaction }
}

async fn repeat_non_pbh_setup() -> Setup {
    let setup = non_pbh_setup();
    setup
        .pool
        .add_external_transaction(setup.transaction.clone())
        .await
        .unwrap();
    setup
}

async fn repeat_pbh_setup() -> Setup {
    let setup = pbh_setup();
    setup
        .pool
        .add_external_transaction(setup.transaction.clone())
        .await
        .unwrap();
    setup
}

async fn run(setup: Setup) {
    setup
        .pool
        .add_external_transaction(setup.transaction)
        .await
        .unwrap();
}

async fn run_err(setup: Setup) {
    setup
        .pool
        .add_external_transaction(setup.transaction)
        .await
        .unwrap_err();
}

fn non_pbh_bench(c: &mut Criterion) {
    let rt = Builder::new_multi_thread().enable_all().build().unwrap();
    c.bench_function("Non PBH Transaction", |b| {
        b.to_async(&rt)
            .iter_batched(non_pbh_setup, run, criterion::BatchSize::SmallInput)
    });
}

fn repeat_non_pbh_bench(c: &mut Criterion) {
    c.bench_function("Repeat Non PBH Transaction", |b| {
        let rt = Builder::new_multi_thread().enable_all().build().unwrap();
        let setup = async_setup(&rt, repeat_non_pbh_setup());
        b.to_async(rt)
            .iter_batched(|| setup.clone(), run_err, criterion::BatchSize::SmallInput)
    });
}

fn pbh_bench(c: &mut Criterion) {
    let rt = Builder::new_multi_thread().enable_all().build().unwrap();
    c.bench_function("PBH Transaction", |b| {
        b.to_async(&rt)
            .iter_batched(pbh_setup, run, criterion::BatchSize::SmallInput)
    });
}

fn repeat_pbh_bench(c: &mut Criterion) {
    c.bench_function("Repeat PBH Transaction", |b| {
        let rt = Builder::new_multi_thread().enable_all().build().unwrap();
        let setup = async_setup(&rt, repeat_pbh_setup());
        b.to_async(rt)
            .iter_batched(|| setup.clone(), run_err, criterion::BatchSize::SmallInput)
    });
}

fn spoofed_nullifier_bench(c: &mut Criterion) {
    c.bench_function("Spoofed Nullifier", |b| {
        let rt = Builder::new_multi_thread().enable_all().build().unwrap();
        b.to_async(rt).iter_batched(
            spoofed_nullifier_setup,
            run_err,
            criterion::BatchSize::SmallInput,
        )
    });
}

fn criterion_config() -> Criterion {
    Criterion::default().sample_size(30)
}

criterion_group!(
    name = benches;
    config = criterion_config();
    targets = non_pbh_bench,
    repeat_non_pbh_bench,
    pbh_bench,
    repeat_pbh_bench,
    spoofed_nullifier_bench
);
criterion_main!(benches);
