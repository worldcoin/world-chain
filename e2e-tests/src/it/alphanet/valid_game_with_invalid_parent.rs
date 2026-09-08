use crate::it::utils::devnet::{
    GAME_CHALLENGER_WINS, INVALIDATION_REASON_INVALID_PARENT, INVALIDATION_REASON_PROOF_TIMEOUT,
    proof_system_client, wait_for_challenge_with_timeout,
};
use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, B256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use std::{str::FromStr, time::Duration};
use world_chain_proof_protocol::{
    ConsensusProvider, IMultiProofGame::IMultiProofGameInstance, LineageProvider,
    OptimismConsensusClient,
};
use world_chain_proposer::{Proposal, ProposalSubmission, ProposerClient};

#[tokio::test]
#[ignore]
async fn submit_1_bad_1_valid_games() {
    // fetch env vars
    let l1_private_key_str = std::env::var("L1_PRIVATE_KEY").unwrap();
    let l1_rpc_endpoint = std::env::var("L1_RPC_ENDPOINT").unwrap();
    let l2_op_consensus_endpoint = std::env::var("L2_OP_CONSENSUS_ENDPOINT").unwrap();
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
    // create OptimismConsensusClient to fetch output roots
    let op_consensus_client = OptimismConsensusClient::new(&l2_op_consensus_endpoint);
    // create a bad proposal with a bad root claim
    let anchor = alloy_proof_system_client.lineage_anchor().await.unwrap();
    let registered_block_interval = alloy_proof_system_client
        .registered_lineage_config()
        .block_interval;
    let bad_root_claim = B256::with_last_byte(5);
    let l2_block_number = anchor
        .l2_block_number
        .saturating_add(registered_block_interval);
    let bad_proposal = Proposal {
        parent_ref: anchor.address,
        root_claim: bad_root_claim,
        l2_block_number,
        attempt: 0,
    };
    // submit first bad proposal
    let first_proposal_submission = alloy_proof_system_client
        .submit_proposal(&bad_proposal)
        .await
        .unwrap();
    // create a child valid game (that will still be invalidated due to `INVALID_PARENT`)
    let l2_block_number = l2_block_number.saturating_add(registered_block_interval);
    let valid_root_claim = op_consensus_client
        .output_root_at_block(l2_block_number)
        .await
        .unwrap();
    let bad_proposal = Proposal {
        parent_ref: first_proposal_submission.game_address,
        root_claim: valid_root_claim,
        l2_block_number,
        attempt: 0,
    };
    // submit second proposal
    let second_proposal_submission = alloy_proof_system_client
        .submit_proposal(&bad_proposal)
        .await
        .unwrap();
    // save ProposalSubmissions to a .json file
    let proposal_submissions = vec![first_proposal_submission, second_proposal_submission];
    std::fs::write(
        "proposal_subs.json",
        serde_json::to_string_pretty(&proposal_submissions).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn wait_for_challenger() {
    // fetch env vars
    let l1_private_key_str = std::env::var("L1_PRIVATE_KEY").unwrap();
    let l1_rpc_endpoint = std::env::var("L1_RPC_ENDPOINT").unwrap();
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&l1_private_key_str).unwrap();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&l1_rpc_endpoint)
        .await
        .unwrap();
    // read ProposalSubmissions from .json file
    let proposal_submissions: Vec<ProposalSubmission> =
        serde_json::from_slice(&std::fs::read("proposal_subs.json").unwrap()).unwrap();
    assert_eq!(proposal_submissions.len(), 2);
    // wait for the 1st game to be challenged
    let timeout = Duration::from_secs(10);
    let first_game_addr = proposal_submissions[0].game_address;
    let game_instance = IMultiProofGameInstance::new(first_game_addr, &l1_provider);
    let challenger_addr = wait_for_challenge_with_timeout(&game_instance, timeout)
        .await
        .unwrap();
    println!("challenger address of 1st game: {challenger_addr}");
    // assert defender is not defending this game
    let proof_bitmap = game_instance.proofBitmap().call().await.unwrap();
    assert_eq!(proof_bitmap, 0);
    // note that the 2nd child game won't be challenged because it contains a valid root claim.
    // It will still end up invalidated with `INVALID_PARENT` as invalidation reason.
}

#[tokio::test]
#[ignore]
async fn wait_for_challenger_wins() {
    // fetch env vars
    let l1_private_key_str = std::env::var("L1_PRIVATE_KEY").unwrap();
    let l1_rpc_endpoint = std::env::var("L1_RPC_ENDPOINT").unwrap();
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&l1_private_key_str).unwrap();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&l1_rpc_endpoint)
        .await
        .unwrap();
    // read ProposalSubmissions from .json file
    let proposal_submissions: Vec<ProposalSubmission> =
        serde_json::from_slice(&std::fs::read("proposal_subs.json").unwrap()).unwrap();
    assert_eq!(proposal_submissions.len(), 2);
    // assert proof deadline of 2nd game is elapsed, this way proof deadline of 1st game is elapsed too
    let second_game_addr = proposal_submissions[1].game_address;
    let second_game_instance = IMultiProofGameInstance::new(second_game_addr, &l1_provider);
    let proof_deadline = second_game_instance.proofDeadline().call().await.unwrap();
    let latest_block = l1_provider
        .get_block(BlockId::latest())
        .await
        .unwrap()
        .unwrap();
    let latest_block_timestamp = latest_block.header.timestamp;
    assert!(
        latest_block_timestamp > proof_deadline,
        "proof deadline has not elapsed yet - latest block timestamp: {latest_block_timestamp}, proof deadline: {proof_deadline}"
    );
    // assert that the 1st game has resolved `CHALLENGER_WINS`
    let first_game_addr = proposal_submissions[0].game_address;
    let first_game_instance = IMultiProofGameInstance::new(first_game_addr, &l1_provider);
    let status = first_game_instance.status().call().await.unwrap();
    assert_eq!(status, GAME_CHALLENGER_WINS);
    let invalidation_reason = first_game_instance
        .invalidationReason()
        .call()
        .await
        .unwrap();
    assert_eq!(invalidation_reason, INVALIDATION_REASON_PROOF_TIMEOUT);
    // assert that the 2nd game has resolved `CHALLENGER_WINS`
    let status = second_game_instance.status().call().await.unwrap();
    assert_eq!(status, GAME_CHALLENGER_WINS);
    let invalidation_reason = second_game_instance
        .invalidationReason()
        .call()
        .await
        .unwrap();
    assert_eq!(invalidation_reason, INVALIDATION_REASON_INVALID_PARENT);
}
