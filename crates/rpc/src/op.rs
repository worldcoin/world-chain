use jsonrpsee::proc_macros::rpc;
use jsonrpsee_core::RpcResult;

use crate::preconfirmed::CAPABILITY as PRECONFIRMED_LOGS_CAPABILITY;

const FLASHBLOCKS_CAPABILITY: &str = "flashblocksv1";

/// Flashblocks Op API
#[derive(Default)]
pub struct FlashblocksOpApi {
    preconfirmed_logs: bool,
    flashblock_evidence: bool,
    flashblock_receipt_proofs: bool,
}

impl FlashblocksOpApi {
    pub const fn new(preconfirmed_logs: bool) -> Self {
        Self {
            preconfirmed_logs,
            flashblock_evidence: false,
            flashblock_receipt_proofs: false,
        }
    }

    pub const fn with_flashblock_receipt_proofs(mut self, enabled: bool) -> Self {
        self.flashblock_receipt_proofs = enabled;
        self
    }

    pub const fn with_flashblock_evidence(mut self, enabled: bool) -> Self {
        self.flashblock_evidence = enabled;
        self
    }
}

#[rpc(server, client, namespace = "op")]
pub trait OpApiExt {
    /// Method to get supported capabilities
    #[method(name = "supportedCapabilities")]
    fn supported_capabilities(&self) -> RpcResult<Vec<String>>;
}

impl OpApiExtServer for FlashblocksOpApi {
    fn supported_capabilities(&self) -> RpcResult<Vec<String>> {
        let mut capabilities = vec![FLASHBLOCKS_CAPABILITY.to_string()];
        if self.preconfirmed_logs {
            capabilities.push(PRECONFIRMED_LOGS_CAPABILITY.to_string());
        }
        if self.flashblock_evidence {
            capabilities
                .push(world_chain_primitives::evidence::FLASHBLOCK_EVIDENCE_CAPABILITY.to_string());
        }
        if self.flashblock_receipt_proofs {
            capabilities.push(world_chain_primitives::receipt_proof::CAPABILITY.to_string());
        }
        Ok(capabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_capability_requires_explicit_enablement() {
        let capabilities = FlashblocksOpApi::default()
            .with_flashblock_evidence(true)
            .supported_capabilities()
            .expect("capabilities");
        assert!(
            capabilities
                .iter()
                .any(|c| c == world_chain_primitives::evidence::FLASHBLOCK_EVIDENCE_CAPABILITY)
        );
        assert!(
            !FlashblocksOpApi::default()
                .supported_capabilities()
                .expect("capabilities")
                .iter()
                .any(|c| c == world_chain_primitives::evidence::FLASHBLOCK_EVIDENCE_CAPABILITY)
        );
    }

    #[test]
    fn preconfirmed_log_capability_requires_explicit_enablement() {
        assert_eq!(
            FlashblocksOpApi::default()
                .supported_capabilities()
                .unwrap(),
            [FLASHBLOCKS_CAPABILITY]
        );
        assert_eq!(
            FlashblocksOpApi::new(true)
                .supported_capabilities()
                .unwrap(),
            [FLASHBLOCKS_CAPABILITY, PRECONFIRMED_LOGS_CAPABILITY]
        );
    }
}
