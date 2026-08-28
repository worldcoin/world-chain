use alloy_primitives::B256;
use eyre::eyre::ensure;
use world_chain_proof_protocol::LineageProvider;
use world_chain_proposer::{Proposal, ProposerClient};

use crate::it::utils::devnet::{
    GAME_CHALLENGER_WINS, advance_to_timestamp, funded_throwaway_provider, game_at, l1_contract,
    l1_rpc_url, proof_system_client, try_build_ha_devnet, vault_at, wait_for_challenge,
    wait_for_status,
};

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
