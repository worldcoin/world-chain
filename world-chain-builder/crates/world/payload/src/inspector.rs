use reth::revm::{Database, Inspector};
use revm::interpreter::CallOutcome;
use revm_primitives::Address;

/// Simple inspector that keeps track of the call stack
pub struct PBHCallTracer {
    pub pbh_entry_point: Address,
    pub invalid: bool,
}

impl PBHCallTracer {
    pub fn new(pbh_entry_point: Address) -> Self {
        Self {
            pbh_entry_point,
            invalid: false,
        }
    }

    pub fn reset(&mut self) {
        self.invalid = false;
    }
}

impl<DB: Database> Inspector<DB> for PBHCallTracer {
    fn call(
        &mut self,
        _context: &mut reth::revm::EvmContext<DB>,
        inputs: &mut reth::revm::interpreter::CallInputs,
    ) -> Option<reth::revm::interpreter::CallOutcome> {
        if inputs.target_address == self.pbh_entry_point {
            self.invalid = true;
        }

        None
    }
}
