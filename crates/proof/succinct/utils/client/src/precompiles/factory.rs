//! [`EvmFactory`] implementation for the EVM in the ZKVM environment.

use alloy_evm::{Database, EvmEnv, EvmFactory, precompiles::PrecompilesMap};
use alloy_op_evm::{OpEvm, OpEvmContext, OpTx, OpTxError};
use op_revm::{
    L1BlockInfo, OpBuilder, OpHaltReason, OpSpecId, OpTransaction, precompiles::OpPrecompiles,
};
use revm::{
    Context, Inspector, MainContext,
    context::{BlockEnv, result::EVMError},
    inspector::NoOpInspector,
};

use world_chain_proof_core::range::{WorldRangeHardfork, WorldRangeHardforkConfig};

/// Factory producing OP Stack EVMs for the zkVM.
#[derive(Debug, Clone)]
pub struct ZkvmOpEvmFactory {
    world_schedule: Option<WorldRangeHardforkConfig>,
}

impl ZkvmOpEvmFactory {
    /// Creates a new [`ZkvmOpEvmFactory`].
    pub fn new() -> Self {
        Self {
            world_schedule: None,
        }
    }

    /// Creates a factory that selects the post-Jovian EVM spec from World fork names.
    pub fn new_with_world_schedule(world_schedule: WorldRangeHardforkConfig) -> Self {
        Self {
            world_schedule: Some(world_schedule),
        }
    }

    fn spec_for_timestamp(&self, timestamp: u64, kona_spec: OpSpecId) -> OpSpecId {
        let Some(schedule) = &self.world_schedule else {
            return kona_spec;
        };

        match schedule.active_fork_at(0, timestamp) {
            WorldRangeHardfork::Tropo | WorldRangeHardfork::Strato => OpSpecId::KARST,
            _ => kona_spec,
        }
    }

    /// Shared EVM construction used by both [`EvmFactory::create_evm`] and
    /// [`EvmFactory::create_evm_with_inspector`]. The only differences between the two
    /// public entry points are the concrete inspector type and the inspection flag passed
    /// to [`OpEvm::new`].
    fn build_inner<DB: Database, I: Inspector<OpEvmContext<DB>>>(
        &self,
        db: DB,
        mut input: EvmEnv<OpSpecId, BlockEnv>,
        inspector: I,
        inspect: bool,
    ) -> OpEvm<DB, I, PrecompilesMap, OpTx> {
        let spec_id =
            self.spec_for_timestamp(input.block_env.timestamp.to::<u64>(), input.cfg_env.spec);
        input.cfg_env.spec = spec_id;

        OpEvm::new(
            Context::mainnet()
                .with_tx(OpTx(OpTransaction::builder().build_fill()))
                .with_chain(L1BlockInfo::default())
                .with_db(db)
                .with_block(input.block_env)
                .with_cfg(input.cfg_env)
                .build_op_with_inspector(inspector)
                .with_precompiles(PrecompilesMap::from_static(
                    OpPrecompiles::new_with_spec(spec_id).precompiles(),
                )),
            inspect,
        )
    }
}

impl Default for ZkvmOpEvmFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl EvmFactory for ZkvmOpEvmFactory {
    type Evm<DB: Database, I: Inspector<OpEvmContext<DB>>> = OpEvm<DB, I, PrecompilesMap, OpTx>;
    type Context<DB: Database> = OpEvmContext<DB>;
    type Tx = OpTx;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError, OpTxError>;
    type HaltReason = OpHaltReason;
    type Spec = OpSpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId, BlockEnv>,
    ) -> Self::Evm<DB, NoOpInspector> {
        self.build_inner(db, input, NoOpInspector {}, false)
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv<OpSpecId, BlockEnv>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        self.build_inner(db, input, inspector, true)
    }
}
