//! Devnet-level tests for WIP-1006 proof-system invariants, mirroring the properties Optimism
//! exercises in `op-e2e/faultproofs`.
//!
//! `MultiProofGame.t.sol` already covers each property at the contract level, and asserts more
//! about bonds. These drive the same properties through the real deployed stack — real proposer,
//! challenger and defender services racing the test — instead of `vm.prank`/`vm.warp`.

use alloy_primitives::{Address, B256};
use eyre::eyre::ensure;
use world_chain_proof_protocol::{
    ConsensusProvider, LineageProvider, OptimismConsensusClient, ProofLane, encode_compact_proof,
};
use world_chain_proposer::{Proposal, ProposerClient};

use crate::it::utils::devnet::{
    GAME_CHALLENGER_WINS, GAME_DEFENDER_WINS, INVALIDATION_REASON_INVALID_PARENT,
    INVALIDATION_REASON_PROOF_TIMEOUT, advance_to_timestamp, funded_throwaway_provider, game_at,
    l1_contract, l1_rpc_url, l2_op_node_rpc_url, proof_system_client, resolve_if_still_in_progress,
    try_build_ha_devnet, wait_for_challenge, wait_for_multi_proof_game, wait_for_proof_lane,
    wait_for_status_or_resolve,
};

/// A proposal that never accumulates a valid proof cannot win merely by going unchallenged.
///
/// Optimism's `TestInvalidateUnsafeProposal`/`TestInvalidateProposalForFutureBlock` need a
/// special-cased honest-actor strategy here; WIP-1006 doesn't. `resolve()` treats `Unchallenged`
/// and `Challenged` identically once the deadline passes with `proofBitmap == 0`, so a proofless
/// proposal always loses. Submits a bad root and asserts the deadline-driven
/// `ChallengerWins`/`PROOF_TIMEOUT` outcome, whether or not the real challenger races in first.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unproven_proposal_with_invalid_root_claim_times_out_to_challenger_wins() -> eyre::Result<()>
{
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("proof-timeout with invalid root claim E2E").await?
    else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let (_, provider) = funded_throwaway_provider(l1_rpc, factory_address).await?;
    let contracts = proof_system_client(provider.clone(), factory_address).await?;

    let anchor = contracts.lineage_anchor().await?;
    let registered = contracts.registered_lineage_config();
    let proposal = Proposal {
        parent_ref: anchor.address,
        root_claim: B256::repeat_byte(0xba),
        l2_block_number: anchor
            .l2_block_number
            .saturating_add(registered.block_interval),
        attempt: 0,
    };
    let submission = contracts.submit_proposal(&proposal).await?;
    let game = game_at(submission.game_address, provider.clone());

    // `proofDeadline()` is always the later deadline (`proofPeriod > challengePeriod`), so
    // `gameOver()` holds whether or not the real challenger challenged this first.
    let proof_deadline = game.proofDeadline().call().await?;
    advance_to_timestamp(&provider, proof_deadline.saturating_add(1)).await?;

    resolve_if_still_in_progress(&game).await?;

    ensure!(
        game.proofBitmap().call().await? == 0,
        "no proof lane should ever land on a proposal nobody proved"
    );
    ensure!(
        game.status().call().await? == GAME_CHALLENGER_WINS,
        "unproven proposal must resolve ChallengerWins on deadline, whether or not it was formally challenged"
    );
    ensure!(
        game.invalidationReason().call().await? == INVALIDATION_REASON_PROOF_TIMEOUT,
        "unproven proposal must invalidate with PROOF_TIMEOUT"
    );

    Ok(())
}

/// A game that contains a valid root_claim but without any proof will still end up as `CHALLENGER_WINS`.
///
/// This test is the same as the previous one, but this game contains a valid root_claim, instead of the
/// invalid root_claim in the other test.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unproven_proposal_with_valid_root_claim_times_out_to_challenger_wins() -> eyre::Result<()>
{
    reth_tracing::init_test_tracing();

    let Some(mut devnet) = try_build_ha_devnet("proof-timeout with valid root claim E2E").await?
    else {
        return Ok(());
    };

    // A valid root sits on the selected lineage, so the live defender would TEE-prove it and
    // defeat the "nobody proved" premise. Stop it (and its workers) before submitting.
    devnet.stop_defender();

    let l2_op_node_rpc = l2_op_node_rpc_url(&devnet)?;
    let output_root_provider = OptimismConsensusClient::new(l2_op_node_rpc);
    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let (_, provider) = funded_throwaway_provider(l1_rpc, factory_address).await?;
    let contracts = proof_system_client(provider.clone(), factory_address).await?;

    let anchor = contracts.lineage_anchor().await?;
    let registered = contracts.registered_lineage_config();
    let l2_block_number = anchor
        .l2_block_number
        .saturating_add(registered.block_interval);
    let root_claim = output_root_provider
        .output_root_at_block(l2_block_number)
        .await?;
    let proposal = Proposal {
        parent_ref: anchor.address,
        root_claim,
        l2_block_number,
        attempt: 0,
    };
    let submission = contracts.submit_proposal(&proposal).await?;
    let game = game_at(submission.game_address, provider.clone());

    // `proofDeadline()` is always the later deadline (`proofPeriod > challengePeriod`), so
    // `gameOver()` holds whether or not the real challenger challenged this first.
    let proof_deadline = game.proofDeadline().call().await?;
    advance_to_timestamp(&provider, proof_deadline.saturating_add(1)).await?;

    resolve_if_still_in_progress(&game).await?;

    ensure!(
        game.proofBitmap().call().await? == 0,
        "no proof lane should ever land on a proposal nobody proved"
    );
    ensure!(
        game.status().call().await? == GAME_CHALLENGER_WINS,
        "unproven proposal must resolve ChallengerWins on deadline, whether or not it was formally challenged"
    );
    ensure!(
        game.invalidationReason().call().await? == INVALIDATION_REASON_PROOF_TIMEOUT,
        "unproven proposal must invalidate with PROOF_TIMEOUT"
    );

    Ok(())
}

/// A game built on an already-invalidated parent must resolve `ChallengerWins` via
/// `INVALID_PARENT`, regardless of its own proof or challenge state.
///
/// `resolve()` checks parent validity before anything about the game's own claim
/// (`MultiProofGame.sol:552-561`), so a child of an invalid parent can never win.
///
/// Forge covers this via `test_InvalidParent_CascadesAndRefundsChildBonds` (which also asserts bond
/// refunds) and `test_BlacklistedParent_CascadesBeforeParentResolution` (the blacklist route, not
/// exercised here). This adds the cascade from a real proposer-submitted lineage with the real
/// challenger contesting the parent.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_game_with_invalid_parent_cascades_to_challenger_wins() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("invalid game with invalid-parent cascade E2E").await?
    else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let (_, provider) = funded_throwaway_provider(l1_rpc, factory_address).await?;
    let contracts = proof_system_client(provider.clone(), factory_address).await?;

    let anchor = contracts.lineage_anchor().await?;
    let registered = contracts.registered_lineage_config();

    // Parent: a bad root that the real challenger will contest.
    let parent_l2_block_number = anchor
        .l2_block_number
        .saturating_add(registered.block_interval);
    let parent_proposal = Proposal {
        parent_ref: anchor.address,
        root_claim: B256::repeat_byte(0xba),
        l2_block_number: parent_l2_block_number,
        attempt: 0,
    };
    let parent_submission = contracts.submit_proposal(&parent_proposal).await?;
    let parent_game = game_at(parent_submission.game_address, provider.clone());

    // Child must be created *before* the parent resolves: `_isValidParent`
    // (MultiProofGame.sol:421-424) rejects parents already known invalid. The cascade under test is
    // enforced dynamically in `resolve()`, so this ordering is the realistic case.
    let child_proposal = Proposal {
        parent_ref: parent_submission.game_address,
        root_claim: B256::repeat_byte(0xcd),
        l2_block_number: parent_l2_block_number.saturating_add(registered.block_interval),
        attempt: 0,
    };
    let child_submission = contracts.submit_proposal(&child_proposal).await?;
    let child_game = game_at(child_submission.game_address, provider.clone());

    // Wait for the real World Chain challenger to contest the bad parent.
    wait_for_challenge(&parent_game).await?;

    // Skip past the parent's proof deadline so its challenge can resolve.
    let parent_proof_deadline = parent_game.proofDeadline().call().await?;
    advance_to_timestamp(&provider, parent_proof_deadline.saturating_add(1)).await?;

    // Resolved by the real challenger's resolution manager, or by us as a fallback.
    wait_for_status_or_resolve(&parent_game, GAME_CHALLENGER_WINS).await?;

    // Its l2 block is far beyond the finalized head, so the real challenger never picks it up.
    resolve_if_still_in_progress(&child_game).await?;

    ensure!(
        child_game.status().call().await? == GAME_CHALLENGER_WINS,
        "child of an invalidated parent must resolve ChallengerWins regardless of its own root claim"
    );
    ensure!(
        child_game.invalidationReason().call().await? == INVALIDATION_REASON_INVALID_PARENT,
        "child of an invalidated parent must invalidate with INVALID_PARENT, not PROOF_TIMEOUT"
    );

    Ok(())
}

/// A game that contains a valid root_claim but with an invalid parent will end up to `CHALLENGER_WINS`.
///
/// This test is the same as the previous one, but this child game contains a valid root_claim,
/// instead of the invalid root_claim in the other test.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn valid_game_with_invalid_parent_cascades_to_challenger_wins() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("valid game with invalid-parent cascade E2E").await?
    else {
        return Ok(());
    };

    let l2_op_node_rpc = l2_op_node_rpc_url(&devnet)?;
    let output_root_provider = OptimismConsensusClient::new(l2_op_node_rpc);
    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let (_, provider) = funded_throwaway_provider(l1_rpc, factory_address).await?;
    let contracts = proof_system_client(provider.clone(), factory_address).await?;

    let anchor = contracts.lineage_anchor().await?;
    let registered = contracts.registered_lineage_config();

    // Parent: a bad root that the real challenger will contest.
    let parent_l2_block_number = anchor
        .l2_block_number
        .saturating_add(registered.block_interval);
    let parent_proposal = Proposal {
        parent_ref: anchor.address,
        root_claim: B256::repeat_byte(0xba),
        l2_block_number: parent_l2_block_number,
        attempt: 0,
    };
    let parent_submission = contracts.submit_proposal(&parent_proposal).await?;
    let parent_game = game_at(parent_submission.game_address, provider.clone());

    // Child must be created *before* the parent resolves: `_isValidParent`
    // (MultiProofGame.sol:421-424) rejects parents already known invalid. The cascade under test is
    // enforced dynamically in `resolve()`, so this ordering is the realistic case.
    let l2_block_number = parent_l2_block_number.saturating_add(registered.block_interval);
    let root_claim = output_root_provider
        .output_root_at_block(l2_block_number)
        .await?;
    let child_proposal = Proposal {
        parent_ref: parent_submission.game_address,
        root_claim,
        l2_block_number,
        attempt: 0,
    };
    let child_submission = contracts.submit_proposal(&child_proposal).await?;
    let child_game = game_at(child_submission.game_address, provider.clone());

    // Wait for the real World Chain challenger to contest the bad parent.
    wait_for_challenge(&parent_game).await?;

    // Skip past the parent's proof deadline so its challenge can resolve.
    let parent_proof_deadline = parent_game.proofDeadline().call().await?;
    advance_to_timestamp(&provider, parent_proof_deadline.saturating_add(1)).await?;

    // Resolved by the real challenger's resolution manager, or by us as a fallback.
    wait_for_status_or_resolve(&parent_game, GAME_CHALLENGER_WINS).await?;

    // Its l2 block is far beyond the finalized head, so the real challenger never picks it up.
    resolve_if_still_in_progress(&child_game).await?;

    ensure!(
        child_game.status().call().await? == GAME_CHALLENGER_WINS,
        "child of an invalidated parent must resolve ChallengerWins regardless of its own root claim"
    );
    ensure!(
        child_game.invalidationReason().call().await? == INVALIDATION_REASON_INVALID_PARENT,
        "child of an invalidated parent must invalidate with INVALID_PARENT, not PROOF_TIMEOUT"
    );

    Ok(())
}

/// A genuinely honest claim must resolve `DefenderWins` even when maliciously challenged, once
/// the proof threshold is reached.
///
/// Mirrors Optimism's `AttackWithCorrectTrace`/`DefendWithCorrectTrace`.
///
/// Does *not* route through the in-process SP1 worker: its `OnlineHostConfig` unconditionally
/// requires a beacon-API endpoint (kona's `OnlineBlobProvider` calls `/eth/v1/config/spec`) even
/// though this devnet's batcher uses calldata DA and never touches blobs, and Anvil has no
/// consensus layer to serve it — so the worker panics before producing a validity proof. That's a
/// real wiring gap, just not one worth fixing to exercise this invariant.
///
/// Instead the validity lane is submitted directly. `submitProofLane()` builds
/// `TransitionPublicValues` from the game's own on-chain state and passes the proof bytes straight
/// to the verifier, and the devnet only deploys `MockRootIdVerifier(acceptAny=true)`
/// (`DeployProofMocks.s.sol`) — the same trick `DevnetNitroBackend::handle_claimed_job` uses for the
/// Nitro lane. The real defender still supplies the TEE lane, so the two reach `PROOF_THRESHOLD`.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn valid_proposal_survives_adversarial_challenge() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("adversarial-challenge E2E").await? else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    // An adversary griefs the honest proposer's perfectly valid claim.
    let (adversary_address, adversary_provider) =
        funded_throwaway_provider(l1_rpc, factory_address).await?;
    let (_, honest_game_address, _) =
        wait_for_multi_proof_game(adversary_provider.clone(), factory_address, 0).await?;
    let game = game_at(honest_game_address, adversary_provider.clone());

    let receipt = game.challenge().send().await?.get_receipt().await?;
    ensure!(
        receipt.status(),
        "adversarial challenge() transaction reverted"
    );
    ensure!(
        game.challenger().call().await? != Address::ZERO,
        "challenge should have registered"
    );

    // No SP1 worker runs here, so the defender only ever lands the TEE lane — no race with ours.
    wait_for_proof_lane(&game).await?;

    // Validity lane submitted directly, bypassing SP1 proving — see the doc comment above.
    let compact_proof = encode_compact_proof(ProofLane::ValidityProof, adversary_address, &[0x01]);
    let proof_receipt = game
        .submitProofLane(compact_proof)
        .send()
        .await?
        .get_receipt()
        .await?;
    ensure!(
        proof_receipt.status(),
        "direct validity-proof-lane submission reverted"
    );

    // Both lanes landed, so `gameOver()` holds and the proposer's resolution pass takes it.
    wait_for_status_or_resolve(&game, GAME_DEFENDER_WINS).await?;

    ensure!(
        game.invalidationReason().call().await? == 0,
        "a defended honest claim must have no invalidation reason"
    );

    Ok(())
}
