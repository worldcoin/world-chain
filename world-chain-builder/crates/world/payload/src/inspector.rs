use reth::revm::{Database, Inspector};
use revm_primitives::Address;

/// Simple inspector that keeps track of the call stack
pub struct PBHCallTracer {
    /// A vector of addresses across the call stack.
    pub stack: Vec<Address>,
}

impl PBHCallTracer {
    /// Creates a new instance [`CallTracer`]
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Checks whether the `pbh_entrypoint` exists within the call stack.
    // TODO: fix this
    pub fn is_valid(&self, pbh_entrypoint: Address) -> bool {
        self.stack[1..].iter().all(|&addr| addr != pbh_entrypoint)
    }
}

impl<DB: Database> Inspector<DB> for PBHCallTracer {
    fn call(
        &mut self,
        _context: &mut reth::revm::EvmContext<DB>,
        inputs: &mut reth::revm::interpreter::CallInputs,
    ) -> Option<reth::revm::interpreter::CallOutcome> {
        self.stack.push(inputs.target_address);

        None
    }
}
