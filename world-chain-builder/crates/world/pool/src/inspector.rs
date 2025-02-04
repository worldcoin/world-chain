use reth::revm::{Database, Inspector};
use revm_primitives::Address;

/// Simple inspector that keeps track of the call stack
pub struct CallTracer {
    /// A vector of addresses across the call stack.
    pub stack: Vec<Address>,
    /// Depth of the call stack
    pub depth: u32,
}

impl CallTracer {
    /// Creates a new instance [`CallTracer`]
    pub fn new() -> Self {
        Self {
            stack: Vec::new(),
            depth: 0,
        }
    }

    /// Checks whether the `pbh_entrypoint` exists within the call stack.
    pub fn is_valid(&self, pbh_entrypoint: Address) -> bool {
        self.stack.iter().all(|&addr| addr != pbh_entrypoint)
    }
}

impl<DB: Database> Inspector<DB> for CallTracer {
    fn call(
        &mut self,
        _context: &mut reth::revm::EvmContext<DB>,
        inputs: &mut reth::revm::interpreter::CallInputs,
    ) -> Option<reth::revm::interpreter::CallOutcome> {
        if self.depth != 0 {
            self.stack.push(inputs.target_address);
        }
        self.depth += 1;
        None
    }
}
