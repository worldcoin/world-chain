use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use alloy_provider::Provider;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolValue, sol};
use eyre::eyre::{OptionExt, ensure};
use world_chain_devnet::SUPERCHAIN_GUARDIAN_PRIVATE_KEY;
use world_chain_proof_protocol::MULTI_PROOF_GAME_TYPE;

use crate::it::utils::devnet::{
    GAME_DEFENDER_WINS, advance_to_timestamp, anchor_at, game_at, l1_contract, l1_rpc_url,
    latest_timestamp, signing_provider, try_build_ha_devnet, wait_for_anchor_at_or_beyond,
    wait_for_bond_unlock, wait_for_bond_withdrawal, wait_for_game_finality,
    wait_for_multi_proof_game, wait_for_proof_lane, wait_for_status, weth_at,
};

const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");

sol! {
    struct WithdrawalTransaction {
        uint256 nonce;
        address sender;
        address target;
        uint256 value;
        uint256 gasLimit;
        bytes data;
    }

    struct OutputRootProof {
        bytes32 version;
        bytes32 stateRoot;
        bytes32 messagePasserStorageRoot;
        bytes32 latestBlockhash;
    }

    #[sol(rpc)]
    interface L2ToL1MessagePasser {
        event MessagePassed(
            uint256 indexed nonce,
            address indexed sender,
            address indexed target,
            uint256 value,
            uint256 gasLimit,
            bytes data,
            bytes32 withdrawalHash
        );

        function initiateWithdrawal(address target, uint256 gasLimit, bytes data) external payable;
    }

    #[sol(rpc)]
    interface OptimismPortal {
        function anchorStateRegistry() external view returns (address);
        function disputeGameFactory() external view returns (address);
        function disputeGameFinalityDelaySeconds() external view returns (uint256);
        function proofMaturityDelaySeconds() external view returns (uint256);
        function version() external view returns (string);
        function proveWithdrawalTransaction(
            WithdrawalTransaction tx_,
            uint256 disputeGameIndex,
            OutputRootProof outputRootProof,
            bytes[] withdrawalProof
        ) external;
        function finalizeWithdrawalTransaction(WithdrawalTransaction tx_) external;
        function finalizedWithdrawals(bytes32 withdrawalHash) external view returns (bool);
    }
}

struct InitiatedWithdrawal {
    transaction: WithdrawalTransaction,
    hash: B256,
    l2_block: u64,
}

#[ignore = "requires Docker, Foundry, and the full local OP Stack"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn op_native_wip_1006_portal_withdrawal_and_bond_claim() -> eyre::Result<()> {
    reth_tracing::init_test_tracing();

    let Some(devnet) = try_build_ha_devnet("Portal withdrawal E2E").await? else {
        return Ok(());
    };

    let l1_rpc = l1_rpc_url(&devnet)?;
    let portal_address = l1_contract(devnet.optimism_portal(), "OptimismPortal")?;
    let factory_address = l1_contract(devnet.dispute_game_factory(), "DisputeGameFactory")?;
    let anchor_address = l1_contract(devnet.anchor_state_registry(), "AnchorStateRegistry")?;

    // Prefunded on both L1 and L2 in the devnet genesis, so one key covers the L2 withdrawal
    // initiation and the L1 prove/finalize calls.
    let signer: PrivateKeySigner = SUPERCHAIN_GUARDIAN_PRIVATE_KEY.parse()?;
    let withdrawal_sender = signer.address();
    let l1_provider = signing_provider(l1_rpc, signer.clone())?;
    let l2_provider = signing_provider(&devnet.l2_rpc_url(), signer)?;

    let portal = OptimismPortal::new(portal_address, l1_provider.clone());
    let anchor = anchor_at(anchor_address, l1_provider.clone());
    ensure!(
        portal.version().call().await? == "5.6.1",
        "devnet Portal version does not match the pinned compatibility target"
    );
    ensure!(
        portal.anchorStateRegistry().call().await? == anchor_address,
        "Portal is not wired to the deployed AnchorStateRegistry"
    );
    ensure!(
        portal.disputeGameFactory().call().await? == factory_address,
        "Portal is not wired to the deployed DisputeGameFactory"
    );
    ensure!(
        anchor.respectedGameType().call().await? == MULTI_PROOF_GAME_TYPE,
        "WIP-1006 is not the AnchorStateRegistry's respected game type"
    );

    let withdrawal = initiate_withdrawal(l2_provider.clone(), withdrawal_sender).await?;
    let (game_index, game_address, game_l2_block) =
        wait_for_multi_proof_game(l1_provider.clone(), factory_address, withdrawal.l2_block)
            .await?;
    let game = game_at(game_address, l1_provider.clone());
    ensure!(
        game.wasRespectedGameTypeWhenCreated().call().await?,
        "covering WIP-1006 game was not respected when created"
    );
    ensure!(
        anchor.isGameProper(game_address).call().await?,
        "covering WIP-1006 game is not proper"
    );
    wait_for_proof_lane(&game).await?;

    let (output_root_proof, withdrawal_proof) =
        build_withdrawal_proof(l2_provider, game_l2_block, withdrawal.hash).await?;
    ensure!(
        portal
            .proveWithdrawalTransaction(
                withdrawal.transaction.clone(),
                U256::from(game_index),
                output_root_proof,
                withdrawal_proof,
            )
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "Portal withdrawal proof transaction reverted"
    );

    let current_timestamp = latest_timestamp(&l1_provider).await?;
    let challenge_deadline = game.challengeDeadline().call().await?;
    let proof_maturity_delay: u64 = portal
        .proofMaturityDelaySeconds()
        .call()
        .await?
        .try_into()?;
    advance_to_timestamp(
        &l1_provider,
        current_timestamp
            .saturating_add(proof_maturity_delay)
            .max(challenge_deadline)
            .saturating_add(1),
    )
    .await?;

    wait_for_status(&game, GAME_DEFENDER_WINS).await?;
    let finality_delay: u64 = portal
        .disputeGameFinalityDelaySeconds()
        .call()
        .await?
        .try_into()?;
    let resolved_at = game.resolvedAt().call().await?;
    advance_to_timestamp(
        &l1_provider,
        resolved_at.saturating_add(finality_delay).saturating_add(1),
    )
    .await?;
    wait_for_game_finality(&anchor, game_address).await?;
    ensure!(
        anchor.isGameClaimValid(game_address).call().await?,
        "resolved WIP-1006 game is not a valid Portal claim"
    );

    ensure!(
        portal
            .finalizeWithdrawalTransaction(withdrawal.transaction)
            .send()
            .await?
            .get_receipt()
            .await?
            .status(),
        "Portal withdrawal finalization reverted"
    );
    ensure!(
        portal.finalizedWithdrawals(withdrawal.hash).call().await?,
        "Portal did not persist the finalized withdrawal"
    );
    wait_for_anchor_at_or_beyond(&anchor, game_l2_block).await?;

    let proposer = game.gameCreator().call().await?;
    let weth = weth_at(game.weth().call().await?, l1_provider.clone());
    let unlock_at = wait_for_bond_unlock(&weth, game_address, proposer).await?;
    advance_to_timestamp(&l1_provider, unlock_at).await?;
    wait_for_bond_withdrawal(&weth, game_address, proposer).await?;

    Ok(())
}

async fn initiate_withdrawal<P>(
    provider: P,
    withdrawal_sender: Address,
) -> eyre::Result<InitiatedWithdrawal>
where
    P: Provider,
{
    let receipt = L2ToL1MessagePasser::new(L2_TO_L1_MESSAGE_PASSER, provider)
        .initiateWithdrawal(withdrawal_sender, U256::from(100_000), Bytes::new())
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
