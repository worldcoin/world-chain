//! Read-only access to captured signature evidence; it does not assert finality.

use jsonrpsee::{core::RpcResult, proc_macros::rpc, types::ErrorObjectOwned};
use serde::Serialize;
use world_chain_p2p::evidence::FlashblockEvidenceStore;
use world_chain_primitives::evidence::{FlashblockEvidence, FlashblockEvidenceId};

#[rpc(server, client, namespace = "worldchain")]
pub trait FlashblockEvidenceApi {
    #[method(name = "getFlashblockEvidence")]
    fn get_flashblock_evidence(&self, id: FlashblockEvidenceId) -> RpcResult<FlashblockEvidence>;
}

#[derive(Clone, Debug)]
pub struct FlashblockEvidenceRpc {
    store: FlashblockEvidenceStore,
    chain_id: u64,
}

impl FlashblockEvidenceRpc {
    pub fn new(store: FlashblockEvidenceStore, chain_id: u64) -> Self {
        Self { store, chain_id }
    }
}

impl FlashblockEvidenceApiServer for FlashblockEvidenceRpc {
    fn get_flashblock_evidence(&self, id: FlashblockEvidenceId) -> RpcResult<FlashblockEvidence> {
        #[derive(Serialize)]
        struct Unavailable {
            reason: &'static str,
            id: FlashblockEvidenceId,
        }
        let wire = self.store.get(id).map_err(|reason| {
            ErrorObjectOwned::owned(
                -32080,
                "Flashblock evidence unavailable",
                Some(Unavailable {
                    reason: reason.as_str(),
                    id,
                }),
            )
        })?;
        Ok(FlashblockEvidence {
            version: 1,
            chain_id: self.chain_id,
            id,
            wire,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_evidence_and_malformed_selectors_fail_explicitly() {
        let module = FlashblockEvidenceRpc::new(Default::default(), 480).into_rpc();
        let (response, _) = module.raw_json_request(r#"{"jsonrpc":"2.0","id":1,"method":"worldchain_getFlashblockEvidence","params":[{"payloadId":"0x0000000000000000","index":0,"blockHash":"0x0000000000000000000000000000000000000000000000000000000000000000"}]}"#, 1).await.expect("RPC response");
        let response: serde_json::Value = serde_json::from_str(response.get()).expect("JSON");
        assert_eq!(response["error"]["code"], -32080);
        assert_eq!(response["error"]["data"]["reason"], "notRetained");
        let (response, _) = module.raw_json_request(r#"{"jsonrpc":"2.0","id":1,"method":"worldchain_getFlashblockEvidence","params":[{"index":0,"unknown":true}]}"#, 1).await.expect("RPC response");
        let response: serde_json::Value = serde_json::from_str(response.get()).expect("JSON");
        assert_eq!(response["error"]["code"], -32602);
    }
}
