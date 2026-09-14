use crate::{
    args::InitArgs,
    bindings::{InitiatedWithdrawal, L2ToL1MessagePasser, WithdrawalTransaction},
    storage,
    types::{InitiatedOutcome, StepError, StepOutcome},
};
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, B256, Bytes, U256, address};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre::{OptionExt, ensure};
use std::str::FromStr;

/// Address of the `L2ToL1MessagePasser` contract on L2.
const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");

/// Run the `init` command.
pub async fn run(args: &InitArgs) -> Result<StepOutcome, StepError> {
    // create L2 signer provider
    let local_signer = PrivateKeySigner::from_str(&args.l2_private_key)
        .map_err(|err| StepError::Generic(err.into()))?;
    let local_signer_addr = local_signer.address();
    let l2_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&args.l2_rpc_endpoint)
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    // target address of the L2 -> L1 withdrawal is the same address that sends the tx on L2
    let target_addr = local_signer_addr;
    // sends the initiate_withdrawal transaction to the L2ToL1MessagePasser contract
    let (initiated_withdrawal, tx_hash) = initiate_withdrawal(l2_provider, target_addr, args.value)
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    // save the InitiatedWithdrawal data (local path or s3://bucket/key)
    storage::write_json(&args.initiated, &initiated_withdrawal)
        .await
        .map_err(|err| StepError::PersistenceAfterTransaction {
            transaction_hash: tx_hash,
            source: err.into(),
        })?;
    // create the StepOutcome
    let initiated_outcome = InitiatedOutcome {
        tx_hash,
        withdrawal_hash: initiated_withdrawal.hash,
        l2_block: initiated_withdrawal.l2_block,
    };
    tracing::info!("Initiated outcome: {:?}", initiated_outcome);
    Ok(StepOutcome::Initiated(initiated_outcome))
}

async fn initiate_withdrawal<P>(
    provider: P,
    target_addr: Address,
    value: U256,
) -> eyre::Result<(InitiatedWithdrawal, B256)>
where
    P: Provider,
{
    let receipt = L2ToL1MessagePasser::new(L2_TO_L1_MESSAGE_PASSER, provider)
        .initiateWithdrawal(target_addr, U256::from(100_000), Bytes::new())
        .gas(250_000)
        .value(value)
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
    let tx_hash = receipt.transaction_hash;

    Ok((
        InitiatedWithdrawal {
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
        },
        tx_hash,
    ))
}
