use alloy_consensus::BlockHeader;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, B256, Bytes, U256, address, keccak256};
use alloy_provider::Provider;
use alloy_sol_types::{SolValue, sol};
use eyre::eyre::{OptionExt, ensure};
use serde::{Deserialize, Serialize};

const L2_TO_L1_MESSAGE_PASSER: Address = address!("4200000000000000000000000000000000000016");

sol! {
    #[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct InitiatedWithdrawal {
    pub transaction: WithdrawalTransaction,
    pub hash: B256,
    pub l2_block: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProveWithdrawal {
    pub transaction: WithdrawalTransaction,
    pub game_index: u64,
    pub game_l2_block: u64,
    pub game_addr: Address,
}

pub async fn initiate_withdrawal<P>(
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

pub async fn build_withdrawal_proof<P>(
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
