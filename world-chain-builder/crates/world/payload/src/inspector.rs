use reth::revm::{Database, Inspector};
use revm::interpreter::{CallOutcome, Gas, InstructionResult, InterpreterResult};
use revm_primitives::{Address, Bytes};

/// Inspector that traces calls into the `PBHEntryPoint`.
/// If a tx calls into the `PBHEntryPoint` from an address that is not the
/// tx origin or the `PBHSignatureAggregator`, the tx is marked as as invalid.
pub struct PBHCallTracer {
    pub pbh_entry_point: Address,
    pub pbh_signature_aggregator: Address,
}

impl PBHCallTracer {
    pub fn new(pbh_entry_point: Address, pbh_signature_aggregator: Address) -> Self {
        Self {
            pbh_entry_point,
            pbh_signature_aggregator,
        }
    }
}

impl<DB: Database> Inspector<DB> for PBHCallTracer {
    fn call(
        &mut self,
        context: &mut reth::revm::EvmContext<DB>,
        inputs: &mut reth::revm::interpreter::CallInputs,
    ) -> Option<reth::revm::interpreter::CallOutcome> {
        // Check if the target address is the `PBHEntryPoint`. If the caller is not the tx origin
        // or the `PBHSignatureAggregator`, mark the tx as invalid.
        if inputs.target_address == self.pbh_entry_point {
            if inputs.caller != context.env.tx.caller
                && inputs.caller != self.pbh_signature_aggregator
            {
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

#[cfg(test)]
mod tests {

    #[test]
    fn test_tx_origin_is_caller() {
        todo!()
    }

    #[test]
    fn test_signature_aggregator_is_caller() {
        todo!()
    }

    #[test]
    fn test_invalid_caller() {
        todo!()
    }
}
