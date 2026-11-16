//! Bounded PBH handoff from the existing blocking build worker to async I/O.
use crate::{Error, PbhSigner, Result};
use alloy_consensus::TxEip1559;
use alloy_primitives::{Address, Signature};
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use tokio::{
    runtime::{Handle, RuntimeFlavor},
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;

const CANCELLATION_POLL: Duration = Duration::from_millis(1);

/// Clones share one admission slot across all payload jobs using this wallet.
#[derive(Clone, Debug)]
pub struct PbhBridge {
    client: Arc<PbhSigner>,
    admission: Arc<Semaphore>,
}

impl PbhBridge {
    pub fn new(client: PbhSigner) -> Self {
        Self {
            client: Arc::new(client),
            admission: Arc::new(Semaphore::new(1)),
        }
    }
    pub fn address(&self) -> Address {
        self.client.address()
    }

    /// Call only from a build worker; never from the networking/async polling task.
    pub fn sign(
        &self,
        transaction: &TxEip1559,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<Signature> {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        if deadline <= Instant::now() {
            return Err(Error::Deadline);
        }
        let runtime = Handle::try_current()
            .map_err(|_| Error::Config("PBH bridge requires a running async runtime"))?;
        if runtime.runtime_flavor() != RuntimeFlavor::MultiThread {
            return Err(Error::Config(
                "PBH bridge requires the multithreaded build runtime",
            ));
        }
        let permit = self
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let client = self.client.clone();
        let transaction = transaction.clone();
        runtime.spawn(async move {
            let _permit = permit;
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(Error::Cancelled),
                result = client.sign(&transaction, tokio::time::Instant::from_std(deadline)) => result,
            };
            // Receiver disappearance means the owning build was cancelled or timed out.
            let _ = sender.send(result);
        });
        loop {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            let left = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Error::Deadline)?;
            match receiver.recv_timeout(left.min(CANCELLATION_POLL)) {
                Ok(result) => {
                    if cancelled() {
                        return Err(Error::Cancelled);
                    }
                    if Instant::now() >= deadline {
                        return Err(Error::Deadline);
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Error::Transport {
                        operation: "PBH bridge",
                        detail: "signing task ended without a result".into(),
                    });
                }
            }
        }
    }
}
