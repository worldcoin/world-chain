use op_alloy_consensus::OpTxEnvelope;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum WorldChainTxEnvelope {
    Unverified(OpTxEnvelope),
    Verified(VerifiedTx),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedTx {
    pub tx: OpTxEnvelope,
    pub proof: Vec<u8>,
}
