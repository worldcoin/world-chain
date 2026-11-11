// Projection fixtures are authored locally; no upstream test body is copied.

use super::*;
use alloy_primitives::{Address, LogData};
use revm::{
    context::result::{ExecutionResult, ResultAndState},
    interpreter::InstructionResult,
    state::{Account, EvmStorageSlot, TransactionId},
};
use revm_inspectors::tracing::{
    GethTraceBuilder,
    types::{CallKind, CallLog, CallTrace},
};
use std::{cell::Cell, collections::BTreeMap};

fn budget() -> ProjectionBudget {
    ProjectionBudget::new(ProjectionLimits {
        counts: ProjectionCounts {
            frames_and_logs: 100,
            accounts: 100,
            storage_entries: 100,
            data_bytes: 1_000_000,
        },
        max_call_depth: 16,
    })
}

fn gas() -> ResultGas {
    ResultGas::new_with_state_gas(42_000, 1_000, 0, 5_000)
}

fn node(index: usize, parent: Option<usize>, depth: usize) -> CallTraceNode {
    CallTraceNode {
        idx: index,
        parent,
        trace: CallTrace {
            depth,
            success: true,
            kind: CallKind::Call,
            caller: Address::repeat_byte(1),
            address: Address::repeat_byte(2),
            gas_limit: 90_000,
            gas_used: 5_000,
            status: Some(InstructionResult::Return),
            data: Bytes::from_static(&[1, 2]),
            output: Bytes::from_static(&[3]),
            ..Default::default()
        },
        logs: vec![CallLog {
            address: Address::repeat_byte(2),
            raw_log: LogData::new(vec![B256::repeat_byte(7)], Bytes::from_static(&[8, 9])).unwrap(),
            position: index as u64,
            index: index as u64,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn arena() -> Vec<CallTraceNode> {
    let mut nodes = vec![
        node(0, None, 0),
        node(1, Some(0), 1),
        node(2, Some(1), 2),
        node(3, Some(0), 1),
    ];
    nodes[0].children = vec![1, 3];
    nodes[1].children = vec![2];
    nodes[1].trace.success = false;
    nodes[1].trace.status = Some(InstructionResult::Revert);
    nodes[2].trace.kind = CallKind::StaticCall;
    nodes[3].trace.status = Some(InstructionResult::SelfDestruct);
    nodes[3].trace.selfdestruct_address = Some(Address::repeat_byte(2));
    nodes[3].trace.selfdestruct_refund_target = Some(Address::repeat_byte(3));
    nodes[3].trace.selfdestruct_transferred_value = Some(U256::from(17));
    nodes
}

#[test]
fn call_projection_matches_native_options_reverts_logs_and_gas() {
    for root_failed in [false, true] {
        let mut nodes = arena();
        if root_failed {
            nodes[0].trace.success = false;
            nodes[0].trace.status = Some(InstructionResult::Revert);
        }
        for only in [false, true] {
            for logs in [false, true] {
                for spec in [SpecId::PRAGUE, SpecId::AMSTERDAM] {
                    let config = CallConfig {
                        only_top_call: Some(only),
                        with_log: Some(logs),
                    };
                    let expected = GethTraceBuilder::new_borrowed(&nodes)
                        .with_spec_id(spec)
                        .geth_call_traces_with_result_gas(config, gas());
                    let actual =
                        project_call_trace(&nodes, config, gas(), spec, &mut budget(), &mut || {
                            Ok(())
                        })
                        .unwrap();
                    assert_eq!(actual, expected);
                }
            }
        }
    }
}

#[test]
fn call_projection_bounds_native_revert_and_panic_reason_variants() {
    let mut outputs = vec![
        Bytes::new(),
        Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
        Bytes::from_static(&[0xff, 0xfe]),
    ];
    for code in [0_u64, 1, 0x11, 0x12, 0x21, 0x22, 0x31, 0x32, 0x41, 0x51] {
        let mut data = vec![0x4e, 0x48, 0x7b, 0x71];
        data.extend_from_slice(&U256::from(code).to_be_bytes::<32>());
        outputs.push(Bytes::from(data));
    }
    let mut unknown = vec![0x4e, 0x48, 0x7b, 0x71];
    unknown.extend_from_slice(&[0xff; 32]);
    outputs.push(Bytes::from(unknown));
    for length in 4..36 {
        let mut truncated = vec![0x4e, 0x48, 0x7b, 0x71];
        truncated.resize(length, 0);
        outputs.push(Bytes::from(truncated));
    }
    // Original ABI fixture: Error(string), encoded without importing another decoder version.
    for reason in ["", "bounded native failure", "é🦀"] {
        let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
        data.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(reason.len()).to_be_bytes::<32>());
        data.extend_from_slice(reason.as_bytes());
        let padded = 4 + 64 + reason.len().div_ceil(32) * 32;
        data.resize(padded, 0);
        outputs.push(Bytes::from(data));
    }
    let mut invalid_utf8 = vec![0x08, 0xc3, 0x79, 0xa0];
    invalid_utf8.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
    invalid_utf8.extend_from_slice(&U256::from(64).to_be_bytes::<32>());
    invalid_utf8.extend_from_slice(&[0xff; 64]);
    outputs.push(Bytes::from(invalid_utf8));
    for invalid_offset in [false, true] {
        let mut malformed = vec![0x08, 0xc3, 0x79, 0xa0];
        let offset = if invalid_offset {
            U256::MAX
        } else {
            U256::from(32)
        };
        malformed.extend_from_slice(&offset.to_be_bytes::<32>());
        malformed.extend_from_slice(&U256::MAX.to_be_bytes::<32>());
        outputs.push(Bytes::from(malformed));
    }
    for output in outputs {
        let mut nodes = vec![node(0, None, 0)];
        nodes[0].logs.clear();
        nodes[0].trace.success = false;
        nodes[0].trace.status = Some(InstructionResult::Revert);
        nodes[0].trace.output = output;
        let config = CallConfig::default();
        let expected =
            GethTraceBuilder::new_borrowed(&nodes).geth_call_traces_with_result_gas(config, gas());
        if nodes[0].trace.output.starts_with(&[0x4e, 0x48, 0x7b, 0x71])
            && nodes[0].trace.output.len() < 36
        {
            assert!(expected.revert_reason.is_none());
        }
        let mut reference = budget();
        let actual = project_call_trace(
            &nodes,
            config,
            gas(),
            SpecId::PRAGUE,
            &mut reference,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(actual, expected);
        let retained = actual.input.len()
            + actual.output.as_ref().map_or(0, |bytes| bytes.len())
            + actual.revert_reason.as_ref().map_or(0, String::len);
        assert!(reference.used().data_bytes >= retained);
        let mut exact = ProjectionBudget::new(ProjectionLimits {
            counts: reference.used(),
            max_call_depth: 0,
        });
        project_call_trace(
            &nodes,
            config,
            gas(),
            SpecId::PRAGUE,
            &mut exact,
            &mut || Ok(()),
        )
        .unwrap();
        let mut below = ProjectionBudget::new(exact.limits);
        below.limits.counts.data_bytes -= 1;
        assert!(matches!(
            project_call_trace(
                &nodes,
                config,
                gas(),
                SpecId::PRAGUE,
                &mut below,
                &mut || Ok(())
            ),
            Err(ProjectionError::LimitExceeded(
                ProjectionResource::DataBytes
            ))
        ));
    }
}

#[test]
fn call_projection_empty_exact_limits_and_shared_counts() {
    let mut empty = ProjectionBudget::new(ProjectionLimits {
        counts: ProjectionCounts::default(),
        max_call_depth: 0,
    });
    assert!(matches!(
        project_call_trace(
            &[],
            CallConfig::default(),
            gas(),
            SpecId::PRAGUE,
            &mut empty,
            &mut || Ok(())
        ),
        Err(ProjectionError::LimitExceeded(
            ProjectionResource::FramesAndLogs
        ))
    ));
    empty.limits.counts.frames_and_logs = 1;
    assert_eq!(
        project_call_trace(
            &[],
            CallConfig::default(),
            gas(),
            SpecId::PRAGUE,
            &mut empty,
            &mut || Ok(())
        )
        .unwrap(),
        CallFrame::default()
    );
    assert_eq!(empty.used().frames_and_logs, 1);
    assert!(matches!(
        project_call_trace(
            &[],
            CallConfig::default(),
            gas(),
            SpecId::PRAGUE,
            &mut empty,
            &mut || Ok(())
        ),
        Err(ProjectionError::LimitExceeded(
            ProjectionResource::FramesAndLogs
        ))
    ));
    let nodes = arena();
    let config = CallConfig {
        with_log: Some(true),
        ..Default::default()
    };
    let mut reference = budget();
    project_call_trace(
        &nodes,
        config,
        gas(),
        SpecId::PRAGUE,
        &mut reference,
        &mut || Ok(()),
    )
    .unwrap();
    let exact = reference.used();
    let mut shared = ProjectionBudget::new(ProjectionLimits {
        counts: exact,
        max_call_depth: 2,
    });
    project_call_trace(
        &nodes,
        config,
        gas(),
        SpecId::PRAGUE,
        &mut shared,
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(shared.used(), exact);
    assert!(matches!(
        project_call_trace(
            &nodes,
            config,
            gas(),
            SpecId::PRAGUE,
            &mut shared,
            &mut || Ok(())
        ),
        Err(ProjectionError::LimitExceeded(_))
    ));
    for resource in [
        ProjectionResource::FramesAndLogs,
        ProjectionResource::DataBytes,
        ProjectionResource::CallDepth,
    ] {
        let mut limits = ProjectionLimits {
            counts: exact,
            max_call_depth: 2,
        };
        match resource {
            ProjectionResource::FramesAndLogs => limits.counts.frames_and_logs -= 1,
            ProjectionResource::DataBytes => limits.counts.data_bytes -= 1,
            ProjectionResource::CallDepth => limits.max_call_depth -= 1,
            _ => unreachable!(),
        }
        assert!(
            matches!(project_call_trace(&nodes, config, gas(), SpecId::PRAGUE, &mut ProjectionBudget::new(limits), &mut || Ok(())), Err(ProjectionError::LimitExceeded(r)) if r == resource)
        );
    }
}

#[test]
fn call_projection_rejects_parent_cycles_order_indices_and_depth() {
    for fault in 0..5 {
        let mut nodes = arena();
        match fault {
            0 => nodes[1].parent = Some(2),
            1 => nodes[1].parent = Some(1),
            2 => nodes[1].parent = None,
            3 => nodes[1].idx = usize::MAX,
            _ => nodes[1].trace.depth = usize::MAX,
        }
        assert!(matches!(
            project_call_trace(
                &nodes,
                CallConfig::default(),
                gas(),
                SpecId::PRAGUE,
                &mut budget(),
                &mut || Ok(())
            ),
            Err(ProjectionError::InvalidArena)
        ));
    }
}

#[test]
fn call_projection_polls_actual_loops_and_checks_before_large_payloads() {
    let config = CallConfig {
        with_log: Some(true),
        ..Default::default()
    };
    let mut visited = 0;
    project_call_trace(
        &arena(),
        config,
        gas(),
        SpecId::PRAGUE,
        &mut budget(),
        &mut || {
            visited += 1;
            Ok(())
        },
    )
    .unwrap();
    assert!(visited > 15);
    for stop in 1..=visited {
        let mut calls = 0;
        assert!(matches!(
            project_call_trace(
                &arena(),
                config,
                gas(),
                SpecId::PRAGUE,
                &mut budget(),
                &mut || {
                    calls += 1;
                    if calls == stop {
                        Err(ProjectionError::Cancelled)
                    } else {
                        Ok(())
                    }
                }
            ),
            Err(ProjectionError::Cancelled)
        ));
        assert_eq!(calls, stop);
    }
    let mut nodes = vec![node(0, None, 0)];
    nodes[0].trace.data = Bytes::from(vec![0; 64 * 1024]);
    let mut small = budget();
    small.limits.counts.data_bytes = 1;
    assert!(matches!(
        project_call_trace(
            &nodes,
            config,
            gas(),
            SpecId::PRAGUE,
            &mut small,
            &mut || Ok(())
        ),
        Err(ProjectionError::LimitExceeded(
            ProjectionResource::DataBytes
        ))
    ));
    assert_eq!(small.used().data_bytes, 0);
    let mut overflow = budget();
    overflow.limits.counts.frames_and_logs = usize::MAX;
    overflow.used.frames_and_logs = usize::MAX;
    assert!(matches!(
        project_call_trace(
            &arena(),
            config,
            gas(),
            SpecId::PRAGUE,
            &mut overflow,
            &mut || Ok(())
        ),
        Err(ProjectionError::LimitExceeded(
            ProjectionResource::FramesAndLogs
        ))
    ));
}

#[derive(Debug, thiserror::Error)]
#[error("fixture provider detail")]
struct ProviderFailure;
impl revm::database_interface::DBErrorMarker for ProviderFailure {}

#[derive(Default)]
struct Provider {
    accounts: BTreeMap<Address, AccountInfo>,
    code: BTreeMap<B256, Bytecode>,
    reads: Cell<usize>,
    fail_basic: bool,
    fail_code: bool,
}

impl DatabaseRef for Provider {
    type Error = ProviderFailure;
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.reads.set(self.reads.get() + 1);
        if self.fail_basic {
            Err(ProviderFailure)
        } else {
            Ok(self.accounts.get(&address).cloned())
        }
    }
    fn code_by_hash_ref(&self, hash: B256) -> Result<Bytecode, Self::Error> {
        self.reads.set(self.reads.get() + 1);
        if self.fail_code {
            Err(ProviderFailure)
        } else {
            self.code.get(&hash).cloned().ok_or(ProviderFailure)
        }
    }
    fn storage_ref(&self, _: Address, _: U256) -> Result<U256, Self::Error> {
        unreachable!("projection reads captured storage")
    }
    fn block_hash_ref(&self, _: u64) -> Result<B256, Self::Error> {
        unreachable!("projection does not read headers")
    }
}

fn states() -> (EvmState, Provider) {
    let mut state = EvmState::default();
    let mut provider = Provider::default();
    for index in 1..=6 {
        let address = Address::repeat_byte(index);
        let code = Bytecode::new_raw(Bytes::from(vec![0x60, index, 0x00]));
        let info = AccountInfo {
            balance: U256::from(index),
            nonce: u64::from(index),
            code_hash: code.hash_slow(),
            code: None,
            ..Default::default()
        };
        provider.code.insert(info.code_hash, code.clone());
        provider.accounts.insert(address, info.clone());
        let mut account = Account::from(info);
        if index == 1 {
            account.info.balance += U256::from(4);
            account.storage.insert(
                U256::from(1),
                EvmStorageSlot::new_changed(U256::from(5), U256::ZERO, TransactionId::ZERO),
            );
            account.storage.insert(
                U256::from(2),
                EvmStorageSlot::new_changed(U256::ZERO, U256::from(8), TransactionId::ZERO),
            );
            account.storage.insert(
                U256::from(3),
                EvmStorageSlot::new(U256::from(9), TransactionId::ZERO),
            );
        } else if index == 2 {
            provider.accounts.remove(&address);
            account.mark_created();
            account.info.code = Some(code);
        } else if index == 3 {
            account.mark_selfdestruct();
            account.info.balance = U256::ZERO;
        } else if index == 4 {
            provider.accounts.insert(
                address,
                AccountInfo {
                    balance: U256::from(19),
                    ..Default::default()
                },
            );
            account.mark_created();
            account.info.code = Some(code);
        } else if index == 6 {
            provider.accounts.remove(&address);
            account.info = AccountInfo::default();
        }
        state.insert(address, account);
    }
    (state, provider)
}

fn native_state(state: EvmState) -> ResultAndState {
    ResultAndState {
        state,
        result: ExecutionResult::Revert {
            gas: gas(),
            output: Bytes::new(),
            logs: vec![],
        },
    }
}

#[test]
fn prestate_projection_matches_native_diff_code_storage_and_lifecycle() {
    for diff in [false, true] {
        for code in [false, true] {
            for storage in [false, true] {
                let (state, provider) = states();
                let config = PreStateConfig {
                    diff_mode: Some(diff),
                    disable_code: Some(!code),
                    disable_storage: Some(!storage),
                };
                let expected = GethTraceBuilder::new(vec![])
                    .geth_prestate_traces(&native_state(state.clone()), &config, &provider)
                    .unwrap();
                let actual =
                    project_prestate(&state, config, &provider, &mut budget(), &mut || Ok(()))
                        .unwrap();
                assert_eq!(actual, expected);
            }
        }
    }
}

#[test]
fn prestate_projection_shared_pre_post_counts_exact_and_max_plus_one() {
    let (state, provider) = states();
    let config = PreStateConfig {
        diff_mode: Some(true),
        ..Default::default()
    };
    let mut reference = budget();
    project_prestate(&state, config, &provider, &mut reference, &mut || Ok(())).unwrap();
    let counts = reference.used();
    assert_eq!(counts.accounts, state.len() * 2);
    assert_eq!(counts.storage_entries, 4);
    let limits = ProjectionLimits {
        counts,
        max_call_depth: 0,
    };
    let mut shared = ProjectionBudget::new(limits);
    project_prestate(&state, config, &provider, &mut shared, &mut || Ok(())).unwrap();
    assert_eq!(shared.used(), counts);
    assert!(matches!(
        project_prestate(&state, config, &provider, &mut shared, &mut || Ok(())),
        Err(ProjectionError::LimitExceeded(ProjectionResource::Accounts))
    ));
    for resource in [
        ProjectionResource::Accounts,
        ProjectionResource::StorageEntries,
        ProjectionResource::DataBytes,
    ] {
        let mut limits = limits;
        match resource {
            ProjectionResource::Accounts => limits.counts.accounts -= 1,
            ProjectionResource::StorageEntries => limits.counts.storage_entries -= 1,
            ProjectionResource::DataBytes => limits.counts.data_bytes -= 1,
            _ => unreachable!(),
        }
        assert!(
            matches!(project_prestate(&state, config, &provider, &mut ProjectionBudget::new(limits), &mut || Ok(())), Err(ProjectionError::LimitExceeded(r)) if r == resource)
        );
    }
    let empty = project_prestate(
        &EvmState::default(),
        PreStateConfig::default(),
        &provider,
        &mut ProjectionBudget::new(ProjectionLimits {
            counts: ProjectionCounts::default(),
            max_call_depth: 0,
        }),
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(empty, PreStateFrame::Default(PreStateMode::default()));
}

#[test]
fn prestate_projection_polls_actual_traversal_and_provider_boundaries() {
    let (state, provider) = states();
    let config = PreStateConfig {
        diff_mode: Some(true),
        ..Default::default()
    };
    let mut visited = 0;
    project_prestate(&state, config, &provider, &mut budget(), &mut || {
        visited += 1;
        Ok(())
    })
    .unwrap();
    assert!(visited > 30);
    for stop in 1..=visited {
        let mut calls = 0;
        assert!(matches!(
            project_prestate(&state, config, &provider, &mut budget(), &mut || {
                calls += 1;
                if calls == stop {
                    Err(ProjectionError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            }),
            Err(ProjectionError::DeadlineExceeded)
        ));
        assert_eq!(calls, stop);
    }
    let before = provider.reads.get();
    assert!(matches!(
        project_prestate(&state, config, &provider, &mut budget(), &mut || Err(
            ProjectionError::Cancelled
        )),
        Err(ProjectionError::Cancelled)
    ));
    assert_eq!(provider.reads.get(), before);
}

#[test]
fn prestate_projection_propagates_redacted_basic_and_code_failures() {
    for code_failure in [false, true] {
        let (state, mut provider) = states();
        provider.fail_basic = !code_failure;
        provider.fail_code = code_failure;
        let error = project_prestate(
            &state,
            PreStateConfig::default(),
            &provider,
            &mut budget(),
            &mut || Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, ProjectionError::Provider(_)));
        assert!(!error.to_string().contains("fixture provider detail"));
        assert!(!format!("{error:?}").contains("fixture provider detail"));
        assert_eq!(
            error.source().unwrap().to_string(),
            "fixture provider detail"
        );
    }
}

#[test]
fn synchronous_provider_is_not_preempted_but_post_read_cancellation_rejects() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    struct HeldProvider {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        cancelled: Arc<AtomicBool>,
    }
    impl DatabaseRef for HeldProvider {
        type Error = ProviderFailure;
        fn basic_ref(&self, _: Address) -> Result<Option<AccountInfo>, Self::Error> {
            self.started.send(()).map_err(|_| ProviderFailure)?;
            self.release
                .recv_timeout(std::time::Duration::from_secs(30))
                .map_err(|_| ProviderFailure)?;
            assert!(self.cancelled.load(Ordering::SeqCst));
            Ok(None)
        }
        fn code_by_hash_ref(&self, _: B256) -> Result<Bytecode, Self::Error> {
            unreachable!()
        }
        fn storage_ref(&self, _: Address, _: U256) -> Result<U256, Self::Error> {
            unreachable!()
        }
        fn block_hash_ref(&self, _: u64) -> Result<B256, Self::Error> {
            unreachable!()
        }
    }
    let (started, seen) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let worker = std::thread::spawn(move || {
        let provider = HeldProvider {
            started,
            release: held,
            cancelled: worker_cancelled.clone(),
        };
        let state = EvmState::from_iter([(Address::ZERO, Account::default())]);
        project_prestate(
            &state,
            PreStateConfig::default(),
            &provider,
            &mut budget(),
            &mut || {
                if worker_cancelled.load(Ordering::SeqCst) {
                    Err(ProjectionError::Cancelled)
                } else {
                    Ok(())
                }
            },
        )
    });
    seen.recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    cancelled.store(true, Ordering::SeqCst);
    assert!(
        !worker.is_finished(),
        "a held synchronous provider call is not preempted"
    );
    release.send(()).unwrap();
    assert!(matches!(
        worker.join().unwrap(),
        Err(ProjectionError::Cancelled)
    ));
}

#[test]
fn native_op_evm_collected_data_matches_both_projection_families() {
    use alloy_op_evm::{OpEvmFactory, OpTx};
    use op_revm::{OpSpecId, OpTransaction};
    use reth_evm::{Evm, EvmFactory};
    use revm::context::{BlockEnv, CfgEnv, TxEnv};
    use revm_database::{CacheDB, EmptyDB};
    use revm_inspectors::tracing::{TracingInspector, TracingInspectorConfig};
    let sender = Address::repeat_byte(0x31);
    let target = Address::repeat_byte(0x32);
    let code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x2a, 0x60, 0x01, 0x55, 0x00]));
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        sender,
        AccountInfo {
            balance: U256::from(1_000_000_000),
            ..Default::default()
        },
    );
    db.insert_account_info(
        target,
        AccountInfo {
            nonce: 1,
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    let mut cfg = CfgEnv::new_with_spec(OpSpecId::ISTHMUS);
    cfg.chain_id = 480;
    cfg.disable_nonce_check = true;
    let env = reth_evm::EvmEnv::new(cfg, BlockEnv::default());
    let mut evm = OpEvmFactory::<OpTx>::default().create_evm_with_inspector(
        &mut db,
        env,
        TracingInspector::new(TracingInspectorConfig::default_geth()),
    );
    let result = evm
        .transact(OpTx(OpTransaction {
            base: TxEnv {
                caller: sender,
                kind: revm_primitives::TxKind::Call(target),
                gas_limit: 100_000,
                gas_price: 0,
                chain_id: Some(480),
                ..Default::default()
            },
            ..Default::default()
        }))
        .unwrap();
    assert!(result.result.is_success());
    let (_, inspector, _) = evm.components_mut();
    let mut inspector = std::mem::replace(
        inspector,
        TracingInspector::new(TracingInspectorConfig::default_geth()),
    );
    drop(evm);
    inspector.set_transaction_gas_limit(100_000);
    inspector.set_transaction_caller(sender);
    assert!(!inspector.traces().nodes().is_empty());
    let config = CallConfig::default();
    let expected = inspector
        .geth_builder()
        .with_spec_id(SpecId::PRAGUE)
        .geth_call_traces_with_result_gas(config, *result.result.gas());
    assert_eq!(
        project_call_trace(
            inspector.traces().nodes(),
            config,
            *result.result.gas(),
            SpecId::PRAGUE,
            &mut budget(),
            &mut || Ok(())
        )
        .unwrap(),
        expected
    );
    for diff in [false, true] {
        let config = PreStateConfig {
            diff_mode: Some(diff),
            ..Default::default()
        };
        let expected = inspector
            .geth_builder()
            .geth_prestate_traces(&result, &config, &db)
            .unwrap();
        assert_eq!(
            project_prestate(&result.state, config, &db, &mut budget(), &mut || Ok(())).unwrap(),
            expected
        );
    }
}
