use alloy_primitives::{B256, U256};
use eyre::eyre::ensure;
use world_chain_proof_protocol::LineageProvider;
use world_chain_proposer::{Proposal, ProposerClient};

use crate::it::utils::devnet::{
    GAME_CHALLENGER_WINS, advance_to_timestamp, funded_throwaway_provider, game_at, l1_contract,
    l1_rpc_url, proof_system_client, try_build_ha_devnet, wait_for_challenge, wait_for_status,
};

/// End-to-end test of the fault path: a dishonest proposer posts a root that disagrees with
/// consensus, the real World Chain challenger (running in-process against the devnet, exactly
/// as it would in production) detects and challenges it, the real defender correctly declines
/// to defend the bad claim, and the game resolves `ChallengerWins` with the proposer's bond
/// forfeited.
///
/// This is the fault-injection counterpart to `devnet_withdrawal`'s happy path: that test proves
/// an honest root survives to finalization, this one proves a dishonest root does not survive
/// challenge. Mirrors how Base/Optimism validate their fault-proof dispute game — via op-e2e
/// style tests that submit an invalid claim and assert the challenger wins — but exercises World
/// Chain's own proposer/challenger/defender services rather than op-challenger.
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
    let (malicious_address, malicious_provider) = funded_throwaway_provider(l1_rpc).await?;
    let contracts = proof_system_client(malicious_provider.clone(), factory_address).await?;

    // Race the honest in-process World Chain proposer to the very first proposal window. The
    // honest proposer can't submit until at least `block_interval` L2 blocks are safe/finalized
    // (several seconds away at devnet block time), so submitting immediately after devnet startup
    // reliably wins the slot for the malicious claim instead.
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

    // The real World Chain defender must never submit a proof for a claim that doesn't match its
    // own consensus-derived root. This is the key invariant: an honest defender does not
    // "accidentally" rescue an invalid proposal.
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
        game.credit(malicious_address).call().await? == U256::ZERO,
        "the dishonest proposer's bond must not be creditable back to them"
    );

    println!("devnet challenge: bad root at game {game_address} correctly resolved ChallengerWins");

    Ok(())
}
