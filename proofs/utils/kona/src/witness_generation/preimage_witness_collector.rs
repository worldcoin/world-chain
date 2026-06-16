use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use kona_preimage::{
    CommsClient, HintWriterClient, PreimageKey, PreimageOracleClient,
    errors::{PreimageOracleError, PreimageOracleResult},
};
use kona_proof::FlushableCache;
use world_chain_proof_core::witness::preimage_store::PreimageStore;

#[derive(Clone, Debug)]
pub struct PreimageWitnessCollector<P: CommsClient + FlushableCache + Send + Sync + Clone> {
    pub preimage_oracle: Arc<P>,
    pub preimage_witness_store: Arc<Mutex<PreimageStore>>,
    /// DIAGNOSTIC: total preimage requests served. A small count (hundreds) means the bulk
    /// `debug_executePayload` prefetch is populating the KV; a huge count (tens of thousands)
    /// means the host is falling back to per-node on-demand fetching.
    pub request_count: Arc<AtomicU64>,
}

#[async_trait]
impl<P> PreimageOracleClient for PreimageWitnessCollector<P>
where
    P: CommsClient + FlushableCache + Send + Sync + Clone,
{
    async fn get(&self, key: PreimageKey) -> PreimageOracleResult<Vec<u8>> {
        self.tick();
        let value = self.preimage_oracle.get(key).await?;
        self.save(key, &value)?;
        Ok(value)
    }

    async fn get_exact(&self, key: PreimageKey, buf: &mut [u8]) -> PreimageOracleResult<()> {
        self.tick();
        self.preimage_oracle.get_exact(key, buf).await?;
        self.save(key, buf)?;
        Ok(())
    }
}

#[async_trait]
impl<P> HintWriterClient for PreimageWitnessCollector<P>
where
    P: CommsClient + FlushableCache + Send + Sync + Clone,
{
    async fn write(&self, hint: &str) -> PreimageOracleResult<()> {
        // DIAGNOSTIC: every preimage hint the client sends flows through here. Log the hint
        // *type* (first token) so we can see the fetch pattern — crucially, whether
        // `l2-payload-witness` (the bulk `debug_executePayload` prefetch) is emitted at all,
        // vs. a storm of per-node `l2-state-node` / `l2-account-proof` hints.
        let kind = hint.split_whitespace().next().unwrap_or("");
        tracing::info!(target: "witness", hint = kind, "client hint");
        self.preimage_oracle.write(hint).await
    }
}

impl<P> FlushableCache for PreimageWitnessCollector<P>
where
    P: CommsClient + FlushableCache + Send + Sync + Clone,
{
    fn flush(&self) {
        self.preimage_oracle.flush();
    }
}

impl<P> PreimageWitnessCollector<P>
where
    P: CommsClient + FlushableCache + Send + Sync + Clone,
{
    /// DIAGNOSTIC: count a preimage request and log every 1000 so the request volume/rate is
    /// visible even if witness collection is killed before completing.
    fn tick(&self) {
        let n = self.request_count.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 1000 == 0 {
            tracing::info!(target: "witness", requests = n, "preimage requests served");
        }
    }

    pub fn save(&self, key: PreimageKey, value: &[u8]) -> PreimageOracleResult<()> {
        let mut witness_store_lock = self.preimage_witness_store.lock().map_err(|_| {
            PreimageOracleError::Other("failed to acquire preimage witness store lock".to_string())
        })?;
        witness_store_lock.save_preimage(key, value.to_vec())
    }
}
