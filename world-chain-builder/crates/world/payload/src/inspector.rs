use reth::revm::{Database, Inspector};
use revm::interpreter::{CallOutcome, Gas, InstructionResult, InterpreterResult};
use revm_primitives::{Address, Bytes};

/// Inspector that traces calls into the PBHEntryPoint.
/// If a tx calls into the PBHEntryPoint from an address that is not the
/// tx origin, the tx is marked as as invalid.
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
        context: &mut reth::revm::EvmContext<DB>,
        inputs: &mut reth::revm::interpreter::CallInputs,
    ) -> Option<reth::revm::interpreter::CallOutcome> {
        // Check if the target address is the PBHEntryPoint. If the caller is not the tx origin, mark the tx as invalid.
        if inputs.target_address == self.pbh_entry_point {
            if inputs.caller != context.env.tx.caller {
                let interpreter_res = InterpreterResult::new(
                    InstructionResult::InvalidEXTCALLTarget,
                    Bytes::default(),
                    Gas::default(),
                );

                return Some(CallOutcome::new(interpreter_res, 0..0));
            }
        }

        None
    }
}
