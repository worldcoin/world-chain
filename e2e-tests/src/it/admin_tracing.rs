//! End-to-end test for the `admin_tracingDirectives` RPC over a full node.
//!
//! Spins up a single World Chain node with the `admin` namespace enabled,
//! installs a reloadable tracing subscriber (mirroring what the CLI does when
//! the admin namespace is enabled — the in-process harness does not run the CLI
//! path), and exercises the endpoint over real HTTP: an ephemeral override is
//! applied, observed to take effect immediately, and observed to auto-revert to
//! the startup configuration once the TTL expires. Also asserts that invalid
//! input is rejected over the wire without mutating the live filter.

use std::time::Duration;

use jsonrpsee::{core::client::ClientT, http_client::HttpClientBuilder, rpc_params};
use reth_tracing::tracing_subscriber::{
    EnvFilter, fmt, layer::SubscriberExt, reload, util::SubscriberInitExt,
};
use tracing::level_filters::LevelFilter;
use world_chain_node::context::WorldChainDefaultContext;
use world_chain_test_utils::e2e_harness::setup::WorldChainTestBuilder;

#[tokio::test(flavor = "multi_thread")]
async fn test_admin_tracing_directives_apply_and_revert() -> eyre::Result<()> {
    // Install a reloadable tracing subscriber and register the reload handle,
    // exactly as `reth_tracing` does when the CLI enables reload. Each nextest
    // test runs in its own process, so this global subscriber is isolated.
    let (filter, handle) = reload::Layer::new(EnvFilter::new("info"));
    let _ = reth_tracing::tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .try_init();
    reth_tracing::install_log_handle(handle);
    world_chain_primitives::tracing::set_startup_tracing_directives("info".to_string());

    assert!(
        reth_tracing::log_handle_available(),
        "reload handle should be installed"
    );
    assert_eq!(LevelFilter::current(), LevelFilter::INFO);

    // Spin up a single full node with the `admin` RPC namespace enabled.
    let (_, nodes, _tasks, _env, _spammer) = WorldChainTestBuilder::builder()
        .nodes(1)
        .flashblocks(false)
        .admin_rpc(true)
        .build()
        .setup::<WorldChainDefaultContext>()
        .await?;

    let rpc_url = nodes[0].node.rpc_url();
    let client = HttpClientBuilder::default().build(rpc_url.as_str())?;

    // Apply an ephemeral override raising the global level to TRACE for 2s.
    let resp: serde_json::Value = client
        .request(
            "admin_tracingDirectives",
            rpc_params![serde_json::json!({ "directives": "trace", "ttlSecs": 2 })],
        )
        .await?;
    assert_eq!(resp["applied"], "trace");
    assert_eq!(resp["ttlSecs"], 2);
    assert_eq!(resp["revertsTo"], "info");

    // The override takes effect immediately.
    assert_eq!(LevelFilter::current(), LevelFilter::TRACE);

    // After the TTL elapses the filter reverts to the startup configuration.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(LevelFilter::current(), LevelFilter::INFO);

    // Invalid input is rejected over the wire before any state change.
    let err = client
        .request::<serde_json::Value, _>(
            "admin_tracingDirectives",
            rpc_params![serde_json::json!({ "directives": "debug", "ttlSecs": 0 })],
        )
        .await
        .expect_err("ttlSecs=0 must be rejected");
    assert!(
        err.to_string().contains("ttlSecs"),
        "expected ttlSecs validation error, got: {err}"
    );

    // The rejected call left the live filter untouched.
    assert_eq!(LevelFilter::current(), LevelFilter::INFO);

    Ok(())
}
