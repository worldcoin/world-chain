//! OP-aware revmc JIT integration.
//!
//! revmc's Alloy adapter executes Ethereum's mainnet handler directly. World Chain must retain
//! [`op_revm::handler::OpHandler`] so deposits, L1 fees, operator fees, and OP-specific validation
//! continue to follow consensus. This module therefore only uses revmc as the execution backend
//! and explicitly drives the OP handler around it.

use alloy_evm::{Database, Evm, EvmEnv, EvmFactory, IntoTxEnv, precompiles::PrecompilesMap};
use alloy_op_evm::{
    OpEvmContext, OpTx, OpTxError, map_op_err,
    post_exec::{
        PostExecCompositeInspector, PostExecEvmFactoryHooks, PostExecExecutedTx,
        PostExecRefundInspector, PostExecTxContext, SDMWarmingInspector, WarmingState,
    },
};
use alloy_primitives::{Address, Bytes};
use core::{fmt::Debug, marker::PhantomData};
use op_revm::{
    L1BlockInfo, OpBuilder, OpHaltReason, OpSpecId, OpTransaction, OpTransactionError, constants::{BASE_FEE_RECIPIENT, L1_FEE_RECIPIENT, OPERATOR_FEE_RECIPIENT}, handler::OpHandler, precompiles::OpPrecompiles,
};
use reth_evm_ethereum::factory::{JitMode, RevmcMetrics, RuntimeConfig, RuntimeTuning};
use reth_node_core::args::JitArgs;
use revm::{
    Context, Inspector, MainContext,
    context::{BlockEnv, CfgEnv, ContextSetters, DBErrorMarker, TxEnv},
    context_interface::{
        Transaction,
        result::{EVMError, ResultAndState},
    },
    handler::{EthFrame, Handler, SystemCallTx, instructions::EthInstructions},
    inspector::{InspectorHandler, NoOpInspector},
    interpreter::interpreter::EthInterpreter,
};
use revmc::{revm_evm::JitEvm as RevmcJitEvm, runtime::JitBackend};
use std::{path::PathBuf, sync::Arc};

pub use reth_evm_ethereum::factory::maybe_run_jit_helper;

type OpEvm<DB, I, R> = op_revm::OpEvm<
    OpEvmContext<DB>,
    PostExecCompositeInspector<I, R>,
    EthInstructions<EthInterpreter, OpEvmContext<DB>>,
    PrecompilesMap,
>;

/// An OP EVM whose interpreter may be replaced by revmc for ordinary transaction execution.
///
/// Inspector execution and SDM post-exec production intentionally stay on the interpreter because
/// revmc's compiled path does not emit the per-opcode callbacks those modes require.
#[allow(missing_debug_implementations)]
pub struct WorldChainJitEvm<DB: Database, I, Tx = OpTx, R = SDMWarmingInspector> {
    inner: RevmcJitEvm<OpEvm<DB, I, R>>,
    enabled_backend: JitBackend,
    inspect: bool,
    post_exec_tracking_active: bool,
    last_tx_post_exec_result: PostExecExecutedTx,
    _tx: PhantomData<Tx>,
}

impl<DB: Database, I, Tx, R: Default> WorldChainJitEvm<DB, I, Tx, R> {
    fn from_env(
        db: DB,
        input: EvmEnv<OpSpecId, BlockEnv>,
        inspector: I,
        inspect: bool,
        backend: JitBackend,
    ) -> Self {
        let spec_id = input.cfg_env.spec;
        let inner = Context::mainnet()
            .with_tx(OpTx(OpTransaction::builder().build_fill()))
            .with_cfg(CfgEnv::new_with_spec(OpSpecId::BEDROCK))
            .with_chain(L1BlockInfo::default())
            .with_db(db)
            .with_block(input.block_env)
            .with_cfg(input.cfg_env)
            .build_op_with_inspector(PostExecCompositeInspector::<I, R>::new(inspector))
            .with_precompiles(PrecompilesMap::from_static(
                OpPrecompiles::new_with_spec(spec_id).precompiles(),
            ));

        let initial_backend = if inspect {
            JitBackend::disabled()
        } else {
            backend.clone()
        };
        Self {
            inner: RevmcJitEvm::new(inner, initial_backend),
            enabled_backend: backend,
            inspect,
            post_exec_tracking_active: false,
            last_tx_post_exec_result: Default::default(),
            _tx: PhantomData,
        }
    }
}

impl<DB: Database, I, Tx, R> WorldChainJitEvm<DB, I, Tx, R> {
    fn refresh_backend(&mut self) {
        let backend = if self.inspect || self.post_exec_tracking_active {
            JitBackend::disabled()
        } else {
            self.enabled_backend.clone()
        };
        self.inner.set_backend(backend);
    }
}

impl<DB: Database, I, Tx, R> WorldChainJitEvm<DB, I, Tx, R>
where
    R: PostExecRefundInspector,
{
    fn begin_post_exec_tx(&mut self, ctx: PostExecTxContext) {
        self.post_exec_tracking_active = true;
        self.inner.inner_mut().0.inspector.begin_post_exec_tx(ctx);
        self.refresh_backend();
    }

    fn take_last_post_exec_tx_result(&mut self) -> PostExecExecutedTx {
        core::mem::take(&mut self.last_tx_post_exec_result)
    }

    fn refund_snapshot(&self) -> R::Snapshot {
        self.inner.inner().0.inspector.refund_snapshot()
    }

    fn seed_refund_snapshot(&mut self, state: R::Snapshot) {
        self.inner
            .inner_mut()
            .0
            .inspector
            .seed_refund_snapshot(state);
    }
}

impl<DB, I, Tx, R> Evm for WorldChainJitEvm<DB, I, Tx, R>
where
    DB: Database,
    I: Inspector<OpEvmContext<DB>>,
    Tx: IntoTxEnv<Tx> + Into<OpTransaction<TxEnv>>,
    R: Inspector<OpEvmContext<DB>> + PostExecRefundInspector,
{
    type DB = DB;
    type Tx = Tx;
    type Error = EVMError<DB::Error, OpTxError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.inner.inner().0.ctx.block
    }

    fn cfg_env(&self) -> &CfgEnv<OpSpecId> {
        &self.inner.inner().0.ctx.cfg
    }

    fn chain_id(&self) -> u64 {
        self.inner.inner().0.ctx.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.last_tx_post_exec_result = Default::default();
        self.inner.inner_mut().0.ctx.set_tx(OpTx(tx.into()));

        let track_post_exec = self.post_exec_tracking_active;
        let output = if self.inspect || track_post_exec {
            let mut handler = OpHandler::<
                _,
                EVMError<DB::Error, OpTransactionError>,
                EthFrame<EthInterpreter>,
            >::new();
            handler.inspect_run(&mut self.inner)
        } else {
            let mut handler = OpHandler::<
                _,
                EVMError<DB::Error, OpTransactionError>,
                EthFrame<EthInterpreter>,
            >::new();
            handler.run(&mut self.inner)
        };

        if track_post_exec {
            if self.inner.inner().0.ctx.tx.tx_type()
                != op_revm::transaction::deposit::DEPOSIT_TRANSACTION_TYPE
            {
                let is_isthmus = self
                    .inner
                    .inner()
                    .0
                    .ctx
                    .cfg
                    .spec
                    .is_enabled_in(OpSpecId::ISTHMUS);
                let inspector = &mut self.inner.inner_mut().0.inspector;
                inspector.note_account_touch(L1_FEE_RECIPIENT);
                inspector.note_account_touch(BASE_FEE_RECIPIENT);
                if is_isthmus {
                    inspector.note_account_touch(OPERATOR_FEE_RECIPIENT);
                }
            }
            self.last_tx_post_exec_result =
                self.inner.inner_mut().0.inspector.finish_post_exec_tx();
            self.post_exec_tracking_active = false;
            self.refresh_backend();
        }

        let state = self.inner.inner_mut().0.ctx.journaled_state.finalize();
        let result = output.map_err(map_op_err)?;
        Ok(ResultAndState::new(result, state))
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.inner
            .inner_mut()
            .0
            .ctx
            .set_tx(OpTx::new_system_tx_with_caller(caller, contract, data));
        let output = if self.inspect {
            let mut handler = OpHandler::<
                _,
                EVMError<DB::Error, op_revm::OpTransactionError>,
                EthFrame<EthInterpreter>,
            >::new();
            handler.inspect_run_system_call(&mut self.inner)
        } else {
            let mut handler = OpHandler::<
                _,
                EVMError<DB::Error, op_revm::OpTransactionError>,
                EthFrame<EthInterpreter>,
            >::new();
            handler.run_system_call(&mut self.inner)
        };
        let state = self.inner.inner_mut().0.ctx.journaled_state.finalize();
        let result = output.map_err(map_op_err)?;
        Ok(ResultAndState::new(result, state))
    }

    fn finish(self) -> (DB, EvmEnv<OpSpecId, BlockEnv>) {
        let Context {
            block: block_env,
            cfg: cfg_env,
            journaled_state,
            ..
        } = self.inner.into_inner().0.ctx;
        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
        self.refresh_backend();
    }

    fn components(&self) -> (&DB, &I, &PrecompilesMap) {
        let inner = self.inner.inner();
        (
            &inner.0.ctx.journaled_state.database,
            inner.0.inspector.inner(),
            &inner.0.precompiles,
        )
    }

    fn components_mut(&mut self) -> (&mut DB, &mut I, &mut PrecompilesMap) {
        let inner = self.inner.inner_mut();
        (
            &mut inner.0.ctx.journaled_state.database,
            inner.0.inspector.inner_mut(),
            &mut inner.0.precompiles,
        )
    }
}

/// OP EVM factory backed by revmc when both the runtime and local JIT gates are enabled.
#[derive(Clone, Debug)]
pub struct OpJitEvmFactory<Tx = OpTx> {
    backend: JitBackend,
    disabled: JitBackend,
    metrics: RevmcMetrics,
    jit_support: bool,
    _tx: PhantomData<Tx>,
}

impl<Tx> Default for OpJitEvmFactory<Tx> {
    fn default() -> Self {
        Self::disabled()
    }
}

impl<Tx> OpJitEvmFactory<Tx> {
    /// Creates a factory using `backend`.
    pub fn new(backend: JitBackend) -> Self {
        Self::new_with_metrics(backend, RevmcMetrics::default())
    }

    /// Creates a factory using `backend` and the supplied metrics handles.
    pub fn new_with_metrics(backend: JitBackend, metrics: RevmcMetrics) -> Self {
        Self {
            backend,
            disabled: JitBackend::disabled(),
            metrics,
            jit_support: false,
            _tx: PhantomData,
        }
    }

    /// Creates a factory whose runtime backend starts disabled.
    pub fn disabled() -> Self {
        Self::new(JitBackend::disabled())
    }

    /// Returns the shared runtime JIT backend.
    pub const fn backend(&self) -> &JitBackend {
        &self.backend
    }

    /// Enables or disables the local JIT gate for subsequently created EVMs.
    pub const fn set_jit_support(&mut self, enabled: bool) {
        self.jit_support = enabled;
    }

    /// Returns whether the local JIT gate is enabled.
    pub const fn jit_support_enabled(&self) -> bool {
        self.jit_support
    }

    fn selected_backend(&self) -> JitBackend {
        if self.jit_support_enabled() {
            self.backend.clone()
        } else {
            self.disabled.clone()
        }
    }

    fn pause_jit(&self) {
        let was_paused = self.backend.is_paused();
        self.backend.pause();
        let is_paused = self.backend.is_paused();
        if !was_paused && is_paused {
            self.metrics.pauses_total.increment(1);
        }
        self.metrics.paused.set(is_paused as u8 as f64);
    }

    fn resume_jit(&self) {
        let was_paused = self.backend.is_paused();
        self.backend.resume();
        let is_paused = self.backend.is_paused();
        if was_paused && !is_paused {
            self.metrics.resumes_total.increment(1);
        }
        self.metrics.paused.set(is_paused as u8 as f64);
    }
}

impl<Tx> reth_evm::JitBackend for OpJitEvmFactory<Tx>
where
    Tx: Send + Sync,
{
    fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        self.backend
            .set_enabled(enabled)
            .map_err(|err| err.to_string())
    }

    fn pause(&self) {
        self.pause_jit();
    }

    fn resume(&self) {
        self.resume_jit();
    }

    fn clear(&self) {
        self.backend.clear_all();
    }
}

impl<Tx> EvmFactory for OpJitEvmFactory<Tx>
where
    Tx: IntoTxEnv<Tx> + Into<OpTransaction<TxEnv>> + Default + Clone + Debug,
{
    type Evm<DB: Database, I: Inspector<OpEvmContext<DB>>> = WorldChainJitEvm<DB, I, Tx>;
    type Context<DB: Database> = OpEvmContext<DB>;
    type Tx = Tx;
    type Error<DBError: DBErrorMarker> = EVMError<DBError, OpTxError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId, BlockEnv>,
    ) -> Self::Evm<DB, NoOpInspector> {
        WorldChainJitEvm::from_env(db, input, NoOpInspector {}, false, self.selected_backend())
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId, BlockEnv>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        WorldChainJitEvm::from_env(db, input, inspector, true, self.selected_backend())
    }
}

impl<Tx> PostExecEvmFactoryHooks for OpJitEvmFactory<Tx>
where
    Tx: IntoTxEnv<Tx> + Into<OpTransaction<TxEnv>> + Default + Clone + Debug,
{
    type Snapshot = WarmingState;

    fn begin_post_exec_tx<DB, I>(evm: &mut Self::Evm<DB, I>, ctx: PostExecTxContext)
    where
        DB: Database,
        I: Inspector<Self::Context<DB>>,
    {
        evm.begin_post_exec_tx(ctx);
    }

    fn take_last_post_exec_tx_result<DB, I>(evm: &mut Self::Evm<DB, I>) -> PostExecExecutedTx
    where
        DB: Database,
        I: Inspector<Self::Context<DB>>,
    {
        evm.take_last_post_exec_tx_result()
    }

    fn refund_snapshot<DB, I>(evm: &Self::Evm<DB, I>) -> Self::Snapshot
    where
        DB: Database,
        I: Inspector<Self::Context<DB>>,
    {
        evm.refund_snapshot()
    }

    fn seed_refund_snapshot<DB, I>(evm: &mut Self::Evm<DB, I>, state: Self::Snapshot)
    where
        DB: Database,
        I: Inspector<Self::Context<DB>>,
    {
        evm.seed_refund_snapshot(state);
    }
}

/// Builds the OP JIT factory and optional metrics recorder from the upstream Reth CLI arguments.
pub fn build_jit_factory(
    jit: &JitArgs,
    dump_dir: Option<PathBuf>,
) -> eyre::Result<(OpJitEvmFactory<OpTx>, Option<Arc<RevmcMetrics>>)> {
    if !jit.enabled {
        return Ok((OpJitEvmFactory::disabled(), None));
    }

    let default_tuning = RuntimeTuning::default();
    let tuning = RuntimeTuning {
        channel_capacity: jit.channel_capacity,
        jit_hot_threshold: jit.hot_threshold,
        jit_max_bytecode_len: jit.max_bytecode_len,
        jit_max_pending_jobs: jit.max_pending_jobs,
        jit_worker_count: jit.worker_count.unwrap_or(default_tuning.jit_worker_count),
        jit_timeout: default_tuning.jit_timeout,
        jit_helper_memory_limit_bytes: default_tuning.jit_helper_memory_limit_bytes,
        jit_helper_cpu_count: default_tuning.jit_helper_cpu_count,
        resident_code_cache_bytes: jit.code_cache_bytes,
        idle_evict_duration: Some(jit.idle_evict_duration),
        max_events_per_drain: default_tuning.max_events_per_drain,
        event_drain_interval: default_tuning.event_drain_interval,
        shutdown_timeout: default_tuning.shutdown_timeout,
        jit_worker_queue_capacity: default_tuning.jit_worker_queue_capacity,
        jit_opt_level: default_tuning.jit_opt_level,
        aot_opt_level: default_tuning.aot_opt_level,
        eviction_sweep_interval: default_tuning.eviction_sweep_interval,
        compiler_recycle_threshold: default_tuning.compiler_recycle_threshold,
    };

    let default_config = RuntimeConfig::default();
    let mut config = RuntimeConfig {
        enabled: jit.enabled,
        thread_name: default_config.thread_name,
        store: default_config.store,
        tuning,
        dump_dir,
        debug_assertions: jit.debug,
        blocking: jit.blocking,
        single_error: default_config.single_error,
        no_dedup: default_config.no_dedup,
        no_dse: default_config.no_dse,
        gas_params: default_config.gas_params,
        aot: default_config.aot,
        jit_mode: JitMode::OutOfProcess,
        jit_helper_path: default_config.jit_helper_path,
        on_compilation: default_config.on_compilation,
    };

    let revmc_metrics = Arc::new(RevmcMetrics::default());
    let compilation_metrics = revmc_metrics.clone();
    config.on_compilation = Some(Arc::new(move |event| {
        compilation_metrics.record_compilation(&event);
    }));

    let tuning = config.tuning;
    let jit_mode = config.jit_mode;
    let backend = JitBackend::new(config)?;

    tracing::warn!(
        target: "reth::cli",
        hot_threshold = tuning.jit_hot_threshold,
        workers = tuning.jit_worker_count,
        mode = ?jit_mode,
        blocking = jit.blocking,
        "Started experimental revmc JIT backend for the OP EVM; this may cause instability",
    );

    let factory = OpJitEvmFactory::new_with_metrics(backend, revmc_metrics.as_ref().clone());
    Ok((factory, Some(revmc_metrics)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_op_evm::{OpEvmFactory, post_exec::PostExecTxKind};
    use alloy_primitives::{TxKind, U256};
    use revm::{
        bytecode::Bytecode,
        database::{CacheDB, EmptyDB},
        database_interface::EmptyDB as EmptyDatabase,
        state::AccountInfo,
    };

    fn env() -> EvmEnv<OpSpecId, BlockEnv> {
        EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::BEDROCK),
            BlockEnv {
                gas_limit: 30_000_000,
                ..Default::default()
            },
        )
    }

    #[test]
    fn op_handler_is_used_instead_of_mainnet_handler() {
        let mut evm = OpJitEvmFactory::<OpTx>::default().create_evm(EmptyDB::default(), env());

        // MainnetHandler would not enforce the OP requirement for an encoded envelope.
        let missing_envelope = OpTx(OpTransaction::new(TxEnv::default()));
        let err = evm.transact_raw(missing_envelope).unwrap_err();
        assert!(matches!(
            err,
            EVMError::Transaction(OpTxError(op_revm::OpTransactionError::MissingEnvelopedTx))
        ));
    }

    #[test]
    fn user_inspection_forces_interpreter_and_can_be_toggled() {
        let backend = JitBackend::disabled();
        backend.set_enabled(true).unwrap();
        let mut factory = OpJitEvmFactory::<OpTx>::new(backend);
        factory.set_jit_support(true);
        let mut evm =
            factory.create_evm_with_inspector(EmptyDB::default(), env(), NoOpInspector {});

        assert!(!evm.inner.backend().enabled());
        evm.set_inspector_enabled(false);
        assert!(evm.inner.backend().enabled());
        evm.set_inspector_enabled(true);
        assert!(!evm.inner.backend().enabled());
    }

    #[test]
    fn post_exec_production_forces_interpreter_and_restores_backend() {
        let backend = JitBackend::disabled();
        backend.set_enabled(true).unwrap();
        let mut factory = OpJitEvmFactory::<OpTx>::new(backend);
        factory.set_jit_support(true);
        let mut evm = factory.create_evm(EmptyDB::default(), env());

        assert!(evm.inner.backend().enabled());
        evm.begin_post_exec_tx(PostExecTxContext {
            tx_index: 0,
            kind: PostExecTxKind::Normal,
        });
        assert!(!evm.inner.backend().enabled());

        let missing_envelope = OpTx(OpTransaction::new(TxEnv::default()));
        assert!(evm.transact_raw(missing_envelope).is_err());
        assert!(evm.inner.backend().enabled());
        assert!(!evm.post_exec_tracking_active);
    }

    #[test]
    fn blocking_jit_matches_op_interpreter_for_contract_call() {
        const RUNTIME_CODE: &[u8] = &[0x60, 0x42, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3];

        let caller = Address::with_last_byte(0x11);
        let target = Address::with_last_byte(0x22);
        let test_db = || {
            let mut db = CacheDB::<EmptyDatabase>::default();
            db.insert_account_info(
                caller,
                AccountInfo {
                    balance: U256::MAX,
                    ..Default::default()
                },
            );
            let bytecode = Bytecode::new_raw(Bytes::from_static(RUNTIME_CODE));
            db.insert_account_info(
                target,
                AccountInfo {
                    nonce: 1,
                    code_hash: bytecode.hash_slow(),
                    code: Some(bytecode),
                    ..Default::default()
                },
            );
            db
        };
        let call = || {
            let mut tx = OpTransaction::new(TxEnv {
                caller,
                kind: TxKind::Call(target),
                gas_limit: 1_000_000,
                ..Default::default()
            });
            tx.enveloped_tx = Some(Bytes::from_static(b"\0"));
            OpTx(tx)
        };
        let call_env = EvmEnv::new(
            CfgEnv::new_with_spec(OpSpecId::CANYON),
            BlockEnv {
                gas_limit: 30_000_000,
                ..Default::default()
            },
        );

        let backend = JitBackend::new(RuntimeConfig {
            blocking: true,
            ..Default::default()
        })
        .unwrap();
        let mut jit_factory = OpJitEvmFactory::<OpTx>::new(backend.clone());
        jit_factory.set_jit_support(true);
        let jit_result = jit_factory
            .create_evm(test_db(), call_env.clone())
            .transact_raw(call())
            .unwrap();
        let interpreter_result = OpEvmFactory::<OpTx>::default()
            .create_evm(test_db(), call_env)
            .transact_raw(call())
            .unwrap();

        assert_eq!(jit_result, interpreter_result);
        let stats = backend.stats();
        assert!(
            stats.compilations_succeeded > 0,
            "expected a successful compilation, got: {stats:?}"
        );
        assert!(
            stats.resident_entries > 0,
            "expected resident compiled code, got: {stats:?}"
        );
    }
}
