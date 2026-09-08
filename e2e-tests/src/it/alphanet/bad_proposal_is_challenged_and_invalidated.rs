use crate::it::utils::devnet::{
    GAME_CHALLENGER_WINS, IMockBondToken, proof_system_client, wait_for_challenge_with_timeout,
};
use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use revm_primitives::B256;
use std::{str::FromStr, time::Duration};
use world_chain_proof_protocol::{
    IDisputeGameFactory::IDisputeGameFactoryInstance,
    IERC20StakingVault::IERC20StakingVaultInstance, IMultiProofGame::IMultiProofGameInstance,
    LineageProvider, read_registered_bond_vault,
};
use world_chain_proposer::{Proposal, ProposalSubmission, ProposerClient};

/// Mock bond tokens deposited for each throwaway proof-system participant (100 tokens).
const THROWAWAY_ACCOUNT_BOND_TOKEN_BALANCE: u128 = 100_000_000_000_000_000_000;

#[tokio::test]
#[ignore]
async fn fund_l1_signer_mock_token() {
    // fetch env vars
    let l1_private_key_str = std::env::var("L1_PRIVATE_KEY").unwrap();
    let l1_rpc_endpoint = std::env::var("L1_RPC_ENDPOINT").unwrap();
    let dispute_game_factory_str = std::env::var("DISPUTE_GAME_FACTORY").unwrap();
    let dispute_game_facatory_addr = Address::from_str(&dispute_game_factory_str).unwrap();
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&l1_private_key_str).unwrap();
    let local_signer_addr = local_signer.address();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&l1_rpc_endpoint)
        .await
        .unwrap();
    // create dispute game factory instance
    let dispute_game_factory_instance =
        IDisputeGameFactoryInstance::new(dispute_game_facatory_addr, l1_provider.clone());
    // read the registered bond vault contract
    let bond_vault_addr = read_registered_bond_vault(&l1_provider, &dispute_game_factory_instance)
        .await
        .unwrap();
    let bond_vault_instance = IERC20StakingVaultInstance::new(bond_vault_addr, &l1_provider);
    // get the bond token contract
    let bond_token_address = bond_vault_instance.token().call().await.unwrap();
    let mock_bond_token = IMockBondToken::new(bond_token_address, &l1_provider);
    // mint tokens
    let amount = U256::from(THROWAWAY_ACCOUNT_BOND_TOKEN_BALANCE);
    let mint_pending_tx = mock_bond_token
        .mint(local_signer_addr, amount)
        .send()
        .await
        .unwrap();
    let mint_receipt = mint_pending_tx.get_receipt().await.unwrap();
    assert!(mint_receipt.status());
    // approve mock tokens to be used by the bond vault contract
    let approve_pending_tx = mock_bond_token
        .approve(bond_vault_addr, amount)
        .send()
        .await
        .unwrap();
    let approve_receipt = approve_pending_tx.get_receipt().await.unwrap();
    assert!(approve_receipt.status());
    // deposit mock tokens into the bond vault contract
    let deposit_pending_tx = bond_vault_instance
        .deposit(local_signer_addr, amount)
        .send()
        .await
        .unwrap();
    let deposit_receipt = deposit_pending_tx.get_receipt().await.unwrap();
    assert!(deposit_receipt.status());
}

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
    let bad_root_claim = B256::with_last_byte(1);
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
    // read ProposalSubmission from .json file
    let proposal_submission: ProposalSubmission =
        serde_json::from_slice(&std::fs::read("submit_bad_proposal.json").unwrap()).unwrap();
    // wait for the game to be challenged
    let timeout = Duration::from_secs(10);
    let game_instance = IMultiProofGameInstance::new(proposal_submission.game_address, l1_provider);
    let challenger_addr = wait_for_challenge_with_timeout(&game_instance, timeout)
        .await
        .unwrap();
    println!("challenger address: {challenger_addr}");
    // assert defender is not defending this game
    let proof_bitmap = game_instance.proofBitmap().call().await.unwrap();
    assert_eq!(proof_bitmap, 0);
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
    let proposal_submission: ProposalSubmission =
        serde_json::from_slice(&std::fs::read("submit_bad_proposal.json").unwrap()).unwrap();
    // assert proof deadline is elapsed
    let game_instance =
        IMultiProofGameInstance::new(proposal_submission.game_address, &l1_provider);
    let proof_deadline = game_instance.proofDeadline().call().await.unwrap();
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
    // assert that the game has resolved `CHALLENGER_WINS`
    let status = game_instance.status().call().await.unwrap();
    assert_eq!(status, GAME_CHALLENGER_WINS);
}
