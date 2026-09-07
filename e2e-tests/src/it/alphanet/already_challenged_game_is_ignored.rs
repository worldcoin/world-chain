use crate::it::utils::devnet::proof_system_client;
use alloy_network::EthereumWallet;
use alloy_primitives::Address;
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use revm_primitives::B256;
use std::str::FromStr;
use world_chain_proof_protocol::{IMultiProofGame::IMultiProofGameInstance, LineageProvider};
use world_chain_proposer::{Proposal, ProposalSubmission, ProposerClient};

#[tokio::test]
#[ignore]
async fn submit_bad_proposal() {
    // fetch env vars
    let l1_private_key_str = std::env::var("L1_PRIVATE_KEY").unwrap();
    let l1_rpc_endpoint = std::env::var("L1_RPC_ENDPOINT").unwrap();
    let dispute_game_factory_str = std::env::var("DISPUTE_GAME_FACTORY").unwrap();
    let dispute_game_facatory_addr = Address::from_str(&dispute_game_factory_str).unwrap();
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&l1_private_key_str).unwrap();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&l1_rpc_endpoint)
        .await
        .unwrap();
    // create Alloy proof system client
    let alloy_proof_system_client = proof_system_client(l1_provider, dispute_game_facatory_addr)
        .await
        .unwrap();
    // create a bad proposal with a bad root claim
    let anchor = alloy_proof_system_client.lineage_anchor().await.unwrap();
    let registered_block_interval = alloy_proof_system_client
        .registered_lineage_config()
        .block_interval;
    let bad_root_claim = B256::with_last_byte(2);
    let bad_proposal = Proposal {
        parent_ref: anchor.address,
        root_claim: bad_root_claim,
        l2_block_number: anchor
            .l2_block_number
            .saturating_add(registered_block_interval),
        attempt: 0,
    };
    // submit bad proposal
    let proposal_submission = alloy_proof_system_client
        .submit_proposal(&bad_proposal)
        .await
        .unwrap();
    // save ProposalSubmission to a .json file
    std::fs::write(
        "submit_bad_proposal.json",
        serde_json::to_string_pretty(&proposal_submission).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn challenge() {
    // fetch env vars
    let l1_private_key_str = std::env::var("L1_PRIVATE_KEY").unwrap();
    let l1_rpc_endpoint = std::env::var("L1_RPC_ENDPOINT").unwrap();
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&l1_private_key_str).unwrap();
    let local_signer_addr = local_signer.address();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&l1_rpc_endpoint)
        .await
        .unwrap();
    // read ProposalSubmission from .json file
    let proposal_submission: ProposalSubmission =
        serde_json::from_slice(&std::fs::read("submit_bad_proposal.json").unwrap()).unwrap();
    // challenge the game
    let game_instance =
        IMultiProofGameInstance::new(proposal_submission.game_address, &l1_provider);
    let challenge_pending_tx = game_instance.challenge().send().await.unwrap();
    let challenge_receipt = challenge_pending_tx.get_receipt().await.unwrap();
    assert!(challenge_receipt.status());
    // ensure the challenger is actually local_signer_addr
    let challenger_addr = game_instance.challenger().call().await.unwrap();
    assert_eq!(challenger_addr, local_signer_addr);
}
