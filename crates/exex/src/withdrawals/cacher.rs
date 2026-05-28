//! Withdrawal cacher core: chain scanning, decoupled from the store.
//!
//! Implements the **Cacher** scan/prune logic from [`wips/wip-1006.md`][wip]
//! §Cacher. Kept independent of the ExEx notification loop so it can be unit
//! tested against synthetic logs/receipts.
//!
//! [`scan_chain`] extracts `MessagePassed` logs from a committed [`Chain`] and
//! persists [`WithdrawalRecord`]s; [`prune_chain`] applies the reorg rule over
//! a reverted chain's block range.

use std::time::{SystemTime, UNIX_EPOCH};

use alloy_consensus::{BlockHeader, TxReceipt};
use alloy_primitives::{Bloom, BloomInput, Log};
use alloy_sol_types::SolEvent;
use reth_execution_types::Chain;
use reth_node_api::NodePrimitives;
use tracing::{debug, warn};

use crate::withdrawals::{
    store::{WithdrawalStore, WithdrawalStoreError},
    types::{MessagePassed, WithdrawalDecodeError, WithdrawalRecord, L2_TO_L1_MESSAGE_PASSER},
};

const TARGET: &str = "exex::relayer";

/// Outcome of scanning a committed chain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// `MessagePassed` logs that decoded and validated into cached records.
    pub cached: usize,
    /// Logs skipped because the recomputed hash did not match the event hash.
    pub rejected: usize,
}

/// Extract and persist every `MessagePassed` withdrawal in the committed
/// chain.
///
/// For each block and its receipts, every log emitted by the
/// `L2ToL1MessagePasser` predeploy whose `topic0` is the `MessagePassed`
/// selector is decoded into a [`WithdrawalRecord`] and written to `store`. The
/// origin block number/hash are taken from the enclosing block. Logs whose
/// recomputed withdrawal hash does not match the event-supplied hash are
/// skipped with a warning (see [`wips/wip-1006.md`][wip] §Cacher).
///
/// Persistence is idempotent, so re-scanning an already-cached range is safe.
///
/// [wip]: ../../../../wips/wip-1006.md
pub fn scan_chain<N>(
    chain: &Chain<N>,
    store: &WithdrawalStore,
) -> Result<ScanStats, WithdrawalStoreError>
where
    N: NodePrimitives,
    N::Receipt: TxReceipt<Log = Log>,
{
    let observed_at_unix = now_unix();
    let mut stats = ScanStats::default();

    for (block, receipts) in chain.blocks_and_receipts() {
        let block_number = block.header().number();
        let block_hash = block.hash();

        // Logs-bloom pre-filter: the header bloom is a probabilistic filter
        // with no false negatives, so a non-match is a *guaranteed* absence of
        // any `L2ToL1MessagePasser` log in this block — we can skip the receipt
        // scan entirely. A match may be a false positive, so matching blocks
        // still go through the full per-receipt decode + hash validation below.
        if !block_may_contain_withdrawals(&block.header().logs_bloom()) {
            continue;
        }

        for receipt in receipts {
            for log in receipt.logs() {
                match record_from_log(log, block_number, block_hash, observed_at_unix) {
                    Ok(Some(record)) => {
                        store.put_withdrawal(&record)?;
                        stats.cached += 1;
                        debug!(
                            target: TARGET,
                            withdrawal_hash = %record.withdrawal_hash,
                            block = block_number,
                            "cached withdrawal",
                        );
                    }
                    // Not a MessagePassed log from the predeploy — ignore.
                    Ok(None) => {}
                    Err(e) => {
                        stats.rejected += 1;
                        warn!(
                            target: TARGET,
                            block = block_number,
                            error = %e,
                            "skipping MessagePassed log with mismatched withdrawal hash",
                        );
                    }
                }
            }
        }
    }

    if stats.cached > 0 || stats.rejected > 0 {
        debug!(
            target: TARGET,
            cached = stats.cached,
            rejected = stats.rejected,
            range = ?chain.range(),
            "scanned committed chain for withdrawals",
        );
    }
    Ok(stats)
}

/// Apply the reorg rule over the reverted chain's block range.
///
/// Delegates to [`WithdrawalStore::prune_range`]: `Cached` records whose origin
/// block falls in the reverted range are deleted, while `Proven`/`Finalized`
/// records are retained and marked `Orphaned`.
pub fn prune_chain<N>(chain: &Chain<N>, store: &WithdrawalStore) -> Result<(), WithdrawalStoreError>
where
    N: NodePrimitives,
{
    let range = chain.range();
    debug!(target: TARGET, range = ?range, "pruning reverted chain from withdrawal cache");
    store.prune_range(range)
}

/// Whether a block's header logs-bloom *may* contain an `L2ToL1MessagePasser`
/// log, and therefore warrants a full receipt scan.
///
/// A bloom filter has no false negatives: if the predeploy address is not in
/// the bloom, the block provably emitted no log from that address, so the scan
/// can be skipped. A `true` result may be a false positive and is resolved by
/// the per-receipt decode + hash validation in [`scan_chain`].
fn block_may_contain_withdrawals(bloom: &Bloom) -> bool {
    bloom.contains_input(BloomInput::Raw(L2_TO_L1_MESSAGE_PASSER.as_slice()))
}

/// Decode a single log into a withdrawal record, if it is a `MessagePassed`
/// event emitted by the `L2ToL1MessagePasser` predeploy.
///
/// Returns:
/// * `Ok(Some(record))` for a valid `MessagePassed` log,
/// * `Ok(None)` if the log is not a `MessagePassed` log from the predeploy,
/// * `Err(_)` if it decodes as `MessagePassed` but fails hash validation.
fn record_from_log(
    log: &Log,
    block_number: u64,
    block_hash: alloy_primitives::B256,
    observed_at_unix: u64,
) -> Result<Option<WithdrawalRecord>, WithdrawalDecodeError> {
    if log.address != L2_TO_L1_MESSAGE_PASSER {
        return Ok(None);
    }
    match log.topics().first() {
        Some(topic0) if *topic0 == MessagePassed::SIGNATURE_HASH => {}
        _ => return Ok(None),
    }
    // Decode the ABI body + indexed topics. A predeploy log with the correct
    // selector but malformed body is treated as a non-match (defensive: such a
    // log cannot have been emitted by the canonical predeploy).
    let Ok(event) = MessagePassed::decode_log_data(&log.data) else {
        return Ok(None);
    };
    let record = WithdrawalRecord::from_event(&event, block_number, block_hash, observed_at_unix)?;
    Ok(Some(record))
}

/// Current unix time in seconds, saturating to `0` before the epoch.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::withdrawals::types::{message_slot, withdrawal_hash, WithdrawalTransaction};
    use alloy_primitives::{Address, Bytes, LogData, B256, U256};

    fn sample_tx(seed: u8) -> WithdrawalTransaction {
        WithdrawalTransaction {
            nonce: U256::from(seed),
            sender: Address::repeat_byte(seed),
            target: Address::repeat_byte(seed.wrapping_add(1)),
            value: U256::from(seed as u64 * 7),
            gas_limit: U256::from(50_000u64),
            data: Bytes::from(vec![seed; 3]),
        }
    }

    /// Build a real `MessagePassed` log (correct topics + ABI body) for `tx`.
    fn message_passed_log(tx: &WithdrawalTransaction, addr: Address) -> Log {
        let event = MessagePassed {
            nonce: tx.nonce,
            sender: tx.sender,
            target: tx.target,
            value: tx.value,
            gasLimit: tx.gas_limit,
            data: tx.data.clone(),
            withdrawalHash: withdrawal_hash(tx),
        };
        let data: LogData = event.encode_log_data();
        Log {
            address: addr,
            data,
        }
    }

    #[test]
    fn record_from_valid_log() {
        let tx = sample_tx(1);
        let log = message_passed_log(&tx, L2_TO_L1_MESSAGE_PASSER);
        let rec = record_from_log(&log, 42, B256::repeat_byte(0xaa), 99)
            .unwrap()
            .unwrap();
        assert_eq!(rec.withdrawal_hash, withdrawal_hash(&tx));
        assert_eq!(rec.message_slot, message_slot(rec.withdrawal_hash));
        assert_eq!(rec.l2_block_number, 42);
        assert_eq!(rec.l2_block_hash, B256::repeat_byte(0xaa));
    }

    #[test]
    fn ignores_log_from_other_address() {
        let tx = sample_tx(2);
        let log = message_passed_log(&tx, Address::repeat_byte(0x99));
        assert!(record_from_log(&log, 1, B256::ZERO, 0).unwrap().is_none());
    }

    #[test]
    fn ignores_non_message_passed_topic() {
        // Right address, wrong topic0.
        let log = Log {
            address: L2_TO_L1_MESSAGE_PASSER,
            data: LogData::new_unchecked(vec![B256::repeat_byte(0x01)], Bytes::new()),
        };
        assert!(record_from_log(&log, 1, B256::ZERO, 0).unwrap().is_none());
    }

    #[test]
    fn rejects_mismatched_hash() {
        let tx = sample_tx(3);
        // Tamper: build the canonical log, then overwrite the trailing
        // withdrawalHash word in the ABI body so validation fails.
        let event = MessagePassed {
            nonce: tx.nonce,
            sender: tx.sender,
            target: tx.target,
            value: tx.value,
            gasLimit: tx.gas_limit,
            data: tx.data.clone(),
            withdrawalHash: B256::ZERO, // wrong
        };
        let log = Log {
            address: L2_TO_L1_MESSAGE_PASSER,
            data: event.encode_log_data(),
        };
        let err = record_from_log(&log, 1, B256::ZERO, 0).unwrap_err();
        assert!(matches!(err, WithdrawalDecodeError::HashMismatch { .. }));
    }

    /// An empty bloom (genesis-style / no logs) never matches, so such blocks
    /// are skipped without inspecting receipts.
    #[test]
    fn bloom_prefilter_skips_empty_bloom() {
        assert!(!block_may_contain_withdrawals(&Bloom::ZERO));
    }

    /// A bloom accrued only from an *unrelated* address must not match the
    /// predeploy — the pre-filter skips the block. (Bloom has no false
    /// negatives, so this skip is correctness-preserving.)
    #[test]
    fn bloom_prefilter_skips_unrelated_logs() {
        let mut bloom = Bloom::ZERO;
        // Accrue a log from some other contract with an arbitrary topic.
        bloom.accrue_raw_log(Address::repeat_byte(0x99), &[B256::repeat_byte(0x01)]);
        assert!(!block_may_contain_withdrawals(&bloom));
    }

    /// A bloom accrued from an `L2ToL1MessagePasser` log matches, so the block
    /// proceeds to the full per-receipt scan.
    #[test]
    fn bloom_prefilter_matches_predeploy_logs() {
        let mut bloom = Bloom::ZERO;
        bloom.accrue_raw_log(L2_TO_L1_MESSAGE_PASSER, &[MessagePassed::SIGNATURE_HASH]);
        assert!(block_may_contain_withdrawals(&bloom));
    }

    /// Sanity: a block's bloom that contains a real `MessagePassed` log matches,
    /// and the same log still decodes through `record_from_log` (the path the
    /// matched block takes). This ties the pre-filter to the positive scan path
    /// without constructing a full `Chain<N>`.
    #[test]
    fn bloom_match_aligns_with_record_decode() {
        let tx = sample_tx(7);
        let log = message_passed_log(&tx, L2_TO_L1_MESSAGE_PASSER);

        let mut bloom = Bloom::ZERO;
        bloom.accrue_raw_log(log.address, log.topics());
        assert!(block_may_contain_withdrawals(&bloom));

        // The matched block would scan this log and cache it.
        assert!(record_from_log(&log, 1, B256::ZERO, 0).unwrap().is_some());
    }
}
