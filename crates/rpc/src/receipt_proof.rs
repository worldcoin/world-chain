//! Read-only proofs for an exact view backed by both validated execution and signed evidence.

use std::{
    collections::VecDeque,
    mem::size_of,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_consensus::{Header, ReceiptWithBloom, TxReceipt, transaction::TxHashRef};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{B256, Bloom, Bytes};
use jsonrpsee::{core::RpcResult, proc_macros::rpc, types::ErrorObjectOwned};
use parking_lot::Mutex;
use reth_optimism_forks::OpHardforks;
use reth_optimism_primitives::{DepositReceipt, OpReceipt};
use reth_storage_api::BlockNumReader;
use reth_tasks::TaskExecutor;
use serde::Serialize;
use tokio::sync::{Semaphore, broadcast::error::RecvError};
use tracing::warn;
use world_chain_chainspec::WorldChainSpec;
use world_chain_p2p::evidence::FlashblockEvidenceStore;
use world_chain_primitives::{
    evidence::FlashblockEvidenceId,
    receipt_proof::{
        FlashblockReceiptProof, FlashblockReceipts, MAX_TRANSACTIONS, MAX_VIEW_BYTES, ordered_proof,
    },
};
use world_chain_validator::{
    coordinator::FlashblocksExecutionCoordinator,
    preconfirmed::{ValidatedEvent, ValidatedFlashblock, ValidatedSnapshot},
};

const MAX_CACHED_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHED_VIEWS: usize = 64;
const MAX_CONCURRENT_REQUESTS: usize = 2;
const RETENTION: Duration = Duration::from_secs(60);
const UNAVAILABLE: i32 = -32082;

#[rpc(server, client, namespace = "worldchain")]
pub trait FlashblockReceiptProofApi {
    #[method(name = "getFlashblockReceiptProof")]
    async fn get_flashblock_receipt_proof(
        &self,
        id: FlashblockEvidenceId,
        transaction_hash: B256,
    ) -> RpcResult<FlashblockReceiptProof>;

    #[method(name = "getFlashblockReceipts")]
    async fn get_flashblock_receipts(
        &self,
        id: FlashblockEvidenceId,
    ) -> RpcResult<FlashblockReceipts>;
}

#[derive(Clone)]
pub struct FlashblockReceiptProofRpc<P> {
    provider: P,
    coordinator: FlashblocksExecutionCoordinator,
    evidence: FlashblockEvidenceStore,
    cache: Arc<Mutex<Cache>>,
    permits: Arc<Semaphore>,
}

impl<P> FlashblockReceiptProofRpc<P> {
    pub fn spawn(
        provider: P,
        chain_spec: Arc<WorldChainSpec>,
        coordinator: FlashblocksExecutionCoordinator,
        evidence: FlashblockEvidenceStore,
        executor: &TaskExecutor,
    ) -> Self {
        let cache = Arc::new(Mutex::new(Cache::default()));
        let (snapshot, mut events) = coordinator.subscribe_validated();
        let guard = CaptureGuard(cache.clone());
        let source = coordinator.clone();
        executor.spawn_task(async move {
            let guard = guard;
            let mut next = snapshot.current;
            loop {
                if let Some(payload) = next.take() {
                    let id = view_id(&payload);
                    let generation = payload.canonical_generation;
                    let at = payload.validated_at;
                    if check_view(&source.snapshot_validated(), id, generation).is_ok()
                        && at.elapsed() < RETENTION
                    {
                        let spec = chain_spec.clone();
                        let started = Instant::now();
                        match tokio::task::spawn_blocking(move || Capture::encode(&payload, &spec)).await {
                            Ok(result) => {
                                if let Err(reason) = &result {
                                    metrics::counter!("worldchain.receipt_proofs.capture_rejections",
                                        "reason" => *reason).increment(1);
                                }
                                if check_view(&source.snapshot_validated(), id, generation).is_ok() {
                                    guard.0.lock().insert(Entry { id, generation, at, result: result.map(Arc::new) });
                                }
                                metrics::histogram!("worldchain.receipt_proofs.capture_seconds")
                                    .record(started.elapsed().as_secs_f64());
                            }
                            Err(error) => {
                                warn!(target: "rpc::receipt_proofs", %error, "Receipt proof capture worker stopped");
                                return;
                            }
                        }
                    }
                }
                next = match events.recv().await {
                    Ok(ValidatedEvent::Payload(payload)) => Some(payload),
                    Ok(ValidatedEvent::Reset { .. }) => None,
                    Err(RecvError::Lagged(_)) => {
                        metrics::counter!("worldchain.receipt_proofs.capture_lag").increment(1);
                        source.snapshot_validated().current
                    }
                    Err(RecvError::Closed) => return,
                };
            }
        });
        Self {
            provider,
            coordinator,
            evidence,
            cache,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS)),
        }
    }
}

impl<P: BlockNumReader + Clone + Sync + 'static> FlashblockReceiptProofRpc<P> {
    async fn serve<T: Send + 'static>(
        &self,
        id: FlashblockEvidenceId,
        compute: impl FnOnce(&Capture) -> Result<T, &'static str> + Send + 'static,
    ) -> RpcResult<T> {
        let error = |reason| unavailable(id, reason);
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| error("busy"))?;
        let rpc = self.clone();
        tokio::task::spawn_blocking(move || {
            // The permit stays with the worker even if the client cancels the request.
            let _permit = permit;
            let started = Instant::now();
            let result = rpc.compute(id, compute);
            metrics::histogram!("worldchain.receipt_proofs.request_seconds")
                .record(started.elapsed().as_secs_f64());
            result
        })
        .await
        .map_err(|join_error| {
            warn!(target: "rpc::receipt_proofs", %join_error, "Receipt proof request worker failed");
            error("unavailable")
        })?
        .map_err(error)
    }

    fn compute<T>(
        &self,
        id: FlashblockEvidenceId,
        compute: impl FnOnce(&Capture) -> Result<T, &'static str>,
    ) -> Result<T, &'static str> {
        self.evidence.get(id).map_err(|reason| reason.as_str())?;
        let snapshot = self.coordinator.snapshot_validated();
        let entry = self.cache.lock().get(id, snapshot.canonical_generation)?;
        self.check_checkpoint(&snapshot, id, entry.generation)?;
        let result = compute(entry.result.as_ref().map_err(|reason| *reason)?)?;
        if entry.at.elapsed() >= RETENTION {
            return Err("expired");
        }
        self.evidence.get(id).map_err(|reason| reason.as_str())?;
        self.check_checkpoint(&self.coordinator.snapshot_validated(), id, entry.generation)?;
        Ok(result)
    }

    fn check_checkpoint(
        &self,
        snapshot: &ValidatedSnapshot,
        id: FlashblockEvidenceId,
        generation: u64,
    ) -> Result<(), &'static str> {
        check_view(snapshot, id, generation)?;
        let tip = snapshot.canonical_tip.ok_or("unavailable")?;
        let info = self.provider.chain_info().map_err(|error| {
            warn!(target: "rpc::receipt_proofs", %error, "Could not read receipt proof canonical checkpoint");
            "unavailable"
        })?;
        if info.best_number != tip.number || info.best_hash != tip.hash {
            return Err("replaced");
        }
        Ok(())
    }
}

#[jsonrpsee::core::async_trait]
impl<P: BlockNumReader + Clone + Sync + 'static> FlashblockReceiptProofApiServer
    for FlashblockReceiptProofRpc<P>
{
    async fn get_flashblock_receipt_proof(
        &self,
        id: FlashblockEvidenceId,
        transaction_hash: B256,
    ) -> RpcResult<FlashblockReceiptProof> {
        self.serve(id, move |view| view.proof(id, transaction_hash))
            .await
    }

    async fn get_flashblock_receipts(
        &self,
        id: FlashblockEvidenceId,
    ) -> RpcResult<FlashblockReceipts> {
        self.serve(id, move |view| view.receipts(id)).await
    }
}

fn unavailable(id: FlashblockEvidenceId, reason: &'static str) -> ErrorObjectOwned {
    #[derive(Serialize)]
    struct Unavailable {
        id: FlashblockEvidenceId,
        reason: &'static str,
    }
    metrics::counter!("worldchain.receipt_proofs.unavailable", "reason" => reason).increment(1);
    ErrorObjectOwned::owned(
        UNAVAILABLE,
        "Flashblock receipt proof unavailable",
        Some(Unavailable { id, reason }),
    )
}

fn view_id(payload: &ValidatedFlashblock) -> FlashblockEvidenceId {
    FlashblockEvidenceId {
        payload_id: payload.payload_id,
        index: payload.index,
        block_hash: payload.executed.recovered_block().hash(),
    }
}

fn check_view(
    snapshot: &ValidatedSnapshot,
    id: FlashblockEvidenceId,
    generation: u64,
) -> Result<(), &'static str> {
    if snapshot.canonical_generation != generation
        || !snapshot.current.as_ref().is_some_and(|current| {
            current.payload_id == id.payload_id
                && (current.index > id.index || view_id(current) == id)
        })
    {
        return Err("replaced");
    }
    Ok(())
}

struct Capture {
    header: Bytes,
    transactions_root: B256,
    receipts_root: B256,
    hashes: Vec<B256>,
    transactions: Vec<Bytes>,
    receipts: Vec<Bytes>,
    weight: usize,
}

impl Capture {
    fn encode(payload: &ValidatedFlashblock, spec: &WorldChainSpec) -> Result<Self, &'static str> {
        let block = payload.executed.recovered_block();
        let header = block.header();
        let transactions = &block.body().transactions;
        let receipts = &payload.executed.execution_outcome().receipts;
        Self::from_block(
            header,
            transactions,
            receipts,
            spec.is_regolith_active_at_timestamp(header.timestamp)
                && !spec.is_canyon_active_at_timestamp(header.timestamp),
        )
    }

    fn from_block<T: Encodable2718 + TxHashRef>(
        header: &Header,
        transactions: &[T],
        receipts: &[OpReceipt],
        strip_deposit_nonce: bool,
    ) -> Result<Self, &'static str> {
        let count = transactions.len();
        if count != receipts.len() {
            return Err("inconsistentExecution");
        }
        if count > MAX_TRANSACTIONS {
            return Err("limitExceeded");
        }
        let raw_header = Bytes::from(alloy_rlp::encode(header));
        let mut remaining = MAX_VIEW_BYTES
            .checked_sub(raw_header.len())
            .ok_or("limitExceeded")?;
        let mut hashes = Vec::with_capacity(count);
        let mut encoded_transactions = Vec::with_capacity(count);
        let mut encoded_receipts = Vec::with_capacity(count);
        for (transaction, receipt) in transactions.iter().zip(receipts) {
            // A zero bloom has the same RLP length and avoids hashing logs before admission.
            let receipt_len = ReceiptWithBloom {
                receipt,
                logs_bloom: Bloom::ZERO,
            }
            .encode_2718_len();
            remaining = remaining
                .checked_sub(transaction.encode_2718_len())
                .and_then(|left| left.checked_sub(receipt_len))
                .ok_or("limitExceeded")?;
            hashes.push(*transaction.tx_hash());
            encoded_transactions.push(transaction.encoded_2718().into());
            let receipt = encode_receipt(receipt, strip_deposit_nonce);
            encoded_receipts.push(receipt);
        }
        Ok(Self {
            header: raw_header,
            transactions_root: header.transactions_root,
            receipts_root: header.receipts_root,
            hashes,
            transactions: encoded_transactions,
            receipts: encoded_receipts,
            weight: MAX_VIEW_BYTES - remaining
                + count * (2 * size_of::<Bytes>() + size_of::<B256>()),
        })
    }

    fn proof(
        &self,
        id: FlashblockEvidenceId,
        hash: B256,
    ) -> Result<FlashblockReceiptProof, &'static str> {
        let index = self
            .hashes
            .iter()
            .position(|known| *known == hash)
            .ok_or("transactionNotFound")?;
        let (tx_root, transaction_proof) =
            ordered_proof(&self.transactions, index).ok_or("inconsistentExecution")?;
        let (receipts_root, receipt_proof) =
            ordered_proof(&self.receipts, index).ok_or("inconsistentExecution")?;
        if tx_root != self.transactions_root || receipts_root != self.receipts_root {
            return Err("rootMismatch");
        }
        let proof = FlashblockReceiptProof {
            version: 1,
            id,
            header: self.header.clone(),
            transaction_index: index as u64,
            transaction: self.transactions[index].clone(),
            receipt: self.receipts[index].clone(),
            transaction_proof,
            receipt_proof,
        };
        proof.check_limits().map_err(|_| "limitExceeded")?;
        Ok(proof)
    }

    fn receipts(&self, id: FlashblockEvidenceId) -> Result<FlashblockReceipts, &'static str> {
        if alloy_trie::root::ordered_trie_root_encoded(&self.receipts) != self.receipts_root {
            return Err("rootMismatch");
        }
        Ok(FlashblockReceipts {
            version: 1,
            id,
            header: self.header.clone(),
            receipts: self.receipts.clone(),
        })
    }
}

fn encode_receipt(receipt: &OpReceipt, strip_deposit_nonce: bool) -> Bytes {
    // Match the Regolith receipt-root exception in OP consensus until Canyon.
    if strip_deposit_nonce && receipt.as_deposit_receipt().is_some() {
        let mut receipt = receipt.clone();
        if let Some(deposit) = receipt.as_deposit_receipt_mut() {
            deposit.deposit_nonce = None;
        }
        return receipt.with_bloom_ref().encoded_2718().into();
    }
    receipt.with_bloom_ref().encoded_2718().into()
}

#[derive(Clone)]
struct Entry {
    id: FlashblockEvidenceId,
    generation: u64,
    at: Instant,
    result: Result<Arc<Capture>, &'static str>,
}

impl Entry {
    fn weight(&self) -> usize {
        self.result.as_ref().map_or(0, |view| view.weight)
    }
}

#[derive(Default)]
struct Cache {
    entries: VecDeque<Entry>,
    bytes: usize,
    stopped: bool,
}

impl Cache {
    fn insert(&mut self, entry: Entry) {
        self.expire();
        if self.stopped
            || self
                .entries
                .iter()
                .any(|old| old.id == entry.id && old.generation == entry.generation)
        {
            return;
        }
        let weight = entry.weight();
        while self.bytes + weight > MAX_CACHED_BYTES || self.entries.len() >= MAX_CACHED_VIEWS {
            let Some(old) = self.entries.pop_front() else {
                return;
            };
            self.bytes -= old.weight();
        }
        self.bytes += weight;
        self.entries.push_back(entry);
        metrics::gauge!("worldchain.receipt_proofs.retained_bytes").set(self.bytes as f64);
    }

    fn expire(&mut self) {
        for entry in &mut self.entries {
            if entry.at.elapsed() >= RETENTION {
                self.bytes -= entry.weight();
                entry.result = Err("expired");
            }
        }
    }

    fn get(&mut self, id: FlashblockEvidenceId, generation: u64) -> Result<Entry, &'static str> {
        self.expire();
        if self.stopped {
            return Err("unavailable");
        }
        let entry = self
            .entries
            .iter()
            .rev()
            .find(|entry| entry.id == id)
            .ok_or("notRetained")?;
        if entry.at.elapsed() >= RETENTION {
            return Err("expired");
        }
        if entry.generation != generation {
            return Err("replaced");
        }
        Ok(entry.clone())
    }
}

struct CaptureGuard(Arc<Mutex<Cache>>);

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        let mut cache = self.0.lock();
        *cache = Cache {
            stopped: true,
            ..Cache::default()
        };
        metrics::gauge!("worldchain.receipt_proofs.retained_bytes").set(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::Receipt;
    use alloy_eips::eip2718::Decodable2718;
    use alloy_rpc_types_engine::PayloadId;
    use op_alloy_consensus::{OpDepositReceipt, OpReceiptEnvelope};

    #[test]
    fn matches_op_deposit_nonce_rules() {
        let deposit = OpReceipt::Deposit(OpDepositReceipt {
            inner: Receipt {
                status: true.into(),
                cumulative_gas_used: 21_000,
                logs: vec![],
            },
            deposit_nonce: Some(7),
            deposit_receipt_version: None,
        });
        for (strip, expected) in [(true, None), (false, Some(7))] {
            let encoded = encode_receipt(&deposit, strip);
            let OpReceiptEnvelope::Deposit(decoded) =
                OpReceiptEnvelope::decode_2718_exact(&encoded).expect("OP receipt")
            else {
                panic!("expected deposit receipt");
            };
            assert_eq!(decoded.receipt.deposit_nonce, expected);
        }
    }

    #[test]
    fn cache_reports_expiry_and_generation_change_for_exact_ids() {
        let id = FlashblockEvidenceId {
            payload_id: PayloadId::default(),
            index: 0,
            block_hash: B256::ZERO,
        };
        let mut cache = Cache::default();
        cache.insert(Entry {
            id,
            generation: 1,
            at: Instant::now() - RETENTION,
            result: Err("limitExceeded"),
        });
        assert!(matches!(cache.get(id, 1), Err("expired")));
        let next = FlashblockEvidenceId { index: 1, ..id };
        cache.insert(Entry {
            id: next,
            generation: 1,
            at: Instant::now(),
            result: Err("limitExceeded"),
        });
        assert!(matches!(cache.get(next, 2), Err("replaced")));
        assert!(matches!(
            cache.get(next, 1).expect("matching entry").result,
            Err("limitExceeded")
        ));
        let unknown = FlashblockEvidenceId {
            block_hash: B256::with_last_byte(1),
            ..next
        };
        assert!(matches!(cache.get(unknown, 1), Err("notRetained")));
    }
}
