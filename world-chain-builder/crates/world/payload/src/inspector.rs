use reth::revm::{Database, Inspector};
use revm::interpreter::{CallOutcome, Gas, InstructionResult, InterpreterResult};
use revm_primitives::{Address, Bytes};

/// Simple inspector that keeps track of the call stack
pub struct PBHCallTracer {
    pub pbh_entry_point: Address,
}

impl PBHCallTracer {
    pub fn new(pbh_entry_point: Address) -> Self {
        Self { pbh_entry_point }
    }
}

impl<DB: Database> Inspector<DB> for PBHCallTracer {
    fn call(
        &mut self,
        _context: &mut reth::revm::EvmContext<DB>,
        inputs: &mut reth::revm::interpreter::CallInputs,
    ) -> Option<reth::revm::interpreter::CallOutcome> {
        if inputs.target_address == self.pbh_entry_point {
            let interpreter_res = InterpreterResult::new(
                InstructionResult::InvalidEXTCALLTarget,
                Bytes::default(),
                Gas::default(),
            );

            return Some(CallOutcome::new(interpreter_res, 0..0));
        }

        None
    }
}
