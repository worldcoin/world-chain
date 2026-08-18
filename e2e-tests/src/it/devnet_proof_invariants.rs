//! Devnet-level tests for World Chain proof-system invariants that mirror the *properties*
//! exercised by Optimism's `op-e2e/faultproofs` Cannon test suite, adapted to WIP-1006's
//! non-interactive, multi-lane-proof-or-timeout design (no bisection game, no Cannon FPVM, no
//! preimage oracle — so those specific mechanics don't have an analog here).
//!
//! - [`unproven_proposal_times_out_to_challenger_wins`] is the analog of Optimism's
//!   `TestInvalidateUnsafeProposal`/`TestInvalidateProposalForFutureBlock`: a proposal that never
//!   accumulates a valid proof cannot win merely by going unchallenged.
//! - [`invalid_parent_cascades_to_challenger_wins`] is untested anywhere else: a game built on an
//!   already-invalidated parent must resolve `ChallengerWins` via `INVALID_PARENT` regardless of
//!   its own proof/challenge state.
//! - [`valid_proposal_survives_adversarial_challenge`] is the analog of Optimism's
//!   `AttackWithCorrectTrace`/`DefendWithCorrectTrace`: a genuinely honest claim must resolve
//!   `DefenderWins` even when maliciously challenged, once the real defender escalates to the
//!   proof threshold.

use alloy_primitives::{Address, B256};
use eyre::eyre::ensure;
use world_chain_proof_protocol::{LineageProvider, ProofLane, encode_compact_proof};
use world_chain_proposer::{Proposal, ProposerClient};

use crate::it::utils::devnet::{
    GAME_CHALLENGER_WINS, GAME_DEFENDER_WINS, INVALIDATION_REASON_INVALID_PARENT,
    INVALIDATION_REASON_PROOF_TIMEOUT, advance_to_timestamp, funded_throwaway_provider, game_at,
    l1_contract, l1_rpc_url, proof_system_client, resolve_if_still_in_progress,
    try_build_ha_devnet, wait_for_challenge, wait_for_multi_proof_game, wait_for_proof_lane,
    wait_for_status_or_resolve,
};

/// A proposal that never accumulates a valid proof cannot win merely by going unchallenged.
///
/// Mirrors Optimism's `TestInvalidateUnsafeProposal`/`TestInvalidateProposalForFutureBlock`,
/// which need a special-cased honest-actor strategy to defeat a claim about unavailable data.
/// WIP-1006 doesn't need that: `MultiProofGame.resolve()` treats `Unchallenged` and `Challenged`
/// identically once the (challenge- or proof-) deadline passes with `proofBitmap == 0` — a
/// proofless proposal always loses, whether or not anyone bothered to formally challenge it. This
/// test submits a bad root and, regardless of whether the real challenger races in first, asserts
/// the deadline-driven `ChallengerWins`/`PROOF_TIMEOUT` outcome.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unproven_proposal_times_out_to_challenger_wins() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("proof-timeout E2E").await? else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let (_, provider) = funded_throwaway_provider(l1_rpc).await?;
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

    // Advance past `proofDeadline()` (always the later of the two deadlines: the contract
    // requires `proofPeriod > challengePeriod`) so `gameOver()` is true regardless of whether the
    // real challenger raced in and challenged this proposal before we got here.
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
/// Untested anywhere else in this repo. `MultiProofGame.resolve()` checks parent validity before
/// anything about the game's own claim (`pkg/contracts/src/dispute/MultiProofGame.sol:552-561`),
/// so a child of an invalid parent can never win even with a perfectly correct root claim.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_parent_cascades_to_challenger_wins() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("invalid-parent cascade E2E").await? else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    let (_, provider) = funded_throwaway_provider(l1_rpc).await?;
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

    // Child: must be submitted *before* the parent resolves. `initialize()`'s `_isValidParent`
    // check (MultiProofGame.sol:421-424) requires `parent.status() != CHALLENGER_WINS` at
    // creation time — it only rejects parents that are *already known* to be invalid, since a
    // still-in-progress parent might yet turn out valid. The INVALID_PARENT cascade this test
    // targets is enforced dynamically inside `resolve()`, not at creation time, so the ordering
    // here (child created while parent is still healthy-looking, then parent invalidated) is the
    // realistic case a chain would actually see.
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

    // The parent resolves ChallengerWins via the real challenger's resolution manager, or we fall
    // back to resolving it ourselves.
    wait_for_status_or_resolve(&parent_game, GAME_CHALLENGER_WINS).await?;

    // The child is never discovered by the real challenger as challengeable (its l2 block number
    // is far beyond the finalized head), so nothing else will resolve it — do it ourselves.
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
/// Mirrors Optimism's `AttackWithCorrectTrace`/`DefendWithCorrectTrace`: challenging a valid claim
/// should not be able to invalidate it.
///
/// This does *not* route through the in-process SP1 worker. That worker's `OnlineHostConfig`
/// unconditionally requires a real beacon-API endpoint (kona's `OnlineBlobProvider` calls
/// `/eth/v1/config/spec` on startup) even though this devnet's batcher runs
/// `--data-availability-type calldata` and never touches blobs — Anvil has no consensus-layer
/// component to serve that endpoint, so the worker panics before it can produce a validity proof.
/// Fixing that is a real gap in how the worker is wired up, but not one worth taking on just to
/// exercise this invariant.
///
/// Instead this test submits the validity-proof lane itself, directly on the game contract.
/// `MultiProofGame.submitProofLane()` builds `TransitionPublicValues` from the game's own on-chain
/// state (`pkg/contracts/src/dispute/MultiProofGame.sol`, `_transition()`); the submitted proof
/// bytes are opaque to the contract and are handed straight to the verifier. The devnet only ever
/// deploys `MockRootIdVerifier(acceptAny=true)` for every lane (`DeployProofMocks.s.sol`), so a
/// well-formed but otherwise arbitrary proof payload is accepted — the same trick
/// `DevnetNitroBackend::handle_claimed_job` (`crates/devnet/src/full_stack.rs`) already relies on
/// for the Nitro lane. The real defender still proactively supplies the TEE lane; this test
/// supplies the validity lane, so together they reach `PROOF_THRESHOLD` without needing real or
/// mock SP1 proving.
#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn valid_proposal_survives_adversarial_challenge() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("adversarial-challenge E2E").await? else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;

    // An adversary challenges the perfectly valid claim posted by the real, honest World Chain
    // proposer, just to grief.
    let (adversary_address, adversary_provider) = funded_throwaway_provider(l1_rpc).await?;
    let (_, honest_game_address, _) =
        wait_for_multi_proof_game(adversary_provider.clone(), factory_address, 0).await?;
    let game = game_at(honest_game_address, adversary_provider.clone());

    let challenger_bond = game.challengerBond().call().await?;
    let receipt = game
        .challenge()
        .value(challenger_bond)
        .send()
        .await?
        .get_receipt()
        .await?;
    ensure!(
        receipt.status(),
        "adversarial challenge() transaction reverted"
    );
    ensure!(
        game.challenger().call().await? != Address::ZERO,
        "challenge should have registered"
    );

    // Wait for the real defender to proactively land the TEE lane before we add the validity
    // lane ourselves. Since we never enable an SP1 worker in this test, the real defender will
    // never attempt to submit the validity lane itself, so there's no risk of racing it.
    wait_for_proof_lane(&game).await?;

    // Submit the validity-proof lane directly, bypassing SP1 proving entirely — see the doc
    // comment above for why this is a faithful devnet exercise of the invariant rather than a
    // shortcut around it.
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

    // With both lanes landed, `gameOver()` triggers immediately, and the real proposer's periodic
    // resolution pass will resolve it DefenderWins.
    wait_for_status_or_resolve(&game, GAME_DEFENDER_WINS).await?;

    ensure!(
        game.invalidationReason().call().await? == 0,
        "a defended honest claim must have no invalidation reason"
    );

    Ok(())
}
