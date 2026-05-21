use std::time::Duration;

use tokio::sync::watch;
use world_chain_chainspec::{WorldChainHardfork, WorldChainHardforks};
use world_chain_devnet::{
    WorldDevnetBuilder, WorldDevnetPreset, ensure_dev_chain_id, is_docker_unavailable,
};
use world_chain_test_utils::DEV_CHAIN_ID;

#[tokio::test(flavor = "multi_thread")]
async fn direct_sequencer_devnet_smoke() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let mut devnet = match WorldDevnetBuilder::new()
        .preset(WorldDevnetPreset::DirectSequencer)
        .block_time(Duration::from_millis(250))
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

    assert!(devnet.l1_rpc_url().is_some());
    assert!(devnet.flashblocks_url().is_some());
    ensure_dev_chain_id(devnet.chain_spec().as_ref())?;

    assert_eq!(devnet.chain_id().await?, DEV_CHAIN_ID);

    let before = devnet.block_number().await?;
    devnet.produce_blocks(2).await?;
    let after = devnet.block_number().await?;
    assert!(after >= before + 2, "expected at least two new blocks");
    assert_eq!(devnet.produced_blocks(), 2);

    let capabilities = devnet.supported_capabilities().await?;
    assert_eq!(capabilities, vec!["flashblocksv1".to_string()]);

    let chain_spec = devnet.chain_spec();
    assert!(chain_spec.is_jovian_active_at_timestamp(0));
    assert!(!chain_spec.is_tropo_active_at_timestamp(0));
    assert!(!devnet.hardforks().is_active(WorldChainHardfork::Tropo));

    Ok(())
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
