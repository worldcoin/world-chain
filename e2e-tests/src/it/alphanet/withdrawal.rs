use crate::it::utils::{
    devnet::wait_for_multi_proof_game,
    withdrawals::{
        InitiatedWithdrawal, OptimismPortal::OptimismPortalInstance, ProveWithdrawal,
        build_withdrawal_proof, initiate_withdrawal,
    },
};
use alloy_network::EthereumWallet;
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use revm_primitives::{Address, U256};
use std::str::FromStr;

#[tokio::test]
#[ignore]
async fn init_withdrawal() {
    // fetch env vars
    let l2_private_key_str = std::env::var("L2_PRIVATE_KEY").unwrap();
    let l2_rpc_endpoint = std::env::var("L2_RPC_ENDPOINT").unwrap();
    // create L2 signer provider
    let local_signer = PrivateKeySigner::from_str(&l2_private_key_str).unwrap();
    let local_signer_addr = local_signer.address();
    let l2_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&l2_rpc_endpoint)
        .await
        .unwrap();
    // target address of the L2 -> L1 withdrawal is the same address that sends the tx on L2
    let target_addr = local_signer_addr;
    // sends the initiate_withdrawal transaction to the L2ToL1MessagePasser contract
    let initiate_withdrawal = initiate_withdrawal(l2_provider, target_addr).await.unwrap();
    // save the InitiatedWithdrawal data to a .json file
    std::fs::write(
        "initiate_withdrawal.json",
        serde_json::to_string_pretty(&initiate_withdrawal).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
#[ignore]
async fn prove_withdrawal() {
    // fetch env vars
    let l1_private_key_str = std::env::var("L1_PRIVATE_KEY").unwrap();
    let l1_rpc_endpoint = std::env::var("L1_RPC_ENDPOINT").unwrap();
    let dispute_game_factory_str = std::env::var("DISPUTE_GAME_FACTORY").unwrap();
    let dispute_game_facatory_addr = Address::from_str(&dispute_game_factory_str).unwrap();
    let l2_rpc_endpoint = std::env::var("L2_RPC_ENDPOINT").unwrap();
    let optimism_portal_str = std::env::var("OPTIMISM_PORTAL").unwrap();
    let optimism_portal_addr = Address::from_str(&optimism_portal_str).unwrap();
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&l1_private_key_str).unwrap();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&l1_rpc_endpoint)
        .await
        .unwrap();
    // create L2 provider
    let l2_provider = ProviderBuilder::new()
        .connect(&l2_rpc_endpoint)
        .await
        .unwrap();
    // fetch InitiatedWithdrawl data from .json file
    let initiated_withdrawal: InitiatedWithdrawal =
        serde_json::from_slice(&std::fs::read("initiated_withdrawal.json").unwrap()).unwrap();
    // wait for a covering WIP1006 game with l2SequenceNumber >= initiated_withdrawal.l2_block
    let (game_index, game_addr, game_l2_block) = wait_for_multi_proof_game(
        &l1_provider,
        dispute_game_facatory_addr,
        initiated_withdrawal.l2_block,
    )
    .await
    .unwrap();
    // get the output root proof and the withdrawal proof
    let (output_root_proof, withdrawal_proof) =
        build_withdrawal_proof(l2_provider, game_l2_block, initiated_withdrawal.hash)
            .await
            .unwrap();
    // send the OptimismPortal::proveWithdrawalTransaction
    let optimism_portal = OptimismPortalInstance::new(optimism_portal_addr, &l1_provider);
    let pending_tx = optimism_portal
        .proveWithdrawalTransaction(
            initiated_withdrawal.transaction.clone(),
            U256::from(game_index),
            output_root_proof,
            withdrawal_proof,
        )
        .send()
        .await
        .unwrap();
    let receipt = pending_tx.get_receipt().await.unwrap();
    assert!(receipt.status());
    // save useful data into a .json file
    let prove_withdrawal = ProveWithdrawal {
        transaction: initiated_withdrawal.transaction,
        game_index,
        game_l2_block,
        game_addr,
    };
    std::fs::write(
        "prove_withdrawal.json",
        serde_json::to_string_pretty(&prove_withdrawal).unwrap(),
    )
    .unwrap();
}
