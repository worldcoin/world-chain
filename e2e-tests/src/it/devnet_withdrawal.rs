use alloy_primitives::U256;
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::{OptionExt, ensure};
use world_chain_devnet::SUPERCHAIN_GUARDIAN_PRIVATE_KEY;
use world_chain_proof_protocol::MULTI_PROOF_GAME_TYPE;

use crate::it::utils::{
    devnet::{
        GAME_DEFENDER_WINS, advance_to_timestamp, anchor_at, game_at, l1_contract, l1_rpc_url,
        latest_timestamp, signing_provider, try_build_ha_devnet, vault_at,
        wait_for_anchor_at_or_beyond, wait_for_game_finality, wait_for_game_settlement,
        wait_for_multi_proof_game, wait_for_proof_lane, wait_for_status,
    },
    withdrawals::{OptimismPortal, build_withdrawal_proof, initiate_withdrawal},
};

#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn op_native_wip_1006_portal_withdrawal_and_bond_settlement() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("Portal withdrawal E2E").await? else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let portal_address = l1_contract(devnet.optimism_portal(), "OptimismPortal")?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;
    let anchor_address = l1_contract(devnet.anchor_state_registry(), "AnchorStateRegistry")?;

    // Prefunded on both chains, so one key covers the L2 initiation and the L1 prove/finalize.
    let signer: PrivateKeySigner = SUPERCHAIN_GUARDIAN_PRIVATE_KEY.parse()?;
    let withdrawal_sender = signer.address();
    let l1_provider = signing_provider(l1_rpc, signer.clone())?;
    let l2_provider = signing_provider(&devnet.l2_rpc_url(), signer)?;

    let portal = OptimismPortal::new(portal_address, l1_provider.clone());
    let anchor = anchor_at(anchor_address, l1_provider.clone());
    ensure!(
        portal.version().call().await? == "5.6.1",
        "devnet Portal version does not match the pinned compatibility target"
    );
    ensure!(
        portal.anchorStateRegistry().call().await? == anchor_address,
        "Portal is not wired to the deployed AnchorStateRegistry"
    );
    ensure!(
        portal.disputeGameFactory().call().await? == factory_address,
        "Portal is not wired to the deployed DisputeGameFactory"
    );
    ensure!(
        anchor.respectedGameType().call().await? == MULTI_PROOF_GAME_TYPE,
        "WIP-1006 is not the AnchorStateRegistry's respected game type"
    );

    let withdrawal = initiate_withdrawal(l2_provider.clone(), withdrawal_sender).await?;
    let (game_index, game_address, game_l2_block) =
        wait_for_multi_proof_game(l1_provider.clone(), factory_address, withdrawal.l2_block)
            .await?;
    let game = game_at(game_address, l1_provider.clone());
    ensure!(
        game.wasRespectedGameTypeWhenCreated().call().await?,
        "covering WIP-1006 game was not respected when created"
    );
    ensure!(
        anchor.isGameProper(game_address).call().await?,
        "covering WIP-1006 game is not proper"
    );
    let proposer = game.gameCreator().call().await?;
    let vault = vault_at(game.bondVault().call().await?, l1_provider.clone());
    let available_while_locked = vault.availableBalance(proposer).call().await?;
    let proposer_bond = game.proposerBond().call().await?;
    wait_for_proof_lane(&game).await?;

    let (output_root_proof, withdrawal_proof) =
        build_withdrawal_proof(l2_provider, game_l2_block, withdrawal.hash).await?;
    ensure!(
        portal
            .proveWithdrawalTransaction(
                withdrawal.transaction.clone(),
                U256::from(game_index),
                output_root_proof,
                withdrawal_proof,
            )
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "Portal withdrawal proof transaction reverted"
    );

    let current_timestamp = latest_timestamp(&l1_provider).await?;
    let challenge_deadline = game.challengeDeadline().call().await?;
    let proof_maturity_delay: u64 = portal
        .proofMaturityDelaySeconds()
        .call()
        .await?
        .try_into()?;
    advance_to_timestamp(
        &l1_provider,
        current_timestamp
            .saturating_add(proof_maturity_delay)
            .max(challenge_deadline)
            .saturating_add(1),
    )
    .await?;

    wait_for_status(&game, GAME_DEFENDER_WINS).await?;
    let finality_delay: u64 = portal
        .disputeGameFinalityDelaySeconds()
        .call()
        .await?
        .try_into()?;
    let resolved_at = game.resolvedAt().call().await?;
    advance_to_timestamp(
        &l1_provider,
        resolved_at.saturating_add(finality_delay).saturating_add(1),
    )
    .await?;
    wait_for_game_finality(&anchor, game_address).await?;
    ensure!(
        anchor.isGameClaimValid(game_address).call().await?,
        "resolved WIP-1006 game is not a valid Portal claim"
    );

    ensure!(
        portal
            .finalizeWithdrawalTransaction(withdrawal.transaction)
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "Portal withdrawal finalization reverted"
    );
    ensure!(
        portal.finalizedWithdrawals(withdrawal.hash).call().await?,
        "Portal did not persist the finalized withdrawal"
    );
    wait_for_anchor_at_or_beyond(&anchor, game_l2_block).await?;

    wait_for_game_settlement(&vault, game_address).await?;
    ensure!(
        game.normalModeCredit(proposer).call().await? == proposer_bond,
        "the unchallenged defender win did not return the complete proposer bond"
    );
    let expected_available = available_while_locked
        .checked_add(proposer_bond)
        .ok_or_eyre("settled proposer balance overflow")?;
    ensure!(
        vault.availableBalance(proposer).call().await? >= expected_available,
        "settlement did not restore the proposer bond to reusable vault balance"
    );

    Ok(())
}
