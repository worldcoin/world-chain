use std::time::{Duration, Instant};

use eyre::eyre::eyre;
use tokio::sync::watch;
use world_chain_chainspec::{WorldChainHardfork, WorldChainHardforks};
use world_chain_devnet::{
    DevnetComponentKind, DevnetComponentStatus, HaSequencerConfig, ObservabilityConfig,
    WorldDevnet, WorldDevnetBuilder, WorldDevnetPreset, ensure_dev_chain_id, is_docker_unavailable,
};
use world_chain_test_utils::DEV_CHAIN_ID;

#[tokio::test(flavor = "multi_thread")]
async fn direct_sequencer_devnet_smoke() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let ha_config = HaSequencerConfig::default()
        .with_sequencer_count(2)
        .with_observability(ObservabilityConfig::default());

    let mut devnet = match WorldDevnetBuilder::new()
        .preset(WorldDevnetPreset::HaSequencer)
        .ha_sequencer(ha_config)
        .block_time(Duration::from_secs(1))
        .build()
        .await
    {
        Ok(devnet) => devnet,
        Err(err) if is_docker_unavailable(&err) => {
            eprintln!("skipping devnet smoke because Docker is unavailable: {err:#}");
            return Ok(());
        }
        Err(err) => return Err(err),
    };

    print_devnet_summary(&devnet);
    assert!(devnet.l1_rpc_url().is_some());
    assert!(devnet.flashblocks_url().is_some());
    assert_component_running(&devnet, DevnetComponentKind::OpBatcher);
    assert_component_running(&devnet, DevnetComponentKind::OpProposer);
    ensure_dev_chain_id(devnet.chain_spec().as_ref())?;

    assert_eq!(devnet.chain_id().await?, DEV_CHAIN_ID);

    let before = devnet.block_number().await?;
    let safe_before = devnet.safe_block_number().await?;
    println!("devnet smoke: initial latest_l2={before} safe_l2={safe_before}");

    devnet.produce_blocks(3).await?;
    let after = devnet.block_number().await?;
    assert!(after >= before + 3, "expected at least three new blocks");

    let (safe_after, latest_after) =
        wait_for_safe_head_progress(&devnet, safe_before, Duration::from_secs(120)).await?;
    assert!(
        safe_after > safe_before,
        "expected safe head to progress from {safe_before}, got {safe_after}"
    );
    assert!(
        safe_after <= latest_after,
        "safe head {safe_after} should not exceed sampled latest block {latest_after}"
    );
    assert!(
        devnet.produced_blocks() >= 3,
        "expected devnet driver to observe at least three produced blocks"
    );

    let capabilities = devnet.supported_capabilities().await?;
    assert_eq!(capabilities, vec!["flashblocksv1".to_string()]);

    let chain_spec = devnet.chain_spec();
    assert!(chain_spec.is_jovian_active_at_timestamp(0));
    assert!(!chain_spec.is_tropo_active_at_timestamp(0));
    assert!(!devnet.hardforks().is_active(WorldChainHardfork::Tropo));

    Ok(())
}

fn assert_component_running(devnet: &WorldDevnet, kind: DevnetComponentKind) {
    assert!(
        devnet
            .components()
            .iter()
            .any(|component| component.kind == kind
                && component.status == DevnetComponentStatus::Running),
        "expected running component kind={}",
        kind.as_str()
    );
}

fn print_devnet_summary(devnet: &WorldDevnet) {
    println!("devnet smoke: l1_rpc_url={:?}", devnet.l1_rpc_url());
    println!("devnet smoke: l2_rpc_url={}", devnet.l2_rpc_url());
    println!(
        "devnet smoke: flashblocks_url={:?}",
        devnet.flashblocks_url()
    );
    println!("devnet smoke: components:");
    for component in devnet.components() {
        println!(
            "  {} kind={} status={}",
            component.id,
            component.kind.as_str(),
            component.status.as_str()
        );
    }
}

async fn wait_for_safe_head_progress(
    devnet: &WorldDevnet,
    safe_before: u64,
    timeout: Duration,
) -> eyre::Result<(u64, u64)> {
    let started = Instant::now();
    let mut last_safe = safe_before;

    loop {
        let latest = devnet.block_number().await?;
        let safe = devnet.safe_block_number().await?;
        println!("devnet smoke: latest_l2={latest} safe_l2={safe} target_safe>{safe_before}");

        if safe < last_safe {
            return Err(eyre!("safe head regressed from {last_safe} to {safe}"));
        }
        if safe > safe_before {
            return Ok((safe, latest));
        }
        if started.elapsed() >= timeout {
            return Err(eyre!(
                "timed out after {timeout:?} waiting for safe head to progress beyond {safe_before}; latest_l2={latest} safe_l2={safe}"
            ));
        }

        last_safe = safe;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn devnet_run_until_shutdown_uses_continuous_block_driver() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let devnet = WorldDevnetBuilder::new()
        .without_l1()
        .block_time(Duration::from_millis(250))
        .build()
        .await?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move { devnet.run_until_shutdown(shutdown_rx).await });

    tokio::time::sleep(Duration::from_millis(1_200)).await;
    shutdown_tx.send(true)?;
    handle.await??;

    Ok(())
}
