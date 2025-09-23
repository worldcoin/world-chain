use jsonrpsee::proc_macros::rpc;
use jsonrpsee_core::{async_trait, RpcResult};

const FLASHBLOCKS_CAPABILITY: &str = "flashblocksv1";

/// Flashblocks Op API
#[derive(Default)]
pub struct FlashblocksOpApi;

#[cfg_attr(not(test), rpc(server, namespace = "op"))]
#[cfg_attr(test, rpc(server, client, namespace = "op"))]
pub trait OpApiExt {
    /// Method to get supported capabilities
    #[method(name = "supportedCapabilities")]
    fn supported_capabilities(&self) -> RpcResult<Vec<String>>;
}

#[async_trait]
impl OpApiExtServer for FlashblocksOpApi {
    fn supported_capabilities(&self) -> RpcResult<Vec<String>> {
        Ok(vec![FLASHBLOCKS_CAPABILITY.to_string()])
    }
}
