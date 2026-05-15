// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC1271} from "@openzeppelin/contracts/interfaces/IERC1271.sol";
import {IWorldChainAccountHooks} from "./IWorldChainAccountHooks.sol";

/// @title IWorldChainSessionVerifier
/// @author Worldcoin
/// @notice Interface implemented by every WIP-1001 session verifier.
/// @dev Session verifiers are EIP-1271 signers that additionally evaluate a programmable
///      execution-trace policy. Both `isValidSignature` (inherited) and `evaluateSessionPolicy`
///      execute inside restricted validation frames and MUST conform to the WIP-1001 tracer
///      rules. Implementations also implement {IWorldChainAccountHooks-install} to materialise
///      their per-account state at installation time.
/// @custom:security-contact security@toolsforhumanity.com
interface IWorldChainSessionVerifier is IERC1271, IWorldChainAccountHooks {
    /// @notice EIP-2930 access-list entry committed to by a `0x1D` transaction.
    /// @param account The account whose storage is pre-warmed.
    /// @param storageKeys The list of pre-warmed storage keys for `account`.
    struct AccessListEntry {
        address account;
        bytes32[] storageKeys;
    }

    /// @notice Canonical transaction context surfaced to a session policy.
    /// @param signingHash The `0x1D` envelope signing hash signed by the session verifier.
    /// @param chainId The chain id under which the envelope is being validated.
    /// @param account The framed sender — the WIP-1001 account address.
    /// @param sessionVerifier The session verifier selected by the envelope.
    /// @param nonce The envelope nonce.
    /// @param maxPriorityFeePerGas EIP-1559 maximum priority fee per gas.
    /// @param maxFeePerGas EIP-1559 maximum fee per gas.
    /// @param gasLimit Envelope payload gas limit.
    /// @param isCreate Whether the envelope is a contract-creation transaction.
    /// @param target Envelope `to` (zero address for creates).
    /// @param value Envelope value in wei.
    /// @param data Envelope calldata or init code.
    /// @param selector Convenience copy of `data[:4]` for calls; zero for creates or short data.
    /// @param accessList The envelope's EIP-2930 access list.
    /// @param keyRingHash The active `keyRingHash` recorded for `account` at validation time.
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

    /// @notice The flavour of an EVM call captured in an execution trace.
    enum TraceCallKind {
        Call,
        StaticCall,
        DelegateCall,
        Create,
        Create2
    }

    /// @notice One captured call frame from the payload execution trace.
    /// @param depth Call-stack depth at the point of capture.
    /// @param kind The EVM call kind that produced this frame.
    /// @param caller The frame caller.
    /// @param target The frame callee (zero for `Create` / `Create2`; the deployed address is
    ///        captured in a subsequent frame).
    /// @param value Wei forwarded to the frame (zero for `StaticCall`/`DelegateCall`).
    /// @param selector First four bytes of the frame input, or zero for create kinds.
    /// @param inputHash `keccak256(input)` of the frame input.
    /// @param outputHash `keccak256(output)` of the frame return data.
    /// @param success Whether the frame returned cleanly.
    /// @param gasUsed Gas consumed by the frame.
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

    /// @notice One log entry captured during payload execution.
    /// @param emitter The contract emitting the log.
    /// @param topics The log topics.
    /// @param dataHash `keccak256(data)` of the log payload.
    /// @param callIndex Index into `WorldChainExecutionTrace.calls` of the emitting frame.
    struct LogTrace {
        address emitter;
        bytes32[] topics;
        bytes32 dataHash;
        uint32 callIndex;
    }

    /// @notice The tentative payload execution trace presented to a session policy.
    /// @param success Whether the outermost payload frame returned cleanly.
    /// @param gasUsed Total payload gas consumed (excluding validation gas).
    /// @param outputHash `keccak256(output)` of the outermost frame return data.
    /// @param calls All captured call frames in execution order.
    /// @param logs All captured logs in emission order.
    struct WorldChainExecutionTrace {
        bool success;
        uint64 gasUsed;
        bytes32 outputHash;
        CallTrace[] calls;
        LogTrace[] logs;
    }

    /// @notice The full input to {evaluateSessionPolicy}.
    struct ExecutionTraceContext {
        WorldChainTransactionContext transaction;
        WorldChainExecutionTrace trace;
    }

    /// @notice Evaluates whether the tentative payload execution is admissible under this
    ///         session verifier's policy.
    /// @dev Invoked via `DELEGATECALL` from the account router during a `STATICCALL` from the
    ///      manager, so the call is non-state-mutating end-to-end. Implementations MUST conform
    ///      to the WIP-1001 restricted validation frame rules.
    /// @param context The transaction context and tentative execution trace.
    /// @return allowed True iff the payload is policy-admissible.
    function evaluateSessionPolicy(ExecutionTraceContext calldata context) external view returns (bool allowed);
}
