// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";

import {IWorldChainAccountHooks} from "./IWorldChainAccountHooks.sol";

/// @title IWorldChainSessionVerifier
/// @author 0xOsiris, World Contributors
/// @notice Interface implemented by session verifier contracts. Session verifiers act as the
///         transaction level signatories via programmable EIP-1271 smart contracts. Session
///         verifiers dually function as signature verifiers, and session key policy evaluators.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainSessionVerifier is IERC1271, IWorldChainAccountHooks {
    /// @notice EIP-2930 access-list entry mirrored into the canonical transaction context.
    /// @param account The account address whose storage keys are being accessed.
    /// @param storageKeys The set of accessed storage keys for `account`.
    struct AccessListEntry {
        address account;
        bytes32[] storageKeys;
    }

    /// @notice Canonical context for a `0x1D` transaction provided to the session verifier during
    ///         policy evaluation. Mirrors the fields of the `0x1D` envelope plus the active
    ///         `keyRingHash` at the time of execution.
    /// @param signingHash The `0x1D` signing hash for the transaction.
    /// @param chainId The executing chain ID.
    /// @param account The World Chain account framed as the sender.
    /// @param sessionVerifier The session verifier address authorized for this transaction.
    /// @param nonce The transaction nonce consumed from `Account.transactionNonce`.
    /// @param maxPriorityFeePerGas EIP-1559 priority fee cap.
    /// @param maxFeePerGas EIP-1559 fee cap.
    /// @param gasLimit Envelope gas limit applied to payload execution.
    /// @param isCreate Whether the payload is a contract creation (empty `to`).
    /// @param target The payload `to` address; meaningless when `isCreate` is true.
    /// @param value Wei value forwarded with the payload call.
    /// @param data The payload calldata; length MUST NOT exceed `MAX_PAYLOAD_DATA_BYTES`.
    /// @param selector The leading 4 bytes of `data`.
    /// @param accessList The EIP-2930 access list from the envelope.
    /// @param keyRingHash The account's active `keyRingHash` at the time of execution.
    struct WorldChainTransactionContext {
        bytes32 signingHash;
        uint256 chainId;
        address account;
        address sessionVerifier;
        uint64 nonce;
        uint256 maxPriorityFeePerGas;
        uint256 maxFeePerGas;
        uint64 gasLimit;
        bool isCreate;
        address target;
        uint256 value;
        bytes data;
        bytes4 selector;
        AccessListEntry[] accessList;
        bytes32 keyRingHash;
    }

    /// @notice The kind of an EVM sub-call captured in an execution trace frame.
    enum TraceCallKind {
        Call,
        StaticCall,
        DelegateCall,
        Create,
        Create2
    }

    /// @notice An EVM sub-call captured during tentative payload execution.
    /// @param depth The call-stack depth at which the sub-call was made.
    /// @param kind The kind of sub-call (call, static, delegate, create, create2).
    /// @param caller The caller address for the sub-call.
    /// @param target The target address for the sub-call.
    /// @param value Wei value forwarded with the sub-call.
    /// @param selector The leading 4 bytes of the sub-call input.
    /// @param inputHash `keccak256` of the sub-call input.
    /// @param outputHash `keccak256` of the sub-call output.
    /// @param success Whether the sub-call returned successfully.
    /// @param gasUsed Gas consumed by the sub-call.
    struct CallTrace {
        uint32 depth;
        TraceCallKind kind;
        address caller;
        address target;
        uint256 value;
        bytes4 selector;
        bytes32 inputHash;
        bytes32 outputHash;
        bool success;
        uint64 gasUsed;
    }

    /// @notice A log emitted during tentative payload execution.
    /// @param emitter The address that emitted the log.
    /// @param topics The log's indexed topics.
    /// @param dataHash `keccak256` of the log's non-indexed data.
    /// @param callIndex Index of the emitting frame within the trace's `calls`.
    struct LogTrace {
        address emitter;
        bytes32[] topics;
        bytes32 dataHash;
        uint32 callIndex;
    }

    /// @notice The tentative execution trace produced by the protocol for policy evaluation.
    /// @param success Whether the top-level payload call returned successfully.
    /// @param gasUsed Total gas used by the tentative payload execution.
    /// @param outputHash `keccak256` of the top-level payload output.
    /// @param calls Ordered sub-call frames captured during execution.
    /// @param logs Logs emitted during execution.
    struct WorldChainExecutionTrace {
        bool success;
        uint64 gasUsed;
        bytes32 outputHash;
        CallTrace[] calls;
        LogTrace[] logs;
    }

    /// @notice The full context passed to `evaluateSessionPolicy`. Pair of canonical transaction
    ///         context and tentative execution trace. The ABI-encoded length of `trace` MUST NOT
    ///         exceed `MAX_EXECUTION_TRACE_BYTES`.
    /// @param transaction The canonical transaction context.
    /// @param trace The tentative execution trace.
    struct ExecutionTraceContext {
        WorldChainTransactionContext transaction;
        WorldChainExecutionTrace trace;
    }

    /// @notice Evaluates the session key policy against the canonical transaction context and the
    ///         tentative execution trace. Invoked by the protocol in a restricted validation
    ///         frame via `STATICCALL` from `WORLD_CHAIN_ACCOUNT_MANAGER` with value `0` and gas
    ///         exactly `EXECUTION_TRACE_VALIDATION_GAS_LIMIT`. Failure, revert, out-of-gas,
    ///         malformed return data, or a tracer rule violation MUST be treated as a policy
    ///         failure.
    /// @param context The canonical transaction context paired with the tentative execution trace.
    /// @return allowed `true` iff the policy permits the transaction; `false` otherwise.
    function evaluateSessionPolicy(ExecutionTraceContext calldata context) external view returns (bool allowed);
}
