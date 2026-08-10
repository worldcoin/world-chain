//! Regression tests for CREATE/CREATE2 handling and terminal revert selection
//! in `SimulationInspector`.
//!
//! Runs entirely against an in-memory `CacheDB<EmptyDB>` — no fork URL needed.

use alloy_op_evm::{OpEvmFactory, OpTx};
use alloy_primitives::{Address, Bytes, U256, address};
use op_revm::{OpHaltReason, OpSpecId, OpTransaction};
use reth_evm::{Evm as RethEvm, EvmFactory};
use revm::{
    bytecode::Bytecode,
    context::{BlockEnv, CfgEnv, TxEnv, result::ExecutionResult},
    state::AccountInfo,
};
use revm_database::{CacheDB, EmptyDB};
use revm_primitives::TxKind;

use world_chain_rpc::{
    simulate::{SimulationInspector, TraceKind, TraceOutcome, relax_cfg_for_simulation},
    simulate_consts::{
        EXEC_TRANSACTION_FROM_MODULE_SELECTOR, EXECUTE_USER_OP_SELECTOR, EXECUTION_FAILED_SELECTOR,
    },
};

const CHAIN_ID: u64 = 480;
const CALLER: Address = address!("00000000000000000000000000000000DeaDBeef");
const TRAMPOLINE: Address = address!("000000000000000000000000000000000000c0de");
const REVERTING_CHILD: Address = address!("000000000000000000000000000000000000cafe");
const CATCHING_CHILD: Address = address!("000000000000000000000000000000000000beef");
const MODULE_GUARD: Address = address!("000000000000000000000000000000000000feed");

/// Reverts with `0xaaaaaaaa`.
const CHILD_REVERT: &[u8] = &[
    0x63, 0xaa, 0xaa, 0xaa, 0xaa, // PUSH4 0xaaaaaaaa
    0x5f, 0x52, // PUSH0; MSTORE
    0x60, 0x04, // PUSH1 4 (revert-data length)
    0x60, 0x1c, // PUSH1 28 (revert-data offset)
    0xfd, // REVERT
];

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

/// Same constructor, but the outer frame catches CREATE's failure and STOPs.
/// This distinguishes the parent and child outcomes and guards trace-index
/// association across nested frames.
const CAUGHT_CREATE_REVERT_TRAMPOLINE: &[u8] = &[
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
    0x50, 0x00, // POP failed CREATE's zero address; STOP
];

/// Executes CREATE with empty init code and then stops. Empty init code
/// successfully deploys an empty contract.
const SUCCESSFUL_CREATE_TRAMPOLINE: &[u8] = &[
    0x5f, 0x5f, 0x5f, 0xf0, // CREATE(value=0, offset=0, size=0)
    0x50, 0x00, // POP deployed address; STOP
];

fn evm_env() -> reth_evm::EvmEnv<OpSpecId> {
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::ISTHMUS);
    cfg.chain_id = CHAIN_ID;
    relax_cfg_for_simulation(&mut cfg);
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

fn run_trampoline(code: &[u8]) -> (ExecutionResult<OpHaltReason>, SimulationInspector) {
    run_trampoline_with_input(code, &[], Bytes::new())
}

fn run_trampoline_with_input(
    code: &[u8],
    contracts: &[(Address, &[u8])],
    input: Bytes,
) -> (ExecutionResult<OpHaltReason>, SimulationInspector) {
    let mut db = CacheDB::<EmptyDB>::default();
    db.insert_account_info(
        CALLER,
        AccountInfo {
            balance: U256::from(10_u128.pow(21)),
            ..Default::default()
        },
    );
    install_runtime_code(&mut db, TRAMPOLINE, code.to_vec());
    for (address, code) in contracts {
        install_runtime_code(&mut db, *address, code.to_vec());
    }

    let mut evm = OpEvmFactory::default().create_evm_with_inspector(
        &mut db,
        evm_env(),
        SimulationInspector::default(),
    );
    let result = RethEvm::transact(
        &mut evm,
        OpTx(OpTransaction {
            base: TxEnv {
                caller: CALLER,
                kind: TxKind::Call(TRAMPOLINE),
                data: input,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(CHAIN_ID),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .expect("EVM transaction should execute")
    .result;

    let (_, inspector, _) = evm.components_mut();
    (result, std::mem::take(inspector))
}

/// Calls `child` with an optional four-byte selector (discarding its result),
/// then reverts with the supplied four-byte payload.
fn call_then_revert(
    child: Address,
    child_selector: Option<[u8; 4]>,
    parent_payload: [u8; 4],
) -> Vec<u8> {
    let mut code = Vec::new();
    if let Some(selector) = child_selector {
        code.push(0x63); // PUSH4 selector
        code.extend_from_slice(&selector);
        code.extend_from_slice(&[0x5f, 0x52]); // PUSH0; MSTORE
    }
    code.extend_from_slice(&[
        0x5f, 0x5f, // Empty CALL output
    ]);
    if child_selector.is_some() {
        code.extend_from_slice(&[
            0x60, 0x04, // PUSH1 4 (input length)
            0x60, 0x1c, // PUSH1 28 (input offset)
        ]);
    } else {
        code.extend_from_slice(&[0x5f, 0x5f]); // Empty CALL input
    }
    code.extend_from_slice(&[
        0x5f, // Zero value
        0x73, // PUSH20 child address
    ]);
    code.extend_from_slice(child.as_slice());
    code.extend_from_slice(&[
        0x61, 0xff, 0xff, // PUSH2 65535 gas
        0xf1, 0x50, // CALL; POP failure result
        0x63, // PUSH4 parent payload
    ]);
    code.extend_from_slice(&parent_payload);
    code.extend_from_slice(&[
        0x5f, 0x52, // PUSH0; MSTORE
        0x60, 0x04, // PUSH1 4 (revert-data length)
        0x60, 0x1c, // PUSH1 28 (revert-data offset)
        0xfd, // REVERT
    ]);
    code
}

fn call_then_stop(child: Address) -> Vec<u8> {
    let mut code = vec![
        0x5f, 0x5f, 0x5f, 0x5f, 0x5f, // Empty CALL output/input and zero value
        0x73, // PUSH20 child address
    ];
    code.extend_from_slice(child.as_slice());
    code.extend_from_slice(&[
        0x61, 0xff, 0xff, // PUSH2 65535 gas
        0xf1, 0x50, // CALL; POP result
        0x00, // STOP
    ]);
    code
}

/// Calls `first` and then `second`, discarding both results, before stopping.
fn call_two_then_stop(first: Address, second: Address) -> Vec<u8> {
    let mut code = Vec::new();
    for child in [first, second] {
        code.extend_from_slice(&[
            0x5f, 0x5f, 0x5f, 0x5f, 0x5f, // Empty CALL output/input and zero value
            0x73, // PUSH20 child address
        ]);
        code.extend_from_slice(child.as_slice());
        code.extend_from_slice(&[
            0x61, 0xff, 0xff, // PUSH2 65535 gas
            0xf1, 0x50, // CALL; POP result
        ]);
    }
    code.push(0x00); // STOP
    code
}

#[test]
fn inspector_captures_constructor_revert_reason() {
    let (result, mut inspector) = run_trampoline(CREATE_REVERT_TRAMPOLINE);

    assert!(matches!(result, ExecutionResult::Revert { .. }));
    assert_eq!(
        inspector.terminal_revert_reason().as_deref(),
        Some("0xdeadbeef")
    );
    assert!(inspector.take_contract_creations().is_empty());

    let trace = inspector
        .trace_entries()
        .expect("completed simulation should produce a complete trace");
    let [parent, create] = trace.as_slice() else {
        panic!("expected parent CALL and child CREATE, got {trace:?}");
    };

    assert_eq!(parent.kind, TraceKind::Call);
    assert_eq!(parent.depth, 0);
    assert_eq!(parent.outcome, TraceOutcome::Revert);
    assert_eq!(parent.revert_reason, None);

    assert_eq!(create.kind, TraceKind::Create);
    assert_eq!(create.depth, 1);
    assert_eq!(create.outcome, TraceOutcome::Revert);
    assert_eq!(create.revert_reason.as_deref(), Some("0xdeadbeef"));
    assert_eq!(create.to, None);
    assert_eq!(create.selector, None);

    let create_json = serde_json::to_value(create).expect("serialize CREATE trace entry");
    assert_eq!(create_json["kind"], "create");
    assert_eq!(create_json["outcome"], "revert");
    assert_eq!(create_json["depth"], 1);
    assert_eq!(create_json["revertReason"], "0xdeadbeef");
    assert!(create_json.get("to").is_none());
    assert!(create_json.get("selector").is_none());
}

#[test]
fn nested_create_revert_does_not_overwrite_successful_parent_outcome() {
    let (result, inspector) = run_trampoline(CAUGHT_CREATE_REVERT_TRAMPOLINE);

    assert!(matches!(result, ExecutionResult::Success { .. }));
    assert!(inspector.terminal_revert_reason().is_none());
    let trace = inspector
        .trace_entries()
        .expect("completed simulation should produce a complete trace");
    let [parent, create] = trace.as_slice() else {
        panic!("expected parent CALL and child CREATE, got {trace:?}");
    };
    assert_eq!(parent.kind, TraceKind::Call);
    assert_eq!(parent.outcome, TraceOutcome::Success);
    assert_eq!(create.kind, TraceKind::Create);
    assert_eq!(create.outcome, TraceOutcome::Revert);
    assert_eq!(create.revert_reason.as_deref(), Some("0xdeadbeef"));
}

#[test]
fn inspector_traces_successful_create_with_deployed_address() {
    let (result, mut inspector) = run_trampoline(SUCCESSFUL_CREATE_TRAMPOLINE);

    assert!(matches!(result, ExecutionResult::Success { .. }));
    let creations = inspector.take_contract_creations();
    assert_eq!(creations.len(), 1);
    let (deployer, deployed) = creations[0];
    assert_eq!(deployer, TRAMPOLINE);
    assert_ne!(deployed, Address::ZERO);

    let trace = inspector
        .trace_entries()
        .expect("completed simulation should produce a complete trace");
    let [parent, create] = trace.as_slice() else {
        panic!("expected parent CALL and child CREATE, got {trace:?}");
    };
    assert_eq!(parent.kind, TraceKind::Call);
    assert_eq!(parent.depth, 0);
    assert_eq!(parent.outcome, TraceOutcome::Success);
    assert_eq!(create.kind, TraceKind::Create);
    assert_eq!(create.depth, 1);
    assert_eq!(create.outcome, TraceOutcome::Success);
    assert_eq!(create.to, Some(deployed));
    assert_eq!(create.selector, None);
    assert_eq!(create.revert_reason, None);
}

#[test]
fn terminal_parent_revert_ignores_earlier_caught_child() {
    let trampoline = call_then_revert(REVERTING_CHILD, None, [0xbb; 4]);
    let (result, inspector) = run_trampoline_with_input(
        &trampoline,
        &[(REVERTING_CHILD, CHILD_REVERT)],
        Bytes::new(),
    );

    assert!(matches!(result, ExecutionResult::Revert { .. }));
    assert_eq!(
        inspector.terminal_revert_reason().as_deref(),
        Some("0xbbbbbbbb")
    );

    let trace = inspector
        .trace_entries()
        .expect("completed simulation should produce a complete trace");
    let [parent, child] = trace.as_slice() else {
        panic!("expected parent and child CALL frames, got {trace:?}");
    };
    assert_eq!(parent.revert_reason.as_deref(), Some("0xbbbbbbbb"));
    assert_eq!(child.revert_reason.as_deref(), Some("0xaaaaaaaa"));
}

#[test]
fn safe_execution_failed_retains_revert_across_successful_catching_frame() {
    // Safe.execTransactionFromModule catches the target revert and returns
    // false successfully. Safe4337Module.executeUserOp then replaces that
    // return value with the exact `ExecutionFailed()` custom error.
    let catching_child = call_then_stop(REVERTING_CHILD);
    let trampoline = call_then_revert(
        CATCHING_CHILD,
        Some(EXEC_TRANSACTION_FROM_MODULE_SELECTOR),
        EXECUTION_FAILED_SELECTOR,
    );
    let (result, inspector) = run_trampoline_with_input(
        &trampoline,
        &[
            (CATCHING_CHILD, &catching_child),
            (REVERTING_CHILD, CHILD_REVERT),
        ],
        Bytes::copy_from_slice(&EXECUTE_USER_OP_SELECTOR),
    );

    assert!(matches!(result, ExecutionResult::Revert { .. }));
    assert_eq!(
        inspector.terminal_revert_reason().as_deref(),
        Some("0xaaaaaaaa")
    );

    let trace = inspector
        .trace_entries()
        .expect("completed simulation should produce a complete trace");
    assert_eq!(trace[0].selector.as_deref(), Some("0x7bb37428"));
    assert_eq!(trace[0].revert_reason.as_deref(), Some("0xacfdb444"));
    assert_eq!(trace[1].outcome, TraceOutcome::Success);
    assert_eq!(trace[1].selector.as_deref(), Some("0x468721a7"));
    assert_eq!(trace[2].revert_reason.as_deref(), Some("0xaaaaaaaa"));
}

#[test]
fn safe_execution_failed_retains_revert_before_successful_module_guard() {
    // Safe 1.5 calls checkAfterModuleExecution after the target. The trailing
    // successful guard call must not hide the target revert that caused
    // execTransactionFromModule to return false.
    let catching_child = call_two_then_stop(REVERTING_CHILD, MODULE_GUARD);
    let trampoline = call_then_revert(
        CATCHING_CHILD,
        Some(EXEC_TRANSACTION_FROM_MODULE_SELECTOR),
        EXECUTION_FAILED_SELECTOR,
    );
    let (result, inspector) = run_trampoline_with_input(
        &trampoline,
        &[
            (CATCHING_CHILD, &catching_child),
            (REVERTING_CHILD, CHILD_REVERT),
            (MODULE_GUARD, &[0x00]),
        ],
        Bytes::copy_from_slice(&EXECUTE_USER_OP_SELECTOR),
    );

    assert!(matches!(result, ExecutionResult::Revert { .. }));
    assert_eq!(
        inspector.terminal_revert_reason().as_deref(),
        Some("0xaaaaaaaa")
    );

    let trace = inspector
        .trace_entries()
        .expect("completed simulation should produce a complete trace");
    let [execute_user_op, safe, target, guard] = trace.as_slice() else {
        panic!("expected executeUserOp, Safe, target, and guard frames, got {trace:?}");
    };
    assert_eq!(execute_user_op.outcome, TraceOutcome::Revert);
    assert_eq!(safe.outcome, TraceOutcome::Success);
    assert_eq!(target.revert_reason.as_deref(), Some("0xaaaaaaaa"));
    assert_eq!(guard.outcome, TraceOutcome::Success);
}
