//! Gas-based range planning for multi-range SP1 validity proofs.
//!
//! Range-proof cycles scale with the L2 gas executed in the range, so ranges are packed by
//! cumulative gas rather than block count. Blocks are fetched over RPC, greedily packed up to
//! a target gas budget, and clamped to a maximum block count (which bounds witness size and
//! build time). A range the SP1 Network still reports unexecutable is bisected at its block
//! midpoint up to a bounded depth.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use futures::stream::{FuturesOrdered, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

/// Blocks requested per JSON-RPC batch when fetching per-block gas.
const GAS_FETCH_BATCH_SIZE: u64 = 256;
/// Concurrent in-flight gas-fetch batch requests.
const GAS_FETCH_MAX_CONCURRENCY: usize = 8;

/// Default SP1 range-proof cycle ceiling; the planning gas target derives from it.
pub const DEFAULT_RANGE_CYCLE_LIMIT: u64 = 1_500_000_000_000;
/// Conservative worst-case zkVM cycles per L2 gas (keccak/precompile-heavy blocks); typical
/// blocks measure ~5-15. Unmeasured on World Chain — replace once `prover-sp1 execute`
/// numbers exist.
pub const DEFAULT_CYCLES_PER_GAS: u64 = 25;
/// Caps witness size and build time per range regardless of how empty the blocks are.
pub const DEFAULT_MAX_BLOCKS_PER_RANGE: u64 = 1_000;
/// Two bisections shrink a mis-estimated range to a quarter of its planned gas.
pub const DEFAULT_MAX_RANGE_SPLITS: u32 = 2;

/// Headroom divisor between the derived gas target and the configured cycle ceiling.
const RANGE_GAS_HEADROOM: u64 = 2;

/// Derives the per-range gas target keeping a `cycles_per_gas` worst-case range at
/// `1/RANGE_GAS_HEADROOM` of the configured cycle ceiling.
pub const fn target_gas_per_range(range_cycle_limit: u64, cycles_per_gas: u64) -> u64 {
    let cycles_per_gas = if cycles_per_gas == 0 {
        1
    } else {
        cycles_per_gas
    };
    let target = range_cycle_limit / cycles_per_gas / RANGE_GAS_HEADROOM;
    if target == 0 { 1 } else { target }
}

/// Limits controlling how a proof interval is split into range proofs.
#[derive(Clone, Copy, Debug)]
pub struct RangePlanConfig {
    /// Target cumulative L2 gas per range proof, derived via [`target_gas_per_range`].
    pub target_gas_per_range: u64,
    /// Maximum L2 blocks per range proof.
    pub max_blocks_per_range: u64,
    /// Maximum times one range may be bisected after `RequestUnexecutable`.
    pub max_range_splits: u32,
}

impl Default for RangePlanConfig {
    fn default() -> Self {
        Self {
            target_gas_per_range: target_gas_per_range(
                DEFAULT_RANGE_CYCLE_LIMIT,
                DEFAULT_CYCLES_PER_GAS,
            ),
            max_blocks_per_range: DEFAULT_MAX_BLOCKS_PER_RANGE,
            max_range_splits: DEFAULT_MAX_RANGE_SPLITS,
        }
    }
}

/// One planned range proof over L2 blocks `(start_block, end_block]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRange {
    /// Exclusive lower bound: the agreed parent block.
    pub start_block: u64,
    /// Inclusive upper bound: the last proved block.
    pub end_block: u64,
    /// Backend session id of the in-flight or completed range proof, if submitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// How many bisections produced this range.
    #[serde(default)]
    pub splits: u32,
}

impl PlannedRange {
    /// A fresh unsubmitted range.
    pub const fn new(start_block: u64, end_block: u64) -> Self {
        Self {
            start_block,
            end_block,
            session_id: None,
            splits: 0,
        }
    }

    /// Number of L2 blocks covered by this range.
    pub const fn block_count(&self) -> u64 {
        self.end_block - self.start_block
    }

    /// Splits this range at its block midpoint, or `None` for a single-block range.
    pub fn bisect(&self) -> Option<(Self, Self)> {
        if self.block_count() < 2 {
            return None;
        }
        let mid = self.start_block + self.block_count() / 2;
        let splits = self.splits + 1;
        Some((
            Self {
                start_block: self.start_block,
                end_block: mid,
                session_id: None,
                splits,
            },
            Self {
                start_block: mid,
                end_block: self.end_block,
                session_id: None,
                splits,
            },
        ))
    }
}

/// Contiguous ordered set of planned ranges covering one proof interval.
///
/// Serialized into the job's single Stark session slot so a restarted worker resumes the
/// in-flight SP1 Network requests instead of re-proving. The prover service treats the value
/// as opaque.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangePlan {
    /// Plan document version.
    pub v: u32,
    /// Planned ranges in ascending block order.
    pub ranges: Vec<PlannedRange>,
}

impl RangePlan {
    /// Packs `(start_block, end_block]` into ranges by cumulative gas.
    ///
    /// `gas_by_block[i]` is the gas used by block `start_block + 1 + i`. A block whose gas
    /// alone exceeds the target still gets its own single-block range; bisection depth is the
    /// only recourse below one block.
    pub fn by_gas(
        start_block: u64,
        end_block: u64,
        gas_by_block: &[u64],
        config: &RangePlanConfig,
    ) -> anyhow::Result<Self> {
        if end_block <= start_block {
            bail!("end block {end_block} must be greater than start block {start_block}");
        }
        let block_count = end_block - start_block;
        if gas_by_block.len() as u64 != block_count {
            bail!(
                "gas data covers {} blocks but range ({start_block}, {end_block}] has {block_count}",
                gas_by_block.len()
            );
        }
        let target_gas = config.target_gas_per_range.max(1);
        let max_blocks = config.max_blocks_per_range.max(1);

        let mut ranges = Vec::new();
        let mut range_start = start_block;
        let mut range_gas = 0u64;
        for (offset, gas) in gas_by_block.iter().enumerate() {
            let block = start_block + 1 + offset as u64;
            let would_exceed_gas = range_gas.saturating_add(*gas) > target_gas;
            let range_is_empty = block == range_start + 1;
            if (would_exceed_gas && !range_is_empty) || block - range_start > max_blocks {
                ranges.push(PlannedRange::new(range_start, block - 1));
                range_start = block - 1;
                range_gas = 0;
            }
            range_gas = range_gas.saturating_add(*gas);
        }
        ranges.push(PlannedRange::new(range_start, end_block));

        Ok(Self { v: 1, ranges })
    }

    /// A plan of one range covering the whole interval.
    pub fn single(start_block: u64, end_block: u64, session_id: Option<String>) -> Self {
        Self {
            v: 1,
            ranges: vec![PlannedRange {
                start_block,
                end_block,
                session_id,
                splits: 0,
            }],
        }
    }

    /// Encodes the plan for the Stark session slot.
    ///
    /// A single never-split range with a session id round-trips as the bare backend id so
    /// workers on either side of this feature can resume each other's single-range jobs.
    pub fn encode(&self) -> anyhow::Result<String> {
        if let [range] = self.ranges.as_slice()
            && range.splits == 0
            && let Some(session_id) = &range.session_id
        {
            return Ok(session_id.clone());
        }
        serde_json::to_string(self).context("failed to encode range plan")
    }

    /// Decodes a Stark session slot value: a plan document, or a legacy bare session id
    /// interpreted as one range covering `(start_block, end_block]`.
    pub fn decode(value: &str, start_block: u64, end_block: u64) -> Self {
        match serde_json::from_str::<Self>(value) {
            Ok(plan) if !plan.ranges.is_empty() => plan,
            _ => Self::single(start_block, end_block, Some(value.to_string())),
        }
    }

    /// Whether the plan's ranges tile `(start_block, end_block]` contiguously in order.
    pub fn covers(&self, start_block: u64, end_block: u64) -> bool {
        let mut cursor = start_block;
        for range in &self.ranges {
            if range.start_block != cursor || range.end_block <= range.start_block {
                return false;
            }
            cursor = range.end_block;
        }
        cursor == end_block
    }
}

/// Fetches per-block `gasUsed` for L2 blocks `(start_block, end_block]`, returned in ascending
/// block order. Batched JSON-RPC calls run concurrently, bounded by a semaphore.
pub async fn fetch_range_gas(
    client: &reqwest::Client,
    l2_rpc: &str,
    start_block: u64,
    end_block: u64,
) -> anyhow::Result<Vec<u64>> {
    if end_block <= start_block {
        bail!("end block {end_block} must be greater than start block {start_block}");
    }

    let semaphore = Arc::new(Semaphore::new(GAS_FETCH_MAX_CONCURRENCY));
    let mut batches = FuturesOrdered::new();
    let mut batch_start = start_block + 1;
    while batch_start <= end_block {
        let batch_end = batch_start
            .saturating_add(GAS_FETCH_BATCH_SIZE - 1)
            .min(end_block);
        let client = client.clone();
        let l2_rpc = l2_rpc.to_string();
        let semaphore = Arc::clone(&semaphore);
        batches.push_back(async move {
            let _permit = semaphore
                .acquire()
                .await
                .context("gas fetch semaphore closed")?;
            fetch_gas_batch(&client, &l2_rpc, batch_start, batch_end)
                .await
                .with_context(|| format!("gas fetch batch ({batch_start}..={batch_end}) failed"))
        });
        batch_start = batch_end + 1;
    }

    let mut gas = Vec::with_capacity((end_block - start_block) as usize);
    while let Some(batch) = batches.next().await {
        gas.extend(batch?);
    }
    Ok(gas)
}

/// Fetches `gasUsed` for blocks `batch_start..=batch_end` in one JSON-RPC batch call.
async fn fetch_gas_batch(
    client: &reqwest::Client,
    l2_rpc: &str,
    batch_start: u64,
    batch_end: u64,
) -> anyhow::Result<Vec<u64>> {
    let requests: Vec<Value> = (batch_start..=batch_end)
        .map(|block| {
            json!({
                "jsonrpc": "2.0",
                "id": block,
                "method": "eth_getBlockByNumber",
                "params": [format!("0x{block:x}"), false],
            })
        })
        .collect();
    let responses: Vec<Value> = client
        .post(l2_rpc)
        .json(&requests)
        .send()
        .await
        .context("gas fetch batch request failed")?
        .error_for_status()
        .context("gas fetch batch returned HTTP error")?
        .json()
        .await
        .context("failed to decode gas fetch batch response")?;
    if responses.len() != requests.len() {
        bail!(
            "gas fetch batch returned {} responses for {} requests",
            responses.len(),
            requests.len()
        );
    }

    // Batch responses may arrive in any order; key them back by request id.
    let mut by_block = HashMap::with_capacity(responses.len());
    for response in responses {
        let block = response
            .get("id")
            .and_then(Value::as_u64)
            .context("gas fetch response is missing its request id")?;
        let gas_used = response
            .get("result")
            .and_then(|result| result.get("gasUsed"))
            .and_then(Value::as_str)
            .with_context(|| format!("block {block} response is missing gasUsed"))?;
        let gas_used = u64::from_str_radix(gas_used.trim_start_matches("0x"), 16)
            .with_context(|| format!("block {block} has invalid gasUsed"))?;
        by_block.insert(block, gas_used);
    }

    (batch_start..=batch_end)
        .map(|block| {
            by_block
                .remove(&block)
                .with_context(|| format!("gas fetch batch is missing block {block}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: RangePlanConfig = RangePlanConfig {
        target_gas_per_range: 100,
        max_blocks_per_range: 4,
        max_range_splits: 2,
    };

    #[test]
    fn gas_target_derives_from_the_cycle_ceiling() {
        // 1.5T cycles / 25 cycles-per-gas / 2 headroom = 30B gas.
        assert_eq!(
            target_gas_per_range(DEFAULT_RANGE_CYCLE_LIMIT, DEFAULT_CYCLES_PER_GAS),
            30_000_000_000
        );
        assert_eq!(target_gas_per_range(1_000, 25), 20);
        // Never zero, even when the ceiling is smaller than one gas worth of cycles.
        assert_eq!(target_gas_per_range(10, 25), 1);
        assert_eq!(
            target_gas_per_range(1_000, 0),
            target_gas_per_range(1_000, 1)
        );
    }

    #[test]
    fn packs_blocks_up_to_the_gas_target() {
        let plan = RangePlan::by_gas(10, 16, &[40, 40, 40, 40, 40, 40], &CONFIG).unwrap();

        assert_eq!(
            plan.ranges,
            vec![
                PlannedRange::new(10, 12),
                PlannedRange::new(12, 14),
                PlannedRange::new(14, 16),
            ]
        );
        assert!(plan.covers(10, 16));
    }

    #[test]
    fn oversized_block_gets_its_own_range() {
        let plan = RangePlan::by_gas(0, 3, &[500, 10, 10], &CONFIG).unwrap();

        assert_eq!(
            plan.ranges,
            vec![PlannedRange::new(0, 1), PlannedRange::new(1, 3)]
        );
    }

    #[test]
    fn clamps_ranges_to_max_blocks() {
        let plan = RangePlan::by_gas(0, 10, &[1; 10], &CONFIG).unwrap();

        assert_eq!(
            plan.ranges,
            vec![
                PlannedRange::new(0, 4),
                PlannedRange::new(4, 8),
                PlannedRange::new(8, 10),
            ]
        );
    }

    #[test]
    fn rejects_mismatched_gas_data() {
        assert!(RangePlan::by_gas(0, 3, &[1, 2], &CONFIG).is_err());
        assert!(RangePlan::by_gas(3, 3, &[], &CONFIG).is_err());
    }

    #[test]
    fn bisect_splits_at_the_midpoint_and_tracks_depth() {
        let range = PlannedRange {
            session_id: Some("0xabc".to_string()),
            ..PlannedRange::new(10, 15)
        };

        let (low, high) = range.bisect().unwrap();

        assert_eq!((low.start_block, low.end_block), (10, 12));
        assert_eq!((high.start_block, high.end_block), (12, 15));
        assert_eq!((low.splits, high.splits), (1, 1));
        assert_eq!((low.session_id, high.session_id), (None, None));
        assert!(PlannedRange::new(10, 11).bisect().is_none());
    }

    #[test]
    fn single_unsplit_range_encodes_as_the_bare_session_id() {
        let plan = RangePlan::single(5, 9, Some("0xdeadbeef".to_string()));

        assert_eq!(plan.encode().unwrap(), "0xdeadbeef");
        assert_eq!(RangePlan::decode("0xdeadbeef", 5, 9), plan);
    }

    #[test]
    fn multi_range_plans_round_trip_through_json() {
        let mut plan = RangePlan::by_gas(0, 4, &[60, 60, 60, 60], &CONFIG).unwrap();
        plan.ranges[0].session_id = Some("0x01".to_string());

        let encoded = plan.encode().unwrap();

        assert!(encoded.starts_with('{'));
        assert_eq!(RangePlan::decode(&encoded, 0, 4), plan);
    }

    #[test]
    fn covers_rejects_gaps_and_reorders() {
        let mut plan = RangePlan::by_gas(0, 6, &[60; 6], &CONFIG).unwrap();
        assert!(plan.covers(0, 6));

        plan.ranges.swap(0, 1);
        assert!(!plan.covers(0, 6));
    }
}
