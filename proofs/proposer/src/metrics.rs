//! Metrics definitions and helpers for the World Chain proposer.

use telemetry_batteries::reexports::metrics;

/// Count of proof-system proposals successfully confirmed on L1.
pub const METRICS_PROPOSALS_SUBMITTED: &str = "proposals.submitted";
/// Registers proposer metric metadata with the active recorder.
pub fn describe_metrics() {
    world_chain_proof_metrics::describe_metrics();
    metrics::describe_counter!(
        METRICS_PROPOSALS_SUBMITTED,
        metrics::Unit::Count,
        "Number of World Chain proof-system proposals successfully confirmed on L1."
    );
}

/// Records a successfully confirmed proof-system proposal.
pub fn increment_proposals_submitted() {
    metrics::counter!(METRICS_PROPOSALS_SUBMITTED).increment(1);
}
