//! Optional bounded capture of accepted, ordered Flashblock wire evidence.

use alloy_primitives::Bytes;
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};
use world_chain_primitives::evidence::{FlashblockEvidenceId, MAX_FLASHBLOCK_EVIDENCE_BYTES};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
const RETENTION: Duration = Duration::from_secs(60);

/// Reasons known to this bounded in-memory capture; old metadata may also be evicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceUnavailable {
    NotRetained,
    Expired,
    Evicted,
    NotOrdered,
}

impl EvidenceUnavailable {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRetained => "notRetained",
            Self::Expired => "expired",
            Self::Evicted => "evicted",
            Self::NotOrdered => "notOrdered",
        }
    }
}

#[derive(Debug)]
struct Entry {
    id: FlashblockEvidenceId,
    captured: Instant,
    wire: Option<Bytes>,
    ordered: bool,
    reason: EvidenceUnavailable,
}

#[derive(Debug, Default)]
struct Cache {
    entries: VecDeque<Entry>,
    bytes: usize,
}

/// Clones share one byte/time/entry budget. Lookups do not extend retention.
#[derive(Clone, Debug, Default)]
pub struct FlashblockEvidenceStore(Arc<Mutex<Cache>>);

impl FlashblockEvidenceStore {
    pub(crate) fn capture(&self, id: FlashblockEvidenceId, wire: Bytes) {
        self.capture_at(id, wire, Instant::now(), MAX_BYTES);
    }

    fn capture_at(&self, id: FlashblockEvidenceId, wire: Bytes, now: Instant, max_bytes: usize) {
        let mut cache = self.0.lock();
        Self::expire(&mut cache, now);
        if cache.entries.iter().any(|entry| entry.id == id) {
            return;
        }
        if wire.len() > MAX_FLASHBLOCK_EVIDENCE_BYTES || wire.len() > max_bytes {
            metrics::counter!("worldchain.evidence.capture_rejections", "reason" => "oversize")
                .increment(1);
            return;
        }
        if cache.entries.len() == MAX_ENTRIES
            && let Some(entry) = cache.entries.pop_front()
        {
            cache.bytes -= entry.wire.as_ref().map_or(0, |wire| wire.len());
        }
        for index in 0..cache.entries.len() {
            if cache.bytes + wire.len() <= max_bytes {
                break;
            }
            if let Some(old) = cache.entries[index].wire.take() {
                cache.bytes -= old.len();
                cache.entries[index].reason = EvidenceUnavailable::Evicted;
                metrics::counter!("worldchain.evidence.evictions", "reason" => "bytes")
                    .increment(1);
            }
        }
        cache.bytes += wire.len();
        cache.entries.push_back(Entry {
            id,
            captured: now,
            wire: Some(wire),
            ordered: false,
            reason: EvidenceUnavailable::NotOrdered,
        });
        metrics::counter!("worldchain.evidence.captured").increment(1);
        Self::record_size(&cache);
    }

    pub(crate) fn mark_ordered(&self, id: FlashblockEvidenceId) {
        if let Some(entry) = self
            .0
            .lock()
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
        {
            entry.ordered = true;
        }
    }

    pub fn get(&self, id: FlashblockEvidenceId) -> Result<Bytes, EvidenceUnavailable> {
        self.get_at(id, Instant::now())
    }

    fn get_at(&self, id: FlashblockEvidenceId, now: Instant) -> Result<Bytes, EvidenceUnavailable> {
        let mut cache = self.0.lock();
        Self::expire(&mut cache, now);
        let result = match cache.entries.iter().find(|entry| entry.id == id) {
            Some(entry) => match &entry.wire {
                Some(wire) if entry.ordered => Ok(wire.clone()),
                Some(_) => Err(EvidenceUnavailable::NotOrdered),
                None => Err(entry.reason),
            },
            None => Err(EvidenceUnavailable::NotRetained),
        };
        metrics::counter!("worldchain.evidence.lookups", "result" => result.as_ref().map_or_else(|e| e.as_str(), |_| "found")).increment(1);
        Self::record_size(&cache);
        result
    }

    fn expire(cache: &mut Cache, now: Instant) {
        for entry in &mut cache.entries {
            if now.saturating_duration_since(entry.captured) >= RETENTION
                && let Some(wire) = entry.wire.take()
            {
                cache.bytes -= wire.len();
                entry.reason = EvidenceUnavailable::Expired;
                metrics::counter!("worldchain.evidence.evictions", "reason" => "age").increment(1);
            }
        }
    }

    fn record_size(cache: &Cache) {
        metrics::gauge!("worldchain.evidence.retained_bytes").set(cache.bytes as f64);
        metrics::gauge!("worldchain.evidence.retained_entries").set(cache.entries.len() as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use alloy_rpc_types_engine::PayloadId;

    fn id(index: u64) -> FlashblockEvidenceId {
        FlashblockEvidenceId {
            payload_id: PayloadId::default(),
            index,
            block_hash: B256::ZERO,
        }
    }

    #[test]
    fn ordered_capture_is_immutable_and_duplicate_does_not_refresh_retention() {
        let store = FlashblockEvidenceStore::default();
        let now = Instant::now();
        store.capture_at(id(1), Bytes::from_static(b"original"), now, 16);
        assert_eq!(
            store.get_at(id(1), now),
            Err(EvidenceUnavailable::NotOrdered)
        );
        store.mark_ordered(id(1));
        store.capture_at(
            id(1),
            Bytes::from_static(b"replacement"),
            now + RETENTION / 2,
            16,
        );
        assert_eq!(store.get_at(id(1), now).expect("retained"), b"original"[..]);
        assert_eq!(
            store.get_at(id(1), now + RETENTION),
            Err(EvidenceUnavailable::Expired)
        );
        assert_eq!(store.0.lock().bytes, 0);
        assert_eq!(
            FlashblockEvidenceStore::default().get(id(1)),
            Err(EvidenceUnavailable::NotRetained)
        );
    }

    #[test]
    fn byte_and_metadata_budgets_bound_retention() {
        let store = FlashblockEvidenceStore::default();
        let now = Instant::now();
        for index in 0..3 {
            store.capture_at(id(index), Bytes::from_static(b"abc"), now, 6);
            store.mark_ordered(id(index));
        }
        assert_eq!(store.get_at(id(0), now), Err(EvidenceUnavailable::Evicted));
        assert!(store.get_at(id(2), now).is_ok());
        assert_eq!(store.0.lock().bytes, 6);
        store.capture_at(id(3), Bytes::from_static(b"oversized"), now, 6);
        assert_eq!(
            store.get_at(id(3), now),
            Err(EvidenceUnavailable::NotRetained)
        );
        for index in 3..(MAX_ENTRIES as u64 + 5) {
            store.capture_at(id(index), Bytes::from_static(b"a"), now, 6);
        }
        let cache = store.0.lock();
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(cache.bytes <= 6);
    }
}
