use crate::{
    args::ProveArgs,
    bindings::{
        IDisputeGameFactory::IDisputeGameFactoryInstance, IMultiProofGame::IMultiProofGameInstance,
        InitiatedWithdrawal, OptimismPortal::OptimismPortalInstance, OutputRootProof,
        ProveWithdrawal,
    },
    storage,
    types::{ProvenOutcome, StepError, StepOutcome, StepStage, WaitingOutcome, WaitingReason},
};
use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_network::{EthereumWallet, ReceiptResponse};
use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256, ruint::FromUintError};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolValue;
use eyre::eyre::{OptionExt, WrapErr, ensure, eyre};
use std::str::FromStr;

/// WIP-1006 game type.
const MULTI_PROOF_GAME_TYPE: u32 = 1006;
/// Address of the `L2ToL1MessagePasser` contract on L2.
const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");

/// Run the `prove` command.
pub async fn run(args: &ProveArgs) -> Result<StepOutcome, StepError> {
    args.validate()?;
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&args.l1_args.l1_private_key).map_err(|_| {
        StepError::InvalidConfiguration {
            field: "L1_PRIVATE_KEY",
        }
    })?;
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect_client(super::rpc::client(
            &args.l1_args.l1_rpc_endpoint,
            "L1_RPC_ENDPOINT",
        )?);
    // create L2 provider
    let l2_provider = ProviderBuilder::new().connect_client(super::rpc::client(
        &args.l2_rpc_endpoint,
        "L2_RPC_ENDPOINT",
    )?);
    // read InitiatedWithdrawal data (local path or s3://bucket/key)
    let initiated_withdrawal: InitiatedWithdrawal = storage::read_json(&args.initiated).await?;
    // wait for a covering WIP1006 game with l2SequenceNumber >= initiated_withdrawal.l2_block
    let Some((game_index, game_addr, game_l2_block)) = check_multi_proof_game(
        &l1_provider,
        args.dispute_game_factory,
        initiated_withdrawal.l2_block,
    )
    .await?
    else {
        return Ok(StepOutcome::Waiting(WaitingOutcome {
            withdrawal_hash: initiated_withdrawal.hash,
            stage: StepStage::Prove,
            waiting_reason: WaitingReason::CoveringGameUnavailable {
                withdrawal_l2_block: initiated_withdrawal.l2_block,
            },
        }));
    };
    // get the output root proof and the withdrawal proof
    let (output_root_proof, withdrawal_proof) =
        build_withdrawal_proof(l2_provider, game_l2_block, initiated_withdrawal.hash).await?;
    // send the OptimismPortal::proveWithdrawalTransaction
    let optimism_portal = OptimismPortalInstance::new(args.optimism_portal, &l1_provider);
    let pending_tx = optimism_portal
        .proveWithdrawalTransaction(
            initiated_withdrawal.transaction.clone(),
            U256::from(game_index),
            output_root_proof,
            withdrawal_proof,
        )
        .send()
        .await
        .map_err(|err| StepError::Generic(err.into()))?;
    let tx_hash = *pending_tx.tx_hash();
    let receipt =
        super::receipt_with_deadline(tx_hash, StepStage::Prove, pending_tx.get_receipt()).await?;
    if !receipt.status() {
        return Err(StepError::Generic(eyre!(
            "ProveWithdrawalTransaction tx has not succeeded. Tx hash: {}",
            receipt.transaction_hash()
        )));
    }
    let proven_at = proven_at_from_receipt(&l1_provider, &receipt).await?;
    let withdrawal_hash = initiated_withdrawal.hash;
    let withdrawal_l2_block = initiated_withdrawal.l2_block;
    let prove_withdrawal = prove_withdrawal(
        initiated_withdrawal,
        game_index,
        game_addr,
        game_l2_block,
        proven_at,
    );
    let handoff =
        serde_json::to_value(&prove_withdrawal).map_err(|err| StepError::Generic(err.into()))?;
    storage::write_json(&args.proven, &handoff)
        .await
        .map_err(|err| StepError::PersistenceAfterTransaction {
            transaction_hash: receipt.transaction_hash(),
            stage: StepStage::Prove,
            handoff: Box::new(handoff),
            source: err,
        })?;
    Ok(StepOutcome::Proven(ProvenOutcome {
        tx_hash: receipt.transaction_hash(),
        withdrawal_hash,
        withdrawal_l2_block,
    }))
}

fn prove_withdrawal(
    initiated_withdrawal: InitiatedWithdrawal,
    game_index: u64,
    game_addr: Address,
    game_l2_block: u64,
    proven_at: u64,
) -> ProveWithdrawal {
    ProveWithdrawal {
        transaction: initiated_withdrawal.transaction,
        hash: initiated_withdrawal.hash,
        game_index,
        game_l2_block,
        game_addr,
        proven_at,
    }
}

async fn proven_at_from_receipt<P: Provider>(
    provider: &P,
    receipt: &impl ReceiptResponse,
) -> Result<u64, StepError> {
    let block_number = receipt
        .block_number()
        .ok_or_eyre("prove receipt missing L1 block number")
        .map_err(|err| StepError::Generic(err))?;
    let block = provider
        .get_block(BlockId::number(block_number))
        .await
        .map_err(|err| StepError::Generic(err.into()))?
        .ok_or_eyre("prove L1 block not found")
        .map_err(|err| StepError::Generic(err))?;
    Ok(block.header.timestamp())
}

async fn check_multi_proof_game<P>(
    provider: P,
    factory_address: Address,
    min_l2_block: u64,
) -> Result<Option<(u64, Address, u64)>, StepError>
where
    P: Provider,
{
    let factory = IDisputeGameFactoryInstance::new(factory_address, &provider);
    // iterate over the last 100 games
    let game_count: u64 = factory
        .gameCount()
        .call()
        .await
        .map_err(|err| StepError::Generic(err.into()))?
        .try_into()
        .map_err(|err: FromUintError<u64>| StepError::Generic(err.into()))?;
    let game_count_sub_100 = game_count.saturating_sub(100);
    for index in game_count_sub_100..game_count {
        let entry = factory
            .gameAtIndex(U256::from(index))
            .call()
            .await
            .map_err(|err| StepError::Generic(err.into()))?;
        if entry.gameType != MULTI_PROOF_GAME_TYPE {
            continue;
        }
        let game = IMultiProofGameInstance::new(entry.proxy, &provider);
        let l2_block: u64 = game
            .l2SequenceNumber()
            .call()
            .await
            .map_err(|err| StepError::Generic(err.into()))?
            .try_into()
            .map_err(|err: FromUintError<u64>| StepError::Generic(err.into()))?;
        if l2_block >= min_l2_block {
            return Ok(Some((index, entry.proxy, l2_block)));
        }
    }
    Ok(None)
}

async fn build_withdrawal_proof<P>(
    l2_provider: P,
    game_l2_block: u64,
    withdrawal_hash: B256,
) -> eyre::Result<(OutputRootProof, Vec<Bytes>)>
where
    P: Provider,
{
    let block = l2_provider
        .get_block_by_number(BlockNumberOrTag::Number(game_l2_block))
        .await?
        .ok_or_eyre("WIP-1006 output block missing from L2")?;
    let storage_key = keccak256((withdrawal_hash, U256::ZERO).abi_encode_params());
    let account_proof = l2_provider
        .get_proof(L2_TO_L1_MESSAGE_PASSER, vec![storage_key])
        .block_id(BlockId::Number(BlockNumberOrTag::Number(game_l2_block)))
        .await?;
    let storage_proof = account_proof
        .storage_proof
        .first()
        .ok_or_eyre("eth_getProof returned no withdrawal storage proof")?;
    ensure!(
        storage_proof.value == U256::from(1),
        "withdrawal is absent from the message passer"
    );

    Ok((
        OutputRootProof {
            version: B256::ZERO,
            stateRoot: block.header.state_root(),
            messagePasserStorageRoot: account_proof.storage_hash,
            latestBlockhash: block.header.hash,
        },
        storage_proof.proof.clone(),
    ))
}
