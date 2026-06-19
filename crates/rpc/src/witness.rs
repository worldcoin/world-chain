use std::sync::Arc;

use alloy_rpc_types_debug::ExecutionWitness;
use jsonrpsee::{
    core::{RpcResult, async_trait},
    proc_macros::rpc,
    types::{ErrorObjectOwned, error::INVALID_PARAMS_CODE},
};
use world_chain_evm::ExecutionWitnessHandle;

/// `debug` namespace extension serving the live pre-image witness oracle.
#[derive(Clone, Debug)]
pub struct DebugWitnessOracle {
    cache: ExecutionWitnessHandle,
}

impl DebugWitnessOracle {
    /// Creates a new oracle backed by the shared witness cache.
    pub const fn new(cache: ExecutionWitnessHandle) -> Self {
        Self { cache }
    }
}

#[cfg_attr(not(test), rpc(server, namespace = "debug"))]
#[cfg_attr(test, rpc(server, client, namespace = "debug"))]
#[async_trait]
pub trait DebugWitnessOracleApi {
    /// Returns the per-block execution witnesses for the L2 range `(start_block, end_block]`.
    #[method(name = "collectRangeWitness")]
    async fn collect_range_witness(
        &self,
        start_block: u64,
        end_block: u64,
    ) -> RpcResult<Vec<Arc<ExecutionWitness>>>;
}

#[async_trait]
impl DebugWitnessOracleApiServer for DebugWitnessOracle {
    async fn collect_range_witness(
        &self,
        start_block: u64,
        end_block: u64,
    ) -> RpcResult<Vec<Arc<ExecutionWitness>>> {
        self.cache.range(start_block, end_block).ok_or_else(|| {
            ErrorObjectOwned::owned(
                INVALID_PARAMS_CODE,
                format!("range ({start_block}, {end_block}] not available in witness cache"),
                None::<String>,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_cached_range() {
        let cache = ExecutionWitnessHandle::default();
        cache.insert(11, ExecutionWitness::default());
        cache.insert(12, ExecutionWitness::default());
        let oracle = DebugWitnessOracle::new(cache);

        let range = oracle
            .collect_range_witness(11, 12)
            .await
            .expect("range cached");
        assert_eq!(range.len(), 2);
    }

    #[tokio::test]
    async fn errors_on_gap() {
        let cache = ExecutionWitnessHandle::default();
        cache.insert(11, ExecutionWitness::default());
        let oracle = DebugWitnessOracle::new(cache);

        let err = oracle
            .collect_range_witness(11, 12)
            .await
            .expect_err("gap should error");
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
    }
}
