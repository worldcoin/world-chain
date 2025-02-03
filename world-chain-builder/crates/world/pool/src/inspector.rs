use reth::revm::{Database, Inspector};
use revm_primitives::Address;

/// Simple inspector that keeps track of the call stack
pub struct CallTracer {
    /// A vector of addresses across the call stack.
    pub stack: Vec<Address>,
}

impl CallTracer {
    /// Creates a new instance [`CallTracer`]
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Checks whether the `pbh_entrypoint` exists within the call stack.
    /// 
    /// Note: This is excluding the target of the transaction from an EOA as the first external call interpreted will be
    ///       within the call context of a contract. 
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
        self.stack.push(inputs.target_address);
        None
    }
}
