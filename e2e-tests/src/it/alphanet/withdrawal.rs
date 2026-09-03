use crate::it::utils::withdrawals::initiate_withdrawal;
use alloy_network::EthereumWallet;
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
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
    // 
}