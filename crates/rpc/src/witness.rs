use std::sync::Arc;

use jsonrpsee::{
    core::{RpcResult, async_trait},
    proc_macros::rpc,
    types::{ErrorObjectOwned, error::INVALID_PARAMS_CODE},
};
use world_chain_witness::{RangeWitness, WitnessCache};

/// `debug` namespace extension serving the live pre-image witness oracle.
#[derive(Clone, Debug)]
pub struct DebugWitnessOracle {
    cache: Arc<WitnessCache>,
}

impl DebugWitnessOracle {
    /// Creates a new oracle backed by the shared [`WitnessCache`].
    pub const fn new(cache: Arc<WitnessCache>) -> Self {
        Self { cache }
    }
}

#[cfg_attr(not(test), rpc(server, namespace = "debug"))]
#[cfg_attr(test, rpc(server, client, namespace = "debug"))]
#[async_trait]
pub trait DebugWitnessOracleApi {
    /// Returns the cached pre-image witness for the L2 range `(start_block, end_block]`.
    #[method(name = "collectRangeWitness")]
    async fn collect_range_witness(
        &self,
        start_block: u64,
        end_block: u64,
    ) -> RpcResult<RangeWitness>;
}

#[async_trait]
impl DebugWitnessOracleApiServer for DebugWitnessOracle {
    async fn collect_range_witness(
        &self,
        start_block: u64,
        end_block: u64,
    ) -> RpcResult<RangeWitness> {
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
    use alloy_primitives::B256;
    use alloy_rpc_types_debug::ExecutionWitness;
    use world_chain_witness::BlockWitness;

    use super::*;

    fn block(number: u64) -> BlockWitness {
        BlockWitness {
            block_number: number,
            block_hash: B256::with_last_byte(number as u8),
            execution_witness: ExecutionWitness::default(),
            header_rlp: Default::default(),
            transactions: Vec::new(),
        }
    }

    #[tokio::test]
    async fn returns_cached_range() {
        let cache = Arc::new(WitnessCache::new());
        cache.insert(block(11));
        cache.insert(block(12));
        let oracle = DebugWitnessOracle::new(cache);

        let range = oracle
            .collect_range_witness(10, 12)
            .await
            .expect("range cached");
        assert_eq!(range.start_block, 10);
        assert_eq!(range.end_block, 12);
        assert_eq!(
            range
                .blocks
                .iter()
                .map(|b| b.block_number)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
    }

    #[tokio::test]
    async fn errors_on_gap() {
        let cache = Arc::new(WitnessCache::new());
        cache.insert(block(11));
        // 12 missing.
        let oracle = DebugWitnessOracle::new(cache);

        let err = oracle
            .collect_range_witness(10, 12)
            .await
            .expect_err("gap should error");
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
    }
}
