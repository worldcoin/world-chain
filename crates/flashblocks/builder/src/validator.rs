use std::sync::Arc;

use alloy_rpc_types_engine::PayloadId;
use flashblocks_p2p::protocol::event::ChainEvent;
use flashblocks_primitives::{flashblocks::{Flashblock, Flashblocks}, primitives::{ExecutionPayloadBaseV1, ExecutionPayloadFlashblockDeltaV1, FlashblocksPayloadV1}};
use reth::core::primitives::SealedHeaderFor;
use reth_chain_state::ExecutedBlock;
use reth_evm::{ConfigureEvm, EvmEnvFor, ExecutionCtxFor};
use reth_node_api::NodePrimitives;
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_primitives::OpPrimitives;
use reth_primitives::SealedHeader;
use reth_provider::ChainSpecProvider;

pub struct FlashblocksExecutionContext <'a, N: NodePrimitives, Evm: ConfigureEvm> {
    state: Flashblocks,
    evm: Evm,
    canon_header: Arc<SealedHeader>,
    executed_block: Option<&'a ExecutedBlock<N>>
}

impl<'a, N: NodePrimitives, Evm: ConfigureEvm> FlashblocksExecutionContext<'a, N, Evm> {
    pub fn new(
        state: Flashblocks,
        evm: Evm,
        canon_header: Arc<SealedHeader>,
    ) -> Self {
        Self {
            state,
            evm,
            canon_header,
            executed_block: None,
        }
    }

    pub fn process_event(&mut self, event: &ChainEvent) {
        // Process the event and update the state accordingly.
        // This is a placeholder implementation; the actual logic will depend on the event types and how they affect the state.
        match event {
            ChainEvent::Canon(block) => {
                
            }
            ChainEvent::Pending(flashblock) => {
                // Update state with new transaction information
            }
            _ => {}
        }
    }
}

#[pin_project::pin_project]
pub struct FlashblockProcessor<V, St, Evm, P: ChainSpecProvider> {
    /// Execution + commit strategy.
    validator: V,

    /// Inner event stream.
    #[pin]
    stream: St,

    /// State provider (for header lookups and state access).
    provider: P,

    /// Lifetime-invariant config.
    chain_spec: Arc<<P as ChainSpecProvider>::ChainSpec>,

    /// EVM configuration (e.g. for opcode costing).
    evm_config: Evm,


    execution_state: Option<FlashblocksExecutionState>,
}

impl<V, St, Evm, P> FlashblockProcessor<V, St, Evm, P>
where
    V: FlashblockValidatorFor<OpEvmConfig>,
    St: WorldChainEventsStream,
    P: ChainSpecProvider,
{
    pub fn update(&mut self, flashblock: FlashblocksExecutionState) {
        self.execution_state = Some(flashblock);
    }


}

/// Epoch-scoped context derived from the base flashblock.
/// Reset on each new epoch.
struct ValidationCtx<'a, N: NodePrimitives, Evm: ConfigureEvm> {
    evm_env: EvmEnvFor<Evm>,
    execution_context: ExecutionCtxFor<'a, Evm>,
    sealed_header: Arc<SealedHeader>,
    executed_block: Option<&'a ExecutedBlock<N>>
}

//  FlashblockValidator — the core adapter struct

/// Core adapter: composes execution + state root strategies over a generic EVM.
/// Created per-epoch. Dropped on epoch boundary (cleans up EpochState).
///
/// `T: FlashblockTypes<Evm>` selects the concrete strategies.
/// `Evm: ConfigureEvm` selects the EVM implementation.
/// Everything monomorphizes — zero vtable overhead.
pub struct FlashblockValidator<T, Evm>
where
    T: FlashblockTypes<Evm>,
    Evm: ConfigureEvm,
{
    /// Execution strategy (BAL, legacy, etc.)
    execution: T::Execution,
    /// State root strategy (deferred, serial, etc.)
    state_root: T::StateRoot,
    /// Epoch-scoped state root state (ancestor handles for deferred, () for serial)
    root_state: <T::StateRoot as StateRootStrategy>::EpochState,
    /// Per-epoch validation context, generic over Evm
    ctx: ValidationCtx<Evm>,
}

impl<T, Evm> FlashblockValidator<T, Evm>
where
    T: FlashblockTypes<Evm>,
    Evm: ConfigureEvm,
{
    pub fn new(ctx: ValidationCtx<Evm>) -> Self {
        Self {
            execution: T::Execution::default(),
            state_root: T::StateRoot::default(),
            root_state: Default::default(),
            ctx,
        }
    }

    /// The pipeline: derive context → execute → commit.
    pub fn validate<P>(
        &mut self,
        coordinator: &FlashblocksExecutionCoordinator,
        provider: &P,
        latest_payload: Option<&OpBuiltPayload>,
        flashblock: Flashblock,
        pending_block: &watch::Sender<Option<ExecutedBlock<<Evm as ConfigureEvm>::Primitives>>>,
    ) -> eyre::Result<()>
    where
        P: StateProviderFactory + HeaderProvider<Header = HeaderOf<Evm>> + Clone + 'static,
    {
       
    }
}
