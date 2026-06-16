//! End-to-end test: the defender finalizes a challenged-but-valid game with a real SP1
//! Groth16 validity proof.
//!
//! This exercises the full loop with **no mocks** on the proving path:
//! proposer posts a game → a griefer challenges its (valid) root → the defender requests an
//! SP1 proof from the prover-service → the in-process SP1 worker generates a real Groth16
//! aggregation proof → the defender submits it via `submitProofLane` → the on-chain
//! `SP1ValidityVerifier` verifies it → with `proofThreshold = 1` the game finalizes.
//!
//! ## Running
//!
//! Heavy and slow — gated behind `#[ignore]` and an explicit opt-in env var:
//!
//! ```bash
//! DEVNET_SP1_WORKER_PROVER=cpu \
//!   cargo test -p world-chain-tests --test '*' \
//!   -- --ignored defender_finalizes_challenged_game_with_sp1_proof --nocapture
//! ```
//!
//! Requirements: Docker (for the L1/op-stack containers), the SP1 toolchain, and a host with
//! ~32–128 GB RAM for local CPU Groth16 proving (use `DEVNET_SP1_WORKER_PROVER=network` with
//! `SP1_PRIVATE_KEY` set to offload proving instead).

use std::time::{Duration, Instant};

use alloy_network::EthereumWallet;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::sol;
use eyre::eyre::eyre;
use tracing::info;
use url::Url;
use world_chain_devnet::{
    HaSequencerConfig, WorldDevnetBuilder, WorldDevnetPreset, is_docker_unavailable,
};
use world_chain_proofs::{IWorldChainProofSystemFactory, IWorldChainProofSystemGame};

sol! {
    // The devnet staking registry is a mock with an open `setStaked`; bind just that.
    #[sol(rpc)]
    interface IMockStakingRegistry {
        function setStaked(address account, bool staked) external;
    }
}

/// `WorldChainProofSystemGame.challenge` bond — `CHALLENGER_BOND` (0.1 ether) in the deploy.
const CHALLENGER_BOND_WEI: u128 = 100_000_000_000_000_000;
/// `RootState` discriminants from `WorldChainProofLib`.
const ROOT_STATE_PROPOSED: u8 = 1;
const ROOT_STATE_CHALLENGED: u8 = 2;
const ROOT_STATE_FINALIZED: u8 = 3;

#[ignore = "real SP1 Groth16 CPU proving: needs the SP1 toolchain + ~32-128GB RAM; run manually"]
#[tokio::test(flavor = "multi_thread")]
async fn defender_finalizes_challenged_game_with_sp1_proof() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    // Enabling the SP1 worker also switches the proof system to the real Groth16 verifier with a
    // single-lane threshold. Opt in explicitly; without it the proving stack is not started.
    if std::env::var("DEVNET_SP1_WORKER_PROVER").is_err() {
        eprintln!("skipping defender e2e: set DEVNET_SP1_WORKER_PROVER=cpu (or network) to run it");
        return Ok(());
    }

    let devnet = match WorldDevnetBuilder::new()
        .preset(WorldDevnetPreset::HaSequencer)
        .ha_sequencer(HaSequencerConfig::default().with_sequencer_count(3))
        .block_time(Duration::from_secs(1))
        .build()
        .await
    {
        Ok(devnet) => devnet,
        Err(err) if is_docker_unavailable(&err) => {
            eprintln!("skipping defender e2e because Docker is unavailable: {err:#}");
            return Ok(());
        }
        Err(err) => return Err(err),
    };

    let proof_system = devnet.proof_system().ok_or_else(|| {
        eyre!("proof system was not deployed; enable world_contracts.proof_system")
    })?;
    let l1_rpc_url = devnet
        .l1_rpc_url()
        .ok_or_else(|| eyre!("devnet has no L1 RPC URL"))?;
    let griefer_key = devnet
        .e2e_griefer_key()
        .ok_or_else(|| eyre!("devnet did not expose an e2e griefer key"))?;

    info!(
        l1_rpc_url,
        factory = %proof_system.factory,
        staking_registry = %proof_system.staking_registry,
        validity_verifier = %proof_system.validity_proof_verifier,
        block_interval = proof_system.block_interval,
        "devnet up with SP1 proof system; starting defender e2e"
    );

    // L1 provider signing as the funded griefer account.
    let signer: PrivateKeySigner = griefer_key.parse()?;
    let griefer = signer.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(Url::parse(l1_rpc_url)?);

    // Stake the griefer so the game contract accepts its challenge (mock registry: open).
    info!(%griefer, "staking griefer account");
    let staking = IMockStakingRegistry::new(proof_system.staking_registry, provider.clone());
    staking
        .setStaked(griefer, true)
        .send()
        .await?
        .get_receipt()
        .await?;

    // Wait for the proposer to post a game, then challenge its (valid) root ourselves — the
    // honest challenger only challenges invalid roots, so a valid game would otherwise never
    // enter the CHALLENGED state the defender reacts to.
    let factory = IWorldChainProofSystemFactory::new(proof_system.factory, provider.clone());
    info!("waiting for the proposer to post a game");
    let game_addr = wait_for_proposed_game(&provider, &factory, Duration::from_secs(180)).await?;
    let game = IWorldChainProofSystemGame::new(game_addr, provider.clone());

    info!(game = %game_addr, "challenging the game's (valid) root to trigger a defense");
    let receipt = game
        .challenge()
        .value(U256::from(CHALLENGER_BOND_WEI))
        .send()
        .await?
        .get_receipt()
        .await?;
    assert_eq!(
        game.state().call().await?,
        ROOT_STATE_CHALLENGED,
        "griefer challenge should move the game to CHALLENGED"
    );
    info!(
        game = %game_addr,
        tx = %receipt.transaction_hash,
        "game is CHALLENGED; waiting for the defender to prove and submit (real Groth16, minutes)"
    );

    // The defender now requests an SP1 proof; the worker generates a real Groth16 aggregation
    // proof (minutes on CPU) and the defender submits it. With threshold 1 that finalizes.
    let started = Instant::now();
    let finalized = wait_for_state(
        &provider,
        game_addr,
        ROOT_STATE_FINALIZED,
        Duration::from_secs(3600),
    )
    .await?;
    assert!(
        finalized,
        "defender did not finalize the challenged game within the timeout"
    );
    let proof_count = game.proofCount().call().await?;
    info!(
        game = %game_addr,
        elapsed_secs = started.elapsed().as_secs(),
        proof_count,
        "game FINALIZED by the defender"
    );
    assert_eq!(
        proof_count, 1,
        "expected exactly one proven lane (the SP1 validity proof)"
    );

    Ok(())
}

/// Polls `GameCreated` logs until a game in the `PROPOSED` state appears, returning the address
/// of the one with the **highest** L2 block number.
///
/// Picking the newest proposal (nearest the safe head) rather than the first keeps the
/// `tip - target` gap small at proving time. The witness host builds the range via
/// `debug_executionWitness`, which forces reth's `HistoricalStateProvider` to reconstruct
/// historical state by unwinding changesets from the chain tip back to the target block —
/// cost O(tip - target). Targeting an old game (e.g. the first, near genesis) makes that
/// unwind grow without bound as the devnet keeps minting blocks, so witness collection never
/// outpaces block production. A near-head target keeps every unwind cheap.
async fn wait_for_proposed_game<P: Provider>(
    provider: &P,
    factory: &IWorldChainProofSystemFactory::IWorldChainProofSystemFactoryInstance<P>,
    timeout: Duration,
) -> eyre::Result<Address> {
    let deadline = Instant::now() + timeout;
    loop {
        let logs = factory
            .GameCreated_filter()
            .from_block(0u64)
            .query()
            .await?;
        let games_seen = logs.len();

        let mut newest: Option<(u64, Address)> = None;
        for (event, _log) in logs {
            let game = IWorldChainProofSystemGame::new(event.game, provider);
            if game.state().call().await? == ROOT_STATE_PROPOSED {
                let l2_block = event.l2BlockNumber.to::<u64>();
                if newest.is_none_or(|(best, _)| l2_block > best) {
                    newest = Some((l2_block, event.game));
                }
            }
        }

        if let Some((l2_block, game)) = newest {
            info!(%game, l2_block, "challenging the newest PROPOSED game (near safe head)");
            return Ok(game);
        }

        info!(games_seen, "no PROPOSED game yet; polling");
        if Instant::now() >= deadline {
            return Err(eyre!(
                "no PROPOSED game appeared within {timeout:?}; is the proposer running?"
            ));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Polls a game's `state()` until it equals `target` or the timeout elapses.
async fn wait_for_state<P: Provider>(
    provider: &P,
    game_addr: Address,
    target: u8,
    timeout: Duration,
) -> eyre::Result<bool> {
    let game = IWorldChainProofSystemGame::new(game_addr, provider);
    let started = Instant::now();
    let deadline = started + timeout;
    loop {
        let state = game.state().call().await?;
        if state == target {
            return Ok(true);
        }
        let bitmap = game.proofBitmap().call().await.unwrap_or(0);
        info!(
            game = %game_addr,
            state,
            target,
            proof_bitmap = bitmap,
            elapsed_secs = started.elapsed().as_secs(),
            "waiting for game state"
        );
        if Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}
