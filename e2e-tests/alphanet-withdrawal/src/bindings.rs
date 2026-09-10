use alloy_primitives::{Address, B256};
use alloy_sol_types::sol;
use serde::{Deserialize, Serialize};

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

    interface IDisputeGame {}

    #[sol(rpc)]
    interface AnchorStateRegistry {
        function isGameClaimValid(IDisputeGame _game) public view returns (bool);
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
    pub hash: B256,
    pub game_index: u64,
    pub game_l2_block: u64,
    pub game_addr: Address,
    pub proven_at: u64,
}
