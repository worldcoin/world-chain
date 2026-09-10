use crate::{
    args::InitArgs,
    bindings::{InitiatedWithdrawal, L2ToL1MessagePasser, WithdrawalTransaction},
};
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, Bytes, U256, address};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::{OptionExt, ensure};
use std::str::FromStr;

/// Address of the `L2ToL1MessagePasser` contract on L2.
const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");

/// Run the `init` command.
pub async fn run(args: &InitArgs) -> eyre::Result<()> {
    // create L2 signer provider
    let local_signer = PrivateKeySigner::from_str(&args.l2_private_key)?;
    let local_signer_addr = local_signer.address();
    let l2_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&args.l2_rpc_endpoint)
        .await?;
    // target address of the L2 -> L1 withdrawal is the same address that sends the tx on L2
    let target_addr = local_signer_addr;
    // sends the initiate_withdrawal transaction to the L2ToL1MessagePasser contract
    let initiated_withdrawal = initiate_withdrawal(l2_provider, target_addr).await?;
    // save the InitiatedWithdrawal data to a .json file
    std::fs::write(
        "initiated_withdrawal.json",
        serde_json::to_string_pretty(&initiated_withdrawal)?,
    )?;
    Ok(())
}

async fn initiate_withdrawal<P>(
    provider: P,
    target_addr: Address,
) -> eyre::Result<InitiatedWithdrawal>
where
    P: Provider,
{
    let receipt = L2ToL1MessagePasser::new(L2_TO_L1_MESSAGE_PASSER, provider)
        .initiateWithdrawal(target_addr, U256::from(100_000), Bytes::new())
        .gas(250_000)
        .send()
        .await?
        .get_receipt()
        .await?;
    ensure!(receipt.status(), "L2 withdrawal initiation reverted");

    let l2_block = receipt
        .block_number
        .ok_or_eyre("withdrawal receipt missing L2 block number")?;
    let message = receipt
        .logs()
        .iter()
        .find_map(|log| {
            log.log_decode_validate::<L2ToL1MessagePasser::MessagePassed>()
                .ok()
        })
        .ok_or_eyre("withdrawal receipt missing MessagePassed event")?;
    let message = message.data();

    Ok(InitiatedWithdrawal {
        transaction: WithdrawalTransaction {
            nonce: message.nonce,
            sender: message.sender,
            target: message.target,
            value: message.value,
            gasLimit: message.gasLimit,
            data: message.data.clone(),
        },
        hash: message.withdrawalHash,
        l2_block,
    })
}
