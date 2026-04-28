//! Regression test for `SimulationInspector` selector capture across the
//! `CallInput::SharedBuffer` variant.
//!
//! Inner CALL/STATICCALL/DELEGATECALL frames are passed to inspectors as a
//! shared-memory range (an internal revm optimization) instead of cloned
//! `Bytes`. If the inspector only reads the `Bytes` variant, every such call
//! records selector `0x00000000` / `method: null` — invisible to the
//! backend's forbidden-method detection. This test deploys a forwarder
//! contract that places a forbidden selector in memory and CALLs through it,
//! then asserts the inspector recovered the selector and decoded the method.
//!
//! Runs entirely against an in-memory `CacheDB<EmptyDB>` — no fork URL needed.

use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Address, Bytes, U256, address};
use op_revm::{OpSpecId, OpTransaction};
use reth_evm::{Evm as RethEvm, EvmFactory};
use revm::{
    bytecode::Bytecode,
    context::{
        BlockEnv, CfgEnv, TxEnv,
        result::{ExecutionResult, Output},
    },
    state::AccountInfo,
};
use revm_database::{CacheDB, EmptyDB};
use revm_primitives::TxKind;

use world_chain_rpc::simulate::SimulationInspector;

const CHAIN_ID: u64 = 480;

/// `addOwnerWithThreshold(address,uint256)` — one of the Safe admin
/// selectors the backend checks for in trace entries.
const FORBIDDEN_SELECTOR: [u8; 4] = [0x0d, 0x58, 0x2f, 0x13];

fn evm_env() -> reth_evm::EvmEnv<OpSpecId> {
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::ISTHMUS);
    cfg.chain_id = CHAIN_ID;
    cfg.disable_nonce_check = true;
    cfg.disable_balance_check = true;
    cfg.disable_base_fee = true;
    reth_evm::EvmEnv::new(cfg, BlockEnv::default())
}

/// Build minimal EVM bytecode that:
///   1. MSTOREs the forbidden selector to memory at offset 0.
///   2. CALLs `target` with input = mem[0..4].
///   3. STOPs.
///
/// The inner CALL is passed to the inspector as `CallInput::SharedBuffer(0..4)`
/// rather than `CallInput::Bytes`, which is the case under test.
fn forwarder_bytecode(target: Address) -> Vec<u8> {
    let mut code = Vec::with_capacity(65);

    // PUSH32 <selector at high bytes, 28 zero bytes after>
    // MSTORE writes the 32-byte word in big-endian order, so the first 4
    // bytes of memory end up holding the selector.
    code.push(0x7f);
    code.extend_from_slice(&FORBIDDEN_SELECTOR);
    code.extend_from_slice(&[0u8; 28]);

    code.push(0x5f); // PUSH0  (offset = 0)
    code.push(0x52); // MSTORE

    // CALL stack (top-of-stack first):
    //   gas, addr, value, argsOffset, argsSize, retOffset, retSize
    code.push(0x5f); // PUSH0  retSize  = 0
    code.push(0x5f); // PUSH0  retOffset = 0
    code.push(0x60); // PUSH1
    code.push(0x04); //         argsSize = 4
    code.push(0x5f); // PUSH0  argsOffset = 0
    code.push(0x5f); // PUSH0  value     = 0
    code.push(0x73); // PUSH20
    code.extend_from_slice(target.as_slice());
    code.push(0x5a); // GAS    (forward all remaining gas)
    code.push(0xf1); // CALL
    code.push(0x00); // STOP

    code
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_buffer_call_resolves_selector() {
    let caller = address!("00000000000000000000000000000000DeaDBeef");
    let forwarder = address!("000000000000000000000000000000000000F0F0");
    let target = address!("000000000000000000000000000000000000Da7E");

    let mut db = CacheDB::<EmptyDB>::default();

    db.insert_account_info(
        caller,
        AccountInfo {
            balance: U256::from(10u128.pow(21)),
            ..Default::default()
        },
    );

    let bytecode = Bytecode::new_raw(Bytes::from(forwarder_bytecode(target)));
    db.insert_account_info(
        forwarder,
        AccountInfo {
            balance: U256::ZERO,
            nonce: 1,
            code_hash: bytecode.hash_slow(),
            code: Some(bytecode),
            ..Default::default()
        },
    );

    db.insert_account_info(target, AccountInfo::default());

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
                kind: TxKind::Call(forwarder),
                data: Bytes::new(),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(CHAIN_ID),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .expect("evm transact");

    match &result.result {
        ExecutionResult::Success {
            output: Output::Call(_),
            ..
        } => {}
        other => panic!("expected Success, got {other:?}"),
    }

    let (_, inspector, _) = evm.components_mut();
    let trace = inspector.take_trace_entries();

    let inner = trace
        .iter()
        .find(|t| t.from == forwarder && t.to == target)
        .expect("inner forwarder->target call should appear in trace");

    assert_eq!(
        inner.selector, "0x0d582f13",
        "selector for SharedBuffer call must match the bytes written to memory"
    );
    assert_eq!(
        inner.method.as_deref(),
        Some("addOwnerWithThreshold"),
        "decoded method must surface so backend forbidden-method checks can detect it"
    );
}
