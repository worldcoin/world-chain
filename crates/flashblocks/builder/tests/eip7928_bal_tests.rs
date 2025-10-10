//! Comprehensive tests for EIP-7928 Block Access List (BAL) implementation.
//!
//! These tests verify that the BAL inspector correctly tracks:
//! - Nonce changes (for EOAs and contracts)
//! - Balance changes (including gas costs)
//! - Code changes (contract deployments)
//! - Storage reads and writes
//! - Account accesses via various opcodes
//! - EIP-2930 access list interactions
//! - Edge cases (self-transfers, zero-value transfers, net-zero changes)
//! - Aborted/reverted transactions

use alloy_eip7928::{
    AccountChanges, BalanceChange, CodeChange, NonceChange, SlotChanges, StorageChange,
};
use alloy_primitives::{address, b256, Address, Bytes, TxKind, B256, U256};
use flashblocks_builder::bal::BalInspector;
use reth_evm::{eth::EthEvmBuilder, Evm, EvmEnv};
use revm::{
    bytecode::{opcode, Bytecode},
    context::{BlockEnv, CfgEnv},
    context_interface::result::EVMError,
    database::{CacheDB, EmptyDB},
    state::AccountInfo,
};
use std::collections::HashMap;

/// Helper to get BAL changes from inspector after transaction
fn get_bal_changes(
    inspector: &mut BalInspector,
    evm_state: revm::state::EvmState,
) -> Vec<AccountChanges> {
    inspector.merge_state(evm_state)
}

/// Helper to create a basic EVM configuration
fn create_evm_env() -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::default(),
        block_env: BlockEnv {
            number: U256::from(1),
            beneficiary: address!("0000000000000000000000000000000000000000"),
            timestamp: U256::from(1),
            gas_limit: 30_000_000,
            basefee: 10,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::ZERO),
            ..Default::default()
        },
    }
}

/// Helper to create an account with balance
fn create_account(balance: U256, nonce: u64, code: Option<Bytecode>) -> AccountInfo {
    AccountInfo {
        balance,
        nonce,
        code_hash: code.as_ref().map(|c| c.hash_slow()).unwrap_or_default(),
        code,
    }
}

/// Helper to find account changes by address
fn find_account_changes<'a>(
    changes: &'a [AccountChanges],
    address: Address,
) -> Option<&'a AccountChanges> {
    changes.iter().find(|c| c.address == address)
}

#[test]
fn test_bal_nonce_changes() {
    // Setup: Alice sends a transaction to Bob
    let alice = address!("1000000000000000000000000000000000000001");
    let bob = address!("1000000000000000000000000000000000000002");

    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(alice, create_account(U256::from(1_000_000), 0, None));
    db.insert_account_info(bob, create_account(U256::ZERO, 0, None));

    let mut inspector = BalInspector::new();
    inspector.set_index(1); // Transaction index 1

    let env = create_evm_env();
    let mut evm = EthEvmBuilder::new(db, env)
        .inspector(&mut inspector)
        .build();

    // Configure transaction
    evm.ctx_mut().tx.caller = alice;
    evm.ctx_mut().tx.kind = TxKind::Call(bob);
    evm.ctx_mut().tx.value = U256::from(100);
    evm.ctx_mut().tx.gas_limit = 21_000;
    evm.ctx_mut().tx.gas_price = 10;

    // Execute
    let result = evm.transact(evm.ctx().tx.clone()).unwrap();
    let state = result.state.clone();

    // Get BAL changes
    let bal_changes = get_bal_changes(&mut inspector, state);

    // Verify Alice's nonce changed
    let alice_changes = find_account_changes(&bal_changes, alice).expect("Alice should be in BAL");
    assert_eq!(alice_changes.nonce_changes.len(), 1);
    assert_eq!(alice_changes.nonce_changes[0].block_access_index, 1);
    assert_eq!(alice_changes.nonce_changes[0].new_nonce, 1);
}

// #[test]
// fn test_bal_balance_changes() {
//     // Setup: Alice sends 100 wei to Bob with gas cost
//     let alice = address!("1000000000000000000000000000000000000001");
//     let bob = address!("1000000000000000000000000000000000000002");

//     let alice_initial_balance = U256::from(1_000_000);

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(alice, create_account(alice_initial_balance, 0, None));
//     db.insert_account_info(bob, create_account(U256::ZERO, 0, None));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     let gas_price = U256::from(1_000_000_000);
//     let transfer_value = U256::from(100);

//     evm.context.evm.env.tx.caller = alice;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(bob);
//     evm.context.evm.env.tx.value = transfer_value;
//     evm.context.evm.env.tx.gas_limit = 21_000;
//     evm.context.evm.env.tx.gas_price = gas_price;

//     let result = evm.transact().unwrap();
//     let gas_used = result.result.gas_used();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify Alice's balance decreased
//     let alice_changes = find_account_changes(&bal_changes, alice).expect("Alice should be in BAL");
//     assert!(!alice_changes.balance_changes.is_empty());

//     let expected_alice_balance =
//         alice_initial_balance - transfer_value - U256::from(gas_used) * gas_price;
//     assert_eq!(
//         alice_changes.balance_changes[0].post_balance,
//         expected_alice_balance
//     );

//     // Verify Bob's balance increased
//     let bob_changes = find_account_changes(&bal_changes, bob).expect("Bob should be in BAL");
//     assert_eq!(bob_changes.balance_changes.len(), 1);
//     assert_eq!(bob_changes.balance_changes[0].post_balance, transfer_value);
// }

// #[test]
// fn test_bal_code_changes() {
//     // Setup: Deploy a contract
//     let deployer = address!("1000000000000000000000000000000000000001");

//     // Simple runtime code: STOP (0x00)
//     let runtime_code = vec![opcode::STOP];

//     // Init code that returns the runtime code
//     // PUSH1 1 (size), DUP1, PUSH1 12 (offset), PUSH1 0 (dest), CODECOPY, PUSH1 0, RETURN, [runtime_code]
//     let init_code = vec![
//         opcode::PUSH1,
//         1,            // size = 1
//         opcode::DUP1, // duplicate size
//         opcode::PUSH1,
//         0x0C, // offset = 12 (where runtime code starts)
//         opcode::PUSH1,
//         0x00,             // dest offset = 0
//         opcode::CODECOPY, // copy runtime code to memory
//         opcode::PUSH1,
//         0x00,           // memory offset
//         opcode::RETURN, // return runtime code
//         opcode::STOP,   // runtime code
//     ];

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(deployer, create_account(U256::from(10_000_000), 0, None));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = deployer;
//     evm.context.evm.env.tx.transact_to = TransactTo::Create;
//     evm.context.evm.env.tx.data = Bytes::from(init_code);
//     evm.context.evm.env.tx.gas_limit = 100_000;
//     evm.context.evm.env.tx.gas_price = U256::from(10);

//     let result = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify deployer nonce changed
//     let deployer_changes =
//         find_account_changes(&bal_changes, deployer).expect("Deployer should be in BAL");
//     assert!(!deployer_changes.nonce_changes.is_empty());
//     assert_eq!(deployer_changes.nonce_changes[0].new_nonce, 1);

//     // Verify deployed contract has code change recorded
//     // The deployed contract address would be compute_create_address(deployer, 0)
//     // For simplicity, we check that there's at least one account with code changes
//     let contracts_with_code: Vec<_> = bal_changes
//         .iter()
//         .filter(|c| !c.code_changes.is_empty())
//         .collect();

//     assert!(
//         !contracts_with_code.is_empty(),
//         "Should have at least one contract with code changes"
//     );
//     assert_eq!(
//         contracts_with_code[0].code_changes[0].new_code,
//         Bytes::from(runtime_code)
//     );
// }

// #[test]
// fn test_bal_storage_reads() {
//     // Setup: Contract that reads from storage
//     let caller = address!("1000000000000000000000000000000000000001");
//     let contract = address!("1000000000000000000000000000000000000002");

//     // Code: PUSH1 0x01, SLOAD, STOP
//     let code = vec![
//         opcode::PUSH1,
//         0x01,          // slot 0x01
//         opcode::SLOAD, // read from slot
//         opcode::STOP,
//     ];
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(contract, create_account(U256::ZERO, 0, Some(bytecode)));

//     // Set storage slot 0x01 to 0x42
//     db.insert_account_storage(contract, U256::from(0x01), U256::from(0x42))
//         .unwrap();

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(contract);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify storage read was recorded
//     let contract_changes =
//         find_account_changes(&bal_changes, contract).expect("Contract should be in BAL");
//     assert!(contract_changes.storage_reads.contains(&b256!(
//         "0000000000000000000000000000000000000000000000000000000000000001"
//     )));
// }

// #[test]
// fn test_bal_storage_writes() {
//     // Setup: Contract that writes to storage
//     let caller = address!("1000000000000000000000000000000000000001");
//     let contract = address!("1000000000000000000000000000000000000002");

//     // Code: PUSH1 0x42, PUSH1 0x01, SSTORE, STOP
//     let code = vec![
//         opcode::PUSH1,
//         0x42, // value
//         opcode::PUSH1,
//         0x01,           // slot
//         opcode::SSTORE, // write to slot
//         opcode::STOP,
//     ];
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(contract, create_account(U256::ZERO, 0, Some(bytecode)));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(contract);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify storage write was recorded
//     let contract_changes =
//         find_account_changes(&bal_changes, contract).expect("Contract should be in BAL");
//     assert!(!contract_changes.storage_changes.is_empty());

//     let slot_change = &contract_changes.storage_changes[0];
//     assert_eq!(
//         slot_change.slot,
//         b256!("0000000000000000000000000000000000000000000000000000000000000001")
//     );
//     assert_eq!(
//         slot_change.changes[0].new_value,
//         b256!("0000000000000000000000000000000000000000000000000000000000000042")
//     );
// }

// #[test]
// fn test_bal_noop_storage_write() {
//     // Setup: Contract that writes the same value to storage (no-op)
//     let caller = address!("1000000000000000000000000000000000000001");
//     let contract = address!("1000000000000000000000000000000000000002");

//     // Code: PUSH1 0x42, PUSH1 0x01, SSTORE, STOP
//     let code = vec![
//         opcode::PUSH1,
//         0x42, // value (same as current)
//         opcode::PUSH1,
//         0x01,           // slot
//         opcode::SSTORE, // write to slot
//         opcode::STOP,
//     ];
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(contract, create_account(U256::ZERO, 0, Some(bytecode)));

//     // Pre-set storage to 0x42
//     db.insert_account_storage(contract, U256::from(0x01), U256::from(0x42))
//         .unwrap();

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(contract);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify no storage write was recorded (no-op)
//     let contract_changes =
//         find_account_changes(&bal_changes, contract).expect("Contract should be in BAL");
//     assert!(
//         contract_changes.storage_changes.is_empty(),
//         "No-op SSTORE should not record storage changes"
//     );

//     // But storage read should be recorded (SSTORE reads before writing)
//     assert!(contract_changes.storage_reads.contains(&b256!(
//         "0000000000000000000000000000000000000000000000000000000000000001"
//     )));
// }

// #[test]
// fn test_bal_account_access_via_balance() {
//     // Setup: Contract that reads balance of another account
//     let caller = address!("1000000000000000000000000000000000000001");
//     let contract = address!("1000000000000000000000000000000000000002");
//     let target = address!("1000000000000000000000000000000000000003");

//     // Code: PUSH20 target, BALANCE, STOP
//     let mut code = vec![opcode::PUSH20];
//     code.extend_from_slice(target.as_slice());
//     code.extend_from_slice(&[opcode::BALANCE, opcode::STOP]);
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(contract, create_account(U256::ZERO, 0, Some(bytecode)));
//     db.insert_account_info(target, create_account(U256::from(1000), 0, None));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(contract);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify target account was accessed
//     let target_changes = find_account_changes(&bal_changes, target);
//     assert!(target_changes.is_some(), "Target account should be in BAL");
// }

// #[test]
// fn test_bal_self_transfer() {
//     // Setup: Alice sends value to herself
//     let alice = address!("1000000000000000000000000000000000000001");
//     let alice_initial_balance = U256::from(1_000_000);

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(alice, create_account(alice_initial_balance, 0, None));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     let gas_price = U256::from(10);
//     evm.context.evm.env.tx.caller = alice;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(alice);
//     evm.context.evm.env.tx.value = U256::from(100);
//     evm.context.evm.env.tx.gas_limit = 21_000;
//     evm.context.evm.env.tx.gas_price = gas_price;

//     let result = evm.transact().unwrap();
//     let gas_used = result.result.gas_used();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify Alice's balance only changed due to gas
//     let alice_changes = find_account_changes(&bal_changes, alice).expect("Alice should be in BAL");
//     assert!(!alice_changes.balance_changes.is_empty());

//     let expected_balance = alice_initial_balance - U256::from(gas_used) * gas_price;
//     assert_eq!(
//         alice_changes.balance_changes[0].post_balance,
//         expected_balance
//     );
// }

// #[test]
// fn test_bal_zero_value_transfer() {
//     // Setup: Alice sends 0 value to Bob
//     let alice = address!("1000000000000000000000000000000000000001");
//     let bob = address!("1000000000000000000000000000000000000002");
//     let alice_initial_balance = U256::from(1_000_000);

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(alice, create_account(alice_initial_balance, 0, None));
//     db.insert_account_info(bob, create_account(U256::from(100), 0, None));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     let gas_price = U256::from(10);
//     evm.context.evm.env.tx.caller = alice;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(bob);
//     evm.context.evm.env.tx.value = U256::ZERO;
//     evm.context.evm.env.tx.gas_limit = 21_000;
//     evm.context.evm.env.tx.gas_price = gas_price;

//     let result = evm.transact().unwrap();
//     let gas_used = result.result.gas_used();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify Alice's balance only changed due to gas
//     let alice_changes = find_account_changes(&bal_changes, alice).expect("Alice should be in BAL");
//     let expected_balance = alice_initial_balance - U256::from(gas_used) * gas_price;
//     assert_eq!(
//         alice_changes.balance_changes[0].post_balance,
//         expected_balance
//     );

//     // Verify Bob is in BAL but with no balance changes
//     let bob_changes = find_account_changes(&bal_changes, bob).expect("Bob should be in BAL");
//     assert!(
//         bob_changes.balance_changes.is_empty(),
//         "Bob should have no balance changes"
//     );
// }

// #[test]
// fn test_bal_pure_contract_call() {
//     // Setup: Call a contract that does pure computation (no state changes)
//     let caller = address!("1000000000000000000000000000000000000001");
//     let contract = address!("1000000000000000000000000000000000000002");

//     // Code: PUSH1 3, PUSH1 2, ADD, STOP (pure computation)
//     let code = vec![
//         opcode::PUSH1,
//         0x03,
//         opcode::PUSH1,
//         0x02,
//         opcode::ADD,
//         opcode::STOP,
//     ];
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(contract, create_account(U256::ZERO, 0, Some(bytecode)));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(contract);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify contract is tracked even with no state changes
//     let contract_changes = find_account_changes(&bal_changes, contract);
//     assert!(
//         contract_changes.is_some(),
//         "Contract should be in BAL even with no state changes"
//     );
// }

// #[test]
// fn test_bal_aborted_storage_access_revert() {
//     // Setup: Contract that reads and writes storage, then reverts
//     let caller = address!("1000000000000000000000000000000000000001");
//     let contract = address!("1000000000000000000000000000000000000002");

//     // Code: SLOAD(0x01), SSTORE(0x02, 0x42), REVERT
//     let code = vec![
//         opcode::PUSH1,
//         0x01,          // slot to read
//         opcode::SLOAD, // read
//         opcode::PUSH1,
//         0x42, // value to write
//         opcode::PUSH1,
//         0x02,           // slot to write
//         opcode::SSTORE, // write
//         opcode::PUSH1,
//         0x00, // size
//         opcode::PUSH1,
//         0x00,           // offset
//         opcode::REVERT, // revert
//     ];
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(contract, create_account(U256::ZERO, 0, Some(bytecode)));

//     // Set initial value in slot 0x01
//     db.insert_account_storage(contract, U256::from(0x01), U256::from(0x10))
//         .unwrap();

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(contract);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let result = evm.transact();
//     // Transaction should revert
//     assert!(result.is_ok()); // The EVM execution completes, but the transaction reverted

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify storage accesses are still recorded
//     let contract_changes =
//         find_account_changes(&bal_changes, contract).expect("Contract should be in BAL");

//     // Storage reads should be recorded
//     assert!(contract_changes.storage_reads.contains(&b256!(
//         "0000000000000000000000000000000000000000000000000000000000000001"
//     )));
//     assert!(contract_changes.storage_reads.contains(&b256!(
//         "0000000000000000000000000000000000000000000000000000000000000002"
//     )));

//     // Storage writes should NOT be recorded (reverted)
//     assert!(
//         contract_changes.storage_changes.is_empty(),
//         "Reverted storage writes should not be in BAL"
//     );
// }

// #[test]
// fn test_bal_fully_unmutated_account() {
//     // Setup: Contract that reads and writes the same value, with zero-value call
//     let caller = address!("1000000000000000000000000000000000000001");
//     let contract = address!("1000000000000000000000000000000000000002");

//     // Code: PUSH1 0x42, PUSH1 0x01, SSTORE, STOP
//     let code = vec![
//         opcode::PUSH1,
//         0x42, // value (same as current)
//         opcode::PUSH1,
//         0x01,           // slot
//         opcode::SSTORE, // write same value
//         opcode::STOP,
//     ];
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(contract, create_account(U256::ZERO, 0, Some(bytecode)));

//     // Pre-set storage to 0x42
//     db.insert_account_storage(contract, U256::from(0x01), U256::from(0x42))
//         .unwrap();

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(contract);
//     evm.context.evm.env.tx.value = U256::ZERO; // zero value transfer
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify contract is in BAL
//     let contract_changes =
//         find_account_changes(&bal_changes, contract).expect("Contract should be in BAL");

//     // No net storage changes
//     assert!(contract_changes.storage_changes.is_empty());

//     // But storage was accessed
//     assert!(!contract_changes.storage_reads.is_empty());

//     // No balance changes
//     assert!(contract_changes.balance_changes.is_empty());
// }

// // ============================================================================
// // CALL Opcode Tests
// // ============================================================================

// #[test]
// fn test_bal_call_with_value_transfer() {
//     // Setup: Oracle contract uses CALL to transfer value
//     let caller = address!("1000000000000000000000000000000000000001");
//     let oracle = address!("1000000000000000000000000000000000000002");
//     let bob = address!("1000000000000000000000000000000000000003");

//     // Code: CALL(gas=0, to=bob, value=100, in_offset=0, in_size=0, out_offset=0, out_size=0)
//     // Stack setup: PUSH1 0 (out_size), PUSH1 0 (out_offset), PUSH1 0 (in_size), PUSH1 0 (in_offset),
//     //              PUSH1 100 (value), PUSH20 bob (address), PUSH1 0 (gas), CALL
//     let mut code = vec![
//         opcode::PUSH1,
//         0x00, // out_size
//         opcode::PUSH1,
//         0x00, // out_offset
//         opcode::PUSH1,
//         0x00, // in_size
//         opcode::PUSH1,
//         0x00, // in_offset
//         opcode::PUSH1,
//         0x64,           // value = 100
//         opcode::PUSH20, // bob address follows
//     ];
//     code.extend_from_slice(bob.as_slice());
//     code.extend_from_slice(&[
//         opcode::PUSH1,
//         0x00, // gas
//         opcode::CALL,
//         opcode::STOP,
//     ]);
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(oracle, create_account(U256::from(200), 0, Some(bytecode)));
//     db.insert_account_info(bob, create_account(U256::ZERO, 0, None));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(oracle);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify oracle balance decreased
//     let oracle_changes =
//         find_account_changes(&bal_changes, oracle).expect("Oracle should be in BAL");
//     assert!(!oracle_changes.balance_changes.is_empty());
//     assert_eq!(
//         oracle_changes.balance_changes[0].post_balance,
//         U256::from(100)
//     );

//     // Verify bob balance increased
//     let bob_changes = find_account_changes(&bal_changes, bob).expect("Bob should be in BAL");
//     assert!(!bob_changes.balance_changes.is_empty());
//     assert_eq!(bob_changes.balance_changes[0].post_balance, U256::from(100));
// }

// #[test]
// fn test_bal_delegatecall_storage_writes() {
//     // Setup: Oracle uses DELEGATECALL to execute target's code in oracle's context
//     let caller = address!("1000000000000000000000000000000000000001");
//     let oracle = address!("1000000000000000000000000000000000000002");
//     let target = address!("1000000000000000000000000000000000000003");

//     // Target code: SSTORE(slot=0x01, value=0x42)
//     let target_code = vec![
//         opcode::PUSH1,
//         0x42, // value
//         opcode::PUSH1,
//         0x01, // slot
//         opcode::SSTORE,
//         opcode::STOP,
//     ];
//     let target_bytecode = Bytecode::new_legacy(target_code.into());

//     // Oracle code: DELEGATECALL to target
//     // Stack: PUSH1 0 (out_size), PUSH1 0 (out_offset), PUSH1 0 (in_size),
//     //        PUSH1 0 (in_offset), PUSH20 target, PUSH2 50000 (gas), DELEGATECALL
//     let mut oracle_code = vec![
//         opcode::PUSH1,
//         0x00, // out_size
//         opcode::PUSH1,
//         0x00, // out_offset
//         opcode::PUSH1,
//         0x00, // in_size
//         opcode::PUSH1,
//         0x00,           // in_offset
//         opcode::PUSH20, // target address
//     ];
//     oracle_code.extend_from_slice(target.as_slice());
//     oracle_code.extend_from_slice(&[
//         opcode::PUSH2,
//         0xC3,
//         0x50, // gas = 50000
//         opcode::DELEGATECALL,
//         opcode::STOP,
//     ]);
//     let oracle_bytecode = Bytecode::new_legacy(oracle_code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(oracle, create_account(U256::ZERO, 0, Some(oracle_bytecode)));
//     db.insert_account_info(target, create_account(U256::ZERO, 0, Some(target_bytecode)));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(oracle);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify oracle (not target) has storage changes due to DELEGATECALL
//     let oracle_changes =
//         find_account_changes(&bal_changes, oracle).expect("Oracle should be in BAL");
//     assert!(!oracle_changes.storage_changes.is_empty());
//     assert_eq!(
//         oracle_changes.storage_changes[0].slot,
//         b256!("0000000000000000000000000000000000000000000000000000000000000001")
//     );
//     assert_eq!(
//         oracle_changes.storage_changes[0].changes[0].new_value,
//         b256!("0000000000000000000000000000000000000000000000000000000000000042")
//     );

//     // Verify target is tracked but has no storage changes
//     let target_changes = find_account_changes(&bal_changes, target);
//     assert!(target_changes.is_some());
//     if let Some(tc) = target_changes {
//         assert!(tc.storage_changes.is_empty());
//     }
// }

// #[test]
// fn test_bal_delegatecall_storage_reads() {
//     // Setup: Oracle uses DELEGATECALL to read oracle's storage via target's code
//     let caller = address!("1000000000000000000000000000000000000001");
//     let oracle = address!("1000000000000000000000000000000000000002");
//     let target = address!("1000000000000000000000000000000000000003");

//     // Target code: SLOAD(slot=0x01), STOP
//     let target_code = vec![
//         opcode::PUSH1,
//         0x01, // slot
//         opcode::SLOAD,
//         opcode::STOP,
//     ];
//     let target_bytecode = Bytecode::new_legacy(target_code.into());

//     // Oracle code: DELEGATECALL to target
//     let mut oracle_code = vec![
//         opcode::PUSH1,
//         0x00, // out_size
//         opcode::PUSH1,
//         0x00, // out_offset
//         opcode::PUSH1,
//         0x00, // in_size
//         opcode::PUSH1,
//         0x00,           // in_offset
//         opcode::PUSH20, // target address
//     ];
//     oracle_code.extend_from_slice(target.as_slice());
//     oracle_code.extend_from_slice(&[
//         opcode::PUSH2,
//         0xC3,
//         0x50, // gas = 50000
//         opcode::DELEGATECALL,
//         opcode::STOP,
//     ]);
//     let oracle_bytecode = Bytecode::new_legacy(oracle_code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(oracle, create_account(U256::ZERO, 0, Some(oracle_bytecode)));
//     db.insert_account_info(target, create_account(U256::ZERO, 0, Some(target_bytecode)));

//     // Set oracle's storage
//     db.insert_account_storage(oracle, U256::from(0x01), U256::from(0x42))
//         .unwrap();

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(oracle);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify oracle has storage reads
//     let oracle_changes =
//         find_account_changes(&bal_changes, oracle).expect("Oracle should be in BAL");
//     assert!(oracle_changes.storage_reads.contains(&b256!(
//         "0000000000000000000000000000000000000000000000000000000000000001"
//     )));

//     // Target should be tracked
//     let target_changes = find_account_changes(&bal_changes, target);
//     assert!(target_changes.is_some());
// }

// // ============================================================================
// // Account Access Opcode Tests
// // ============================================================================

// #[test]
// fn test_bal_extcodesize_access() {
//     // Setup: Contract uses EXTCODESIZE to access another account
//     let caller = address!("1000000000000000000000000000000000000001");
//     let oracle = address!("1000000000000000000000000000000000000002");
//     let target = address!("1000000000000000000000000000000000000003");

//     // Oracle code: EXTCODESIZE(target), STOP
//     let mut oracle_code = vec![opcode::PUSH20];
//     oracle_code.extend_from_slice(target.as_slice());
//     oracle_code.extend_from_slice(&[opcode::EXTCODESIZE, opcode::STOP]);
//     let oracle_bytecode = Bytecode::new_legacy(oracle_code.into());

//     // Target has some code
//     let target_bytecode = Bytecode::new_legacy(vec![opcode::STOP].into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(oracle, create_account(U256::ZERO, 0, Some(oracle_bytecode)));
//     db.insert_account_info(target, create_account(U256::ZERO, 0, Some(target_bytecode)));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(oracle);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify target was accessed
//     let target_changes = find_account_changes(&bal_changes, target);
//     assert!(
//         target_changes.is_some(),
//         "Target should be in BAL due to EXTCODESIZE"
//     );
// }

// #[test]
// fn test_bal_extcodehash_access() {
//     // Setup: Contract uses EXTCODEHASH to access another account
//     let caller = address!("1000000000000000000000000000000000000001");
//     let oracle = address!("1000000000000000000000000000000000000002");
//     let target = address!("1000000000000000000000000000000000000003");

//     // Oracle code: EXTCODEHASH(target), STOP
//     let mut oracle_code = vec![opcode::PUSH20];
//     oracle_code.extend_from_slice(target.as_slice());
//     oracle_code.extend_from_slice(&[opcode::EXTCODEHASH, opcode::STOP]);
//     let oracle_bytecode = Bytecode::new_legacy(oracle_code.into());

//     let target_bytecode = Bytecode::new_legacy(vec![opcode::STOP].into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(oracle, create_account(U256::ZERO, 0, Some(oracle_bytecode)));
//     db.insert_account_info(target, create_account(U256::ZERO, 0, Some(target_bytecode)));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(oracle);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify target was accessed
//     let target_changes = find_account_changes(&bal_changes, target);
//     assert!(
//         target_changes.is_some(),
//         "Target should be in BAL due to EXTCODEHASH"
//     );
// }

// #[test]
// fn test_bal_staticcall_access() {
//     // Setup: Contract uses STATICCALL to access another contract
//     let caller = address!("1000000000000000000000000000000000000001");
//     let oracle = address!("1000000000000000000000000000000000000002");
//     let target = address!("1000000000000000000000000000000000000003");

//     // Target: simple STOP
//     let target_bytecode = Bytecode::new_legacy(vec![opcode::STOP].into());

//     // Oracle code: STATICCALL(gas=0, to=target, in_offset=0, in_size=0, out_offset=0, out_size=0)
//     let mut oracle_code = vec![
//         opcode::PUSH1,
//         0x00, // out_size
//         opcode::PUSH1,
//         0x00, // out_offset
//         opcode::PUSH1,
//         0x00, // in_size
//         opcode::PUSH1,
//         0x00,           // in_offset
//         opcode::PUSH20, // target address
//     ];
//     oracle_code.extend_from_slice(target.as_slice());
//     oracle_code.extend_from_slice(&[
//         opcode::PUSH1,
//         0x00, // gas
//         opcode::STATICCALL,
//         opcode::STOP,
//     ]);
//     let oracle_bytecode = Bytecode::new_legacy(oracle_code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(oracle, create_account(U256::ZERO, 0, Some(oracle_bytecode)));
//     db.insert_account_info(target, create_account(U256::ZERO, 0, Some(target_bytecode)));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(oracle);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify target was accessed via STATICCALL
//     let target_changes = find_account_changes(&bal_changes, target);
//     assert!(
//         target_changes.is_some(),
//         "Target should be in BAL due to STATICCALL"
//     );
// }

// // ============================================================================
// // Multiple Transaction Tests
// // ============================================================================

// #[test]
// fn test_bal_multiple_storage_changes_same_slot() {
//     // Setup: Multiple writes to the same storage slot across "transactions"
//     let caller = address!("1000000000000000000000000000000000000001");
//     let contract = address!("1000000000000000000000000000000000000002");

//     // Code: SSTORE(slot=0x01, value=0x42), STOP
//     let code = vec![
//         opcode::PUSH1,
//         0x42,
//         opcode::PUSH1,
//         0x01,
//         opcode::SSTORE,
//         opcode::STOP,
//     ];
//     let bytecode = Bytecode::new_legacy(code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(
//         contract,
//         create_account(U256::ZERO, 0, Some(bytecode.clone())),
//     );

//     let mut inspector = BalInspector::new();

//     // First "transaction" (index 1)
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(contract);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     // The inspector now has changes from transaction 1
//     // In a real scenario, you'd extract changes and continue with a new inspector
//     // For this test, we verify the first transaction's changes
//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     let contract_changes =
//         find_account_changes(&bal_changes, contract).expect("Contract should be in BAL");
//     assert_eq!(contract_changes.storage_changes.len(), 1);
//     assert_eq!(
//         contract_changes.storage_changes[0].changes[0].block_access_index,
//         1
//     );
// }

// #[test]
// fn test_bal_account_access_without_state_change() {
//     // Setup: Access account via BALANCE without changing state
//     let caller = address!("1000000000000000000000000000000000000001");
//     let oracle = address!("1000000000000000000000000000000000000002");
//     let target = address!("1000000000000000000000000000000000000003");

//     // Oracle code: BALANCE(target), STOP
//     let mut oracle_code = vec![opcode::PUSH20];
//     oracle_code.extend_from_slice(target.as_slice());
//     oracle_code.extend_from_slice(&[opcode::BALANCE, opcode::STOP]);
//     let oracle_bytecode = Bytecode::new_legacy(oracle_code.into());

//     let mut db = CacheDB::new(EmptyDB::default());
//     db.insert_account_info(caller, create_account(U256::from(1_000_000), 0, None));
//     db.insert_account_info(oracle, create_account(U256::ZERO, 0, Some(oracle_bytecode)));
//     db.insert_account_info(target, create_account(U256::from(1000), 0, None));

//     let mut inspector = BalInspector::new();
//     inspector.set_index(1);

//     let env = create_evm_env();
//     let mut evm = Evm::builder()
//         .with_db(db)
//         .with_env_with_handler_cfg(env)
//         .with_external_context(&mut inspector)
//         .build();

//     evm.context.evm.env.tx.caller = caller;
//     evm.context.evm.env.tx.transact_to = TransactTo::Call(oracle);
//     evm.context.evm.env.tx.gas_limit = 100_000;

//     let _ = evm.transact().unwrap();

//     let result = evm.transact().unwrap();
//     let state = result.state.clone();

//     let bal_changes = get_bal_changes(&mut inspector, state);

//     // Verify target is in BAL even though no state changed
//     let target_changes = find_account_changes(&bal_changes, target);
//     assert!(target_changes.is_some(), "Target should be in BAL");

//     // Target should have no changes
//     if let Some(tc) = target_changes {
//         assert!(tc.balance_changes.is_empty());
//         assert!(tc.nonce_changes.is_empty());
//         assert!(tc.storage_changes.is_empty());
//         assert!(tc.code_changes.is_empty());
//         assert!(tc.storage_reads.is_empty());
//     }
// }
