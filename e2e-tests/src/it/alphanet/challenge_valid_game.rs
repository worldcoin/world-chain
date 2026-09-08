//! A valid game that gets challenged should still be able to end up `DEFENDER_WINS`.

use crate::it::utils::devnet::{GAME_DEFENDER_WINS, GAME_IN_PROGRESS};
use alloy_network::EthereumWallet;
use alloy_primitives::Address;
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use revm_primitives::U256;
use std::str::FromStr;
use world_chain_proof_protocol::{
    ConsensusProvider, IDisputeGameFactory::IDisputeGameFactoryInstance,
    IMultiProofGame::IMultiProofGameInstance, OptimismConsensusClient,
};

#[tokio::test]
#[ignore]
async fn challenge_valid_game() {
    // fetch env vars
    let l1_private_key_str = std::env::var("L1_PRIVATE_KEY").unwrap();
    let l1_rpc_endpoint = std::env::var("L1_RPC_ENDPOINT").unwrap();
    let l2_op_consensus_endpoint = std::env::var("L2_OP_CONSENSUS_ENDPOINT").unwrap();
    let dispute_game_factory_str = std::env::var("DISPUTE_GAME_FACTORY").unwrap();
    let dispute_game_factory_addr = Address::from_str(&dispute_game_factory_str).unwrap();
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&l1_private_key_str).unwrap();
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&l1_rpc_endpoint)
        .await
        .unwrap();
    // create OptimismConsensusClient to fetch output roots
    let op_consensus_client = OptimismConsensusClient::new(&l2_op_consensus_endpoint);
    // fetch the latest valid game
    let dispute_game_factory_instance =
        IDisputeGameFactoryInstance::new(dispute_game_factory_addr, &l1_provider);
    let game_count: u64 = dispute_game_factory_instance
        .gameCount()
        .call()
        .await
        .unwrap()
        .try_into()
        .unwrap();
    let mut valid_game = None;
    for index in (0..game_count).rev() {
        let game_at_index_return = dispute_game_factory_instance
            .gameAtIndex(U256::from(index))
            .call()
            .await
            .unwrap();
        let game = IMultiProofGameInstance::new(game_at_index_return.proxy, &l1_provider);
        let root_claim = game.rootClaim().call().await.unwrap();
        let l2_block_number = game.l2SequenceNumber().call().await.unwrap();
        let expected_root_claim = op_consensus_client
            .output_root_at_block(l2_block_number.try_into().unwrap())
            .await
            .unwrap();
        if root_claim == expected_root_claim {
            valid_game = Some(game);
            break;
        }
    }
    let valid_game = valid_game.unwrap();
    // challenge valid game
    let challenge_pending_tx = valid_game.challenge().send().await.unwrap();
    let challenge_receipt = challenge_pending_tx.get_receipt().await.unwrap();
    assert!(challenge_receipt.status());
    // save game address to a .json file
    std::fs::write(
        "challenged_game_addr.json",
        serde_json::to_string_pretty(valid_game.address()).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn wait_defender_wins() {
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
    // read challenged game address from .json file
    let game_addr: Address =
        serde_json::from_slice(&std::fs::read("challenged_game_addr.json").unwrap()).unwrap();
    let game_instance = IMultiProofGameInstance::new(game_addr, &l1_provider);
    // check for defender wins
    let status = game_instance.status().call().await.unwrap();
    // ensure the game is not in progress anymore
    assert_ne!(status, GAME_IN_PROGRESS);
    // ensure the game is DEFENDER_WINS
    assert_eq!(status, GAME_DEFENDER_WINS);
}
