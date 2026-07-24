use std::{sync::Arc, time::Duration};

use tokio::time::MissedTickBehavior;
use tracing::warn;

use crate::ProverService;

/// Runs the periodic maintenance loop that terminalizes proof requests which
/// exhausted their worker attempts.
pub async fn run_status_poller(service: Arc<ProverService>, poll_interval: Duration) {
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        match service.mark_exhausted_proof_requests_failed().await {
            Ok(0) => {}
            Ok(failed) => {
                warn!(
                    failed,
                    "status poller marked exhausted proof requests failed"
                );
            }
            Err(error) => {
                warn!(%error, "status poller scan failed");
            }
        }
    }
}
