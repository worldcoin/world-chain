use crate::{
    args::InitArgs,
    bindings::{InitiatedWithdrawal, L2ToL1MessagePasser, WithdrawalTransaction},
    signer, storage,
    types::{InitiatedOutcome, StepError, StepOutcome, StepStage},
};
use alloy_network::ReceiptResponse;
use alloy_primitives::{Address, B256, Bytes, U256, address};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::TransactionReceipt;
use eyre::eyre::{OptionExt, ensure};

/// Address of the `L2ToL1MessagePasser` contract on L2.
const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");

/// Run the `init` command.
pub async fn run(args: &InitArgs) -> Result<StepOutcome, StepError> {
    args.validate()?;
    // create L2 signer provider
    let wallet = signer::wallet(
        args.l2_private_key.as_deref(),
        args.l2_aws_kms_key_id.as_deref(),
        &args.l2_rpc_endpoint,
        "L2_RPC_ENDPOINT",
        "L2_PRIVATE_KEY or L2_AWS_KMS_KEY_ID (exactly one)",
    )
    .await?;
    let signer_addr = wallet.default_signer().address();
    let l2_provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_client(super::rpc::client(
            &args.l2_rpc_endpoint,
            "L2_RPC_ENDPOINT",
        )?);
    // target address of the L2 -> L1 withdrawal is the same address that sends the tx on L2
    let target_addr = signer_addr;
    // sends the initiate_withdrawal transaction to the L2ToL1MessagePasser contract
    let (initiated_withdrawal, tx_hash) =
        initiate_withdrawal(l2_provider, target_addr, args.value).await?;
    // save the InitiatedWithdrawal data (local path or s3://bucket/key)
    let handoff = serde_json::to_value(&initiated_withdrawal)
        .map_err(|err| StepError::Generic(err.into()))?;
    storage::write_json(&args.initiated, &handoff)
        .await
        .map_err(|err| StepError::PersistenceAfterTransaction {
            transaction_hash: tx_hash,
            stage: StepStage::Init,
            handoff: Box::new(handoff),
            source: err,
        })?;
    // create the StepOutcome
    let initiated_outcome = InitiatedOutcome {
        tx_hash,
        withdrawal_hash: initiated_withdrawal.hash,
        l2_block: initiated_withdrawal.l2_block,
    };
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
    let pending = L2ToL1MessagePasser::new(L2_TO_L1_MESSAGE_PASSER, provider)
        .initiateWithdrawal(target_addr, U256::from(100_000), Bytes::new())
        .gas(250_000)
        .value(value)
        .send()
        .await?;
    let tx_hash = *pending.tx_hash();
    let receipt =
        super::rpc::receipt_with_deadline(tx_hash, StepStage::Init, pending.get_receipt()).await?;
    ensure!(receipt.status(), "L2 withdrawal initiation reverted");
    let tx_hash = receipt.transaction_hash();
    let initiated_withdrawal = initiated_withdrawal_from_receipt(&receipt)?;
    Ok((initiated_withdrawal, tx_hash))
}

fn initiated_withdrawal_from_receipt(
    receipt: &TransactionReceipt,
) -> eyre::Result<InitiatedWithdrawal> {
    let l2_block = receipt
        .block_number()
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
