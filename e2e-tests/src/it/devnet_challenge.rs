use std::time::Duration;

use alloy_primitives::{Address, B256, U256, address, utils::parse_ether};
use alloy_provider::Provider;
use eyre::eyre::{ensure, eyre};
use world_chain_proof_protocol::{IDisputeGameFactory, LineageProvider};
use world_chain_proposer::{Proposal, ProposerClient};

use crate::it::utils::{
    bindings::IFaultDisputeGame::{GameStatus, IFaultDisputeGameInstance},
    devnet::{
        GAME_CHALLENGER_WINS, advance_to_timestamp, fund_address, funded_throwaway_provider,
        game_at, l1_contract, l1_rpc_url, proof_system_client, try_build_ha_devnet,
        try_build_ha_devnet_with_custom_block_time, vault_at, wait_for_challenge, wait_for_status,
    },
};

/// In-process World Chain challenger (`WORLD_CHALLENGER_PRIVATE_KEY` in
/// `crates/devnet/src/full_stack.rs`).
const WORLD_CHALLENGER_ADDRESS: Address = address!("0x743dAA55063C608894C125Cf8eC82Afe83B2d5c5");
/// Long enough for several challenger poll ticks after the game is L1-finalized.
const CHALLENGER_IDLE_OBSERVATION: Duration = Duration::from_secs(20);

/// End-to-end fault path: a dishonest proposer posts a root that disagrees with consensus, the
/// real challenger detects and challenges it, the real defender declines to defend it, and the
/// game resolves `ChallengerWins` with the proposer's bond forfeited.
///
/// The fault-injection counterpart to `devnet_withdrawal`'s happy path — that proves an honest root
/// reaches finalization, this proves a dishonest one doesn't. Same shape as Optimism's op-e2e
/// dispute-game tests, but exercising World Chain's own services rather than op-challenger.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_root_proposal_is_challenged_and_invalidated() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("bad-root challenge E2E").await? else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    // A throwaway signer standing in for a dishonest proposer.
    let (malicious_address, malicious_provider) =
        funded_throwaway_provider(l1_rpc, factory_address).await?;
    let contracts = proof_system_client(malicious_provider.clone(), factory_address).await?;

    // Wins the first proposal window: the honest proposer can't submit until `block_interval` L2
    // blocks are safe, several seconds out at devnet block time.
    let anchor = contracts.lineage_anchor().await?;
    let registered = contracts.registered_lineage_config();
    let bad_root = B256::repeat_byte(0xba);
    let bad_proposal = Proposal {
        parent_ref: anchor.address,
        root_claim: bad_root,
        l2_block_number: anchor
            .l2_block_number
            .saturating_add(registered.block_interval),
        attempt: 0,
    };

    let submission = contracts.submit_proposal(&bad_proposal).await?;
    let game_address = submission.game_address;
    println!(
        "devnet challenge: malicious proposer {malicious_address} posted bad root {bad_root} for l2 block {} at game {game_address}",
        bad_proposal.l2_block_number
    );

    let game = game_at(game_address, malicious_provider.clone());

    // The real World Chain challenger recomputes the output root from consensus and disagrees.
    let challenger = wait_for_challenge(&game).await?;
    println!("devnet challenge: challenged by {challenger}");

    // The key invariant: an honest defender never accidentally rescues an invalid proposal.
    ensure!(
        game.proofBitmap().call().await? == 0,
        "defender must not submit any proof lane for a bad root claim"
    );

    // Skip past the proof deadline so the challenge can resolve without waiting on real time.
    let proof_deadline = game.proofDeadline().call().await?;
    advance_to_timestamp(&malicious_provider, proof_deadline.saturating_add(1)).await?;

    wait_for_status(&game, GAME_CHALLENGER_WINS).await?;

    ensure!(
        game.proofBitmap().call().await? == 0,
        "no proof lane should ever land on an invalidated game"
    );
    ensure!(
        game.gameCreator().call().await? == malicious_address,
        "the proposal bond must remain attributed to the dishonest proposer"
    );
    let vault = vault_at(game.bondVault().call().await?, malicious_provider.clone());
    let game_bond = vault.gameBonds(game_address).call().await?;
    ensure!(
        game.challenger().call().await? == challenger,
        "the game did not attribute the challenge to the observed challenger"
    );
    ensure!(
        game_bond.proposerBond == game.proposerBond().call().await?,
        "the vault did not lock the game's complete proposer bond"
    );
    ensure!(
        game_bond.challengerBond == game.challengerBond().call().await?,
        "the vault did not lock the game's complete challenger bond"
    );

    println!("devnet challenge: bad root at game {game_address} correctly resolved ChallengerWins");

    Ok(())
}

/// The challenger completely ignores games that are already expired.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn already_expired_game_is_ignored() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet_with_custom_block_time(
        "unkown game type is ignored",
        Duration::from_millis(200), // 200ms block time
    )
    .await?
    else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let (address, provider) = funded_throwaway_provider(l1_rpc, factory_address).await?;
    let contracts = proof_system_client(provider.clone(), factory_address).await?;

    // Wins the first proposal window: the honest proposer can't submit until `block_interval` L2
    // blocks are safe, several seconds out at devnet block time.
    let anchor = contracts.lineage_anchor().await?;
    let registered = contracts.registered_lineage_config();
    let bad_root = B256::repeat_byte(0xba);
    let bad_proposal = Proposal {
        parent_ref: anchor.address,
        root_claim: bad_root,
        l2_block_number: anchor
            .l2_block_number
            .saturating_add(registered.block_interval),
        attempt: 0,
    };

    let submission = contracts.submit_proposal(&bad_proposal).await?;
    let game_address = submission.game_address;
    println!(
        "devnet challenge: malicious proposer {address} posted bad root {bad_root} for l2 block {} at game {game_address}",
        bad_proposal.l2_block_number
    );
    let game = game_at(game_address, provider.clone());
    let proof_deadline = game.proofDeadline().call().await?;
    advance_to_timestamp(&provider, proof_deadline.saturating_add(1)).await?;
    // Sleep for ~30 secs so that the block that contains that game becomes finalized.
    // Our challenger only looks at games contained in finalized blocks.
    tokio::time::sleep(Duration::from_secs(30)).await;
    let game_status = game.status().call().await?;
    // The challenger doesn't challenge this game even if it contains an invalid root_claim
    // because proof_deadline has already elapsed, therefore this game won't ever be considered
    // valid anymore.
    let expected_game_status = 0; // IN_PROGRESS
    assert_eq!(game_status, expected_game_status);
    Ok(())
}

/// The challenger completely ignores already challenged games.
///
/// Game state alone cannot prove this: a second `challenge()` reverts with
/// `ClaimAlreadyChallenged` and leaves `challenger()` unchanged. We instead
/// assert the world challenger's L1 nonce does not move after it has had time
/// to see the finalized game. A mined (even reverted) challenge would bump it.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn already_challenged_game_is_ignored() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet_with_custom_block_time(
        "already challenged game is ignored",
        Duration::from_millis(200), // 200ms block time
    )
    .await?
    else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let (address, provider) = funded_throwaway_provider(l1_rpc, factory_address).await?;
    let contracts = proof_system_client(provider.clone(), factory_address).await?;

    // Wins the first proposal window: the honest proposer can't submit until `block_interval` L2
    // blocks are safe, several seconds out at devnet block time.
    let anchor = contracts.lineage_anchor().await?;
    let registered = contracts.registered_lineage_config();
    let bad_root = B256::repeat_byte(0xba);
    let bad_proposal = Proposal {
        parent_ref: anchor.address,
        root_claim: bad_root,
        l2_block_number: anchor
            .l2_block_number
            .saturating_add(registered.block_interval),
        attempt: 0,
    };

    let submission = contracts.submit_proposal(&bad_proposal).await?;
    let game_address = submission.game_address;
    println!(
        "devnet challenge: malicious proposer {address} posted bad root {bad_root} for l2 block {} at game {game_address}",
        bad_proposal.l2_block_number
    );
    let game = game_at(game_address, provider.clone());
    // Challenge before L1 finalization so the real challenger first sees an already-challenged game.
    let receipt = game.challenge().send().await?.get_receipt().await?;
    assert!(receipt.status());
    assert_eq!(game.challenger().call().await?, address);

    // Sleep for ~30 secs so that the block that contains that game becomes finalized.
    // Our challenger only looks at games contained in finalized blocks.
    tokio::time::sleep(Duration::from_secs(30)).await;

    let nonce_before = provider
        .get_transaction_count(WORLD_CHALLENGER_ADDRESS)
        .await?;
    tokio::time::sleep(CHALLENGER_IDLE_OBSERVATION).await;
    let nonce_after = provider
        .get_transaction_count(WORLD_CHALLENGER_ADDRESS)
        .await?;

    assert_eq!(nonce_after, nonce_before,);
    assert_eq!(game.challenger().call().await?, address);
    Ok(())
}

/// The challenger completely ignores games that are not WIP1006 type.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_game_type_is_ignored() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();
    // The type must be already set in the `DisputeGameFactory` contract, otherwise the creation
    // of a game fails with `NoImplementation` error.
    const PERMISSIONED_CANNON: u32 = 1;
    const OP_PROPOSER_PRIVATE_KEY: &str =
        "0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97";

    let Some(devnet) = try_build_ha_devnet_with_custom_block_time(
        "unkown game type is ignored",
        Duration::from_millis(200), // 200ms block time
    )
    .await?
    else {
        return Ok(());
    };
    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let provider = fund_address(l1_rpc, OP_PROPOSER_PRIVATE_KEY).await?;
    let factory =
        IDisputeGameFactory::IDisputeGameFactoryInstance::new(factory_address, provider.clone());

    let dummy_root_claim = B256::ZERO;
    // Extra data must be 32 bytes long in the permissioned cannon game.
    // It represents the l2 block number.
    let dummy_extra_data = U256::from(1).to_be_bytes::<32>().into();
    // Set 0.08 ether as the bond value for the game creation
    let init_bond = parse_ether("0.08")?;
    let pending_create_tx = factory
        .create(PERMISSIONED_CANNON, dummy_root_claim, dummy_extra_data)
        .value(init_bond)
        .send()
        .await?;
    let receipt = pending_create_tx.get_receipt().await?;
    assert!(receipt.status());
    let game_address = receipt
        .logs()
        .iter()
        .filter(|log| log.address() == factory_address)
        .find_map(|log| {
            log.log_decode_validate::<IDisputeGameFactory::DisputeGameCreated>()
                .ok()
                .map(|decoded| decoded.inner.data)
        })
        .filter(|event| {
            event.gameType == PERMISSIONED_CANNON && event.rootClaim == dummy_root_claim
        })
        .map(|event| event.disputeProxy)
        .ok_or_else(|| eyre!("no game created"))?;
    // Sleep for ~30 secs so that the block that contains that game becomes finalized.
    // Our challenger only looks at games contained in finalized blocks.
    tokio::time::sleep(Duration::from_secs(30)).await;
    // Ensure that the game is still `IN_PROGRESS` because our challenger isn't triggered
    // by an unknown game type that differs from WIP1006.
    let fault_dispute_game = IFaultDisputeGameInstance::new(game_address, provider);
    let status = fault_dispute_game.status().call().await?;
    assert_eq!(status, GameStatus::IN_PROGRESS);
    Ok(())
}
