use crate::{
    args::ProveArgs,
    bindings::{
        IDisputeGameFactory::IDisputeGameFactoryInstance, IMultiProofGame::IMultiProofGameInstance,
        InitiatedWithdrawal, OptimismPortal::OptimismPortalInstance, OutputRootProof,
        ProveWithdrawal,
    },
    storage,
};
use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_network::{EthereumWallet, ReceiptResponse};
use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolValue;
use eyre::eyre::{OptionExt, ensure, eyre};
use std::str::FromStr;

/// WIP-1006 game type.
const MULTI_PROOF_GAME_TYPE: u32 = 1006;
/// Address of the `L2ToL1MessagePasser` contract on L2.
const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");

/// Run the `prove` command.
pub async fn run(args: &ProveArgs) -> eyre::Result<()> {
    // create L1 signer provider
    let local_signer = PrivateKeySigner::from_str(&args.l1_args.l1_private_key)?;
    let l1_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(local_signer))
        .connect(&args.l1_args.l1_rpc_endpoint)
        .await?;
    // create L2 provider
    let l2_provider = ProviderBuilder::new()
        .connect(&args.l2_rpc_endpoint)
        .await?;
    // read InitiatedWithdrawal data (local path or s3://bucket/key)
    let initiated_withdrawal: InitiatedWithdrawal = storage::read_json(&args.input).await?;
    // wait for a covering WIP1006 game with l2SequenceNumber >= initiated_withdrawal.l2_block
    let (game_index, game_addr, game_l2_block) = check_multi_proof_game(
        &l1_provider,
        args.dispute_game_factory,
        initiated_withdrawal.l2_block,
    )
    .await?;
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
        .await?;
    let receipt = pending_tx.get_receipt().await?;
    ensure!(
        receipt.status(),
        "ProveWithdrawalTransaction tx has not succeeded. Tx hash: {}",
        receipt.transaction_hash
    );
    // get the L1 timestamp of the block that includes this proveWithdrawal
    let block_number = receipt
        .block_number()
        .ok_or_eyre("Block number not found.")?;
    let block = l1_provider
        .get_block(BlockId::number(block_number))
        .await?
        .ok_or_eyre("Block not found.")?;
    let l1_timestamp = block.header.timestamp;
    // save useful data into a .json file
    let prove_withdrawal = ProveWithdrawal {
        transaction: initiated_withdrawal.transaction,
        hash: initiated_withdrawal.hash,
        game_index,
        game_l2_block,
        game_addr,
        proven_at: l1_timestamp,
    };
    storage::write_json(&args.output, &prove_withdrawal).await?;
    Ok(())
}

async fn check_multi_proof_game<P>(
    provider: P,
    factory_address: Address,
    min_l2_block: u64,
) -> eyre::Result<(u64, Address, u64)>
where
    P: Provider,
{
    let factory = IDisputeGameFactoryInstance::new(factory_address, &provider);
    // iterate over the last 100 games
    let game_count: u64 = factory.gameCount().call().await?.try_into()?;
    let game_count_sub_100 = game_count.saturating_sub(100);
    for index in game_count_sub_100..game_count {
        let entry = factory.gameAtIndex(U256::from(index)).call().await?;
        if entry.gameType != MULTI_PROOF_GAME_TYPE {
            continue;
        }
        let game = IMultiProofGameInstance::new(entry.proxy, &provider);
        let l2_block: u64 = game.l2SequenceNumber().call().await?.try_into()?;
        if l2_block >= min_l2_block {
            return Ok((index, entry.proxy, l2_block));
        }
    }
    Err(eyre!("There is no game that covers {} yet", min_l2_block))
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
