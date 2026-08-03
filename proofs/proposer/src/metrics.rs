//! Metrics definitions and helpers for the World Chain proposer.

use telemetry_batteries::reexports::metrics;

/// Count of proof-system proposals successfully confirmed on L1.
pub const METRICS_PROPOSALS_SUBMITTED: &str = "proposals.submitted";
/// Highest L2 block covered by the currently selected proposal lineage.
pub const METRICS_SELECTED_LINEAGE_L2_BLOCK_NUMBER: &str =
    "proposer.selected_lineage_l2_block_number";
/// Registers proposer metric metadata with the active recorder.
pub fn describe_metrics() {
    world_chain_proof_metrics::describe_metrics();
    metrics::describe_counter!(
        METRICS_PROPOSALS_SUBMITTED,
        metrics::Unit::Count,
        "Number of World Chain proof-system proposals successfully confirmed on L1 by kind."
    );
    metrics::describe_gauge!(
        METRICS_SELECTED_LINEAGE_L2_BLOCK_NUMBER,
        metrics::Unit::Count,
        "Highest L2 block covered by the currently selected proposal lineage."
    );
}

/// Records a successfully confirmed proof-system proposal.
pub fn increment_proposals_submitted(kind: &'static str) {
    metrics::counter!(METRICS_PROPOSALS_SUBMITTED, "kind" => kind).increment(1);
}

/// Updates the highest L2 block covered by the selected proposal lineage.
pub fn set_selected_lineage_l2_block_number(block_number: u64) {
    metrics::gauge!(METRICS_SELECTED_LINEAGE_L2_BLOCK_NUMBER).set(block_number as f64);
}
