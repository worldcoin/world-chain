use jsonrpsee::proc_macros::rpc;
use jsonrpsee_core::RpcResult;

use crate::preconfirmed::CAPABILITY as PRECONFIRMED_LOGS_CAPABILITY;

const FLASHBLOCKS_CAPABILITY: &str = "flashblocksv1";

/// Flashblocks Op API
#[derive(Default)]
pub struct FlashblocksOpApi {
    preconfirmed_logs: bool,
}

impl FlashblocksOpApi {
    pub const fn new(preconfirmed_logs: bool) -> Self {
        Self { preconfirmed_logs }
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
        Ok(capabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
