//! Regression tests for CREATE/CREATE2 handling in `SimulationInspector`.
//!
//! Runs entirely against an in-memory `CacheDB<EmptyDB>` — no fork URL needed.

use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Address, Bytes, U256, address};
use op_revm::{OpSpecId, OpTransaction};
use reth_evm::{Evm as RethEvm, EvmFactory};
use revm::{
    bytecode::Bytecode,
    context::{BlockEnv, CfgEnv, TxEnv, result::ExecutionResult},
    state::AccountInfo,
};
use revm_database::{CacheDB, EmptyDB};
use revm_primitives::TxKind;

use world_chain_rpc::simulate::SimulationInspector;

const CHAIN_ID: u64 = 480;

/// Runtime code that executes CREATE with a 12-byte constructor, then hides
/// the constructor's revert data by reverting the outer frame with no output.
///
/// The constructor is:
/// `PUSH4 0xdeadbeef PUSH0 MSTORE PUSH1 4 PUSH1 28 REVERT`.
/// It writes `0xdeadbeef` to memory and reverts with those four bytes.
const CREATE_REVERT_TRAMPOLINE: &[u8] = &[
    0x6b, // PUSH12 constructor
    0x63, 0xde, 0xad, 0xbe, 0xef, // PUSH4 0xdeadbeef
    0x5f, 0x52, // PUSH0; MSTORE
    0x60, 0x04, // PUSH1 4 (revert-data length)
    0x60, 0x1c, // PUSH1 28 (revert-data offset)
    0xfd, // REVERT
    0x5f, 0x52, // PUSH0; MSTORE constructor at memory[20..32]
    0x60, 0x0c, // PUSH1 12 (init-code length)
    0x60, 0x14, // PUSH1 20 (init-code offset)
    0x5f, 0xf0, // PUSH0 value; CREATE
    0x50, // POP failed CREATE's zero address
    0x5f, 0x5f, 0xfd, // REVERT(0, 0)
];

fn evm_env() -> reth_evm::EvmEnv<OpSpecId> {
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::ISTHMUS);
    cfg.chain_id = CHAIN_ID;
    cfg.disable_nonce_check = true;
    cfg.disable_balance_check = true;
    cfg.disable_base_fee = true;
    reth_evm::EvmEnv::new(cfg, BlockEnv::default())
}

fn install_runtime_code(db: &mut CacheDB<EmptyDB>, address: Address, code: Vec<u8>) {
    let bytecode = Bytecode::new_raw(Bytes::from(code));
    db.insert_account_info(
        address,
        AccountInfo {
            nonce: 1,
            code_hash: bytecode.hash_slow(),
            code: Some(bytecode),
            ..Default::default()
        },
    );
}

#[test]
fn inspector_captures_constructor_revert_reason() {
    let caller = address!("00000000000000000000000000000000DeaDBeef");
    let trampoline = address!("000000000000000000000000000000000000c0de");
    let mut db = CacheDB::<EmptyDB>::default();
    db.insert_account_info(
        caller,
        AccountInfo {
            balance: U256::from(10_u128.pow(21)),
            ..Default::default()
        },
    );
    install_runtime_code(&mut db, trampoline, CREATE_REVERT_TRAMPOLINE.to_vec());

    let mut evm = OpEvmFactory::default().create_evm_with_inspector(
        &mut db,
        evm_env(),
        SimulationInspector::default(),
    );
    let result = RethEvm::transact(
        &mut evm,
        OpTx(OpTransaction {
            base: TxEnv {
                caller,
                kind: TxKind::Call(trampoline),
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(CHAIN_ID),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .expect("EVM transaction should execute");

    assert!(matches!(result.result, ExecutionResult::Revert { .. }));

    let (_, inspector, _) = evm.components_mut();
    assert_eq!(
        inspector.take_deepest_revert_reason().as_deref(),
        Some("0xdeadbeef")
    );
    assert!(inspector.take_contract_creations().is_empty());
}
