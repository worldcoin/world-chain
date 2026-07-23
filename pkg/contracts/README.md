# ⚠️ Active Development Warning

> **This code is currently under active development and has not been audited.**
>
> Any external security audits conducted prior to the completion of development will not be valid. Please do not rely on this code for production use until a full audit has been completed and development is finalized.

# World Chain Contracts

This repository contains smart contracts for World Chain, including PBH (Priority Blockspace for Humans) and Fee Vault contracts.

## Proof System Bond Claims

`WorldChainProofSystemGame.resolve()` records the game outcome and assigns pull-based bond claims. It does not transfer ETH during resolution. After a game resolves, automation such as the challenger, the defender/prover-service flow, or any keeper should call `withdraw(recipient)` for the claimable proposer or challenger.

`claimable(recipient)` returns the amount currently owed to `recipient`. `withdraw(recipient)` is permissionless, but funds are always sent to `recipient`, so the caller cannot redirect or steal another account's claim.

## OP Stack Withdrawal Boundary

The compatibility target is `OptimismPortal2` 5.6.1 shipped by the devnet's version-tagged `op-deployer:v0.7.1` image ([OP source at commit `7525482`](https://github.com/ethereum-optimism/optimism/blob/7525482253bdc076548840638cde165c9004349f/packages/contracts-bedrock/src/L1/OptimismPortal2.sol)). The concrete World Chain game, factory, and registry explicitly implement the narrow interfaces in `src/proofs/interfaces/IOptimismPortal2.sol` and `src/proofs/interfaces/IDisputeGame.sol`; changing a required signature now fails compilation.

| Portal phase | Required calls | World Chain implementation |
| --- | --- | --- |
| Discover | `disputeGameFactory()`, `gameAtIndex(index)` | `WorldChainAnchorStateRegistry`, `WorldChainProofSystemFactory` |
| Prove | `isGameProper`, `isGameRespected`, `status`, `createdAt`, `gameType`, `rootClaim` | Registration and respect are checked while an in-progress game remains proveable |
| Finalize | `isGameClaimValid` | Requires a proper, respected, finalized `DEFENDER_WINS` game |
| Legacy reads | `disputeGameFinalityDelaySeconds`, `respectedGameType`, `retirementTimestamp`, `disputeGameBlacklist` | Exposed with OP-compatible ABI; the finality delay is deployment-configured and defaults to the OP/Base 3.5-day airgap, while WIP-1006 currently has no timestamp-based retirement |

World Chain deliberately keeps finalized game outcomes immutable: the registry owner may pause the registry, change the respected game type, and blacklist an in-progress game, but may not blacklist a `DEFENDER_WINS` game. Production deployment must assign this owner role to the configured guardian Safe. If a finalized root is found invalid during the post-resolution airgap, the supported recovery path is to pause the registry and use the ProxyAdmin-governed Portal migration process before withdrawals can finalize against that root.

This is intentionally not a claim that WIP-1006 implements OP's complete `IDisputeGame`, `IDisputeGameFactory`, or `IAnchorStateRegistry` administration and lifecycle APIs. In particular, WIP-1006's `resolve()` returns its root state and invalidation reason instead of OP's single `GameStatus`. The Portal never calls `resolve()`. `closeGame` is also separate: it advances the anchor used by future proposals and is not part of withdrawal finalization.

Compiler enforcement is paired with the full-stack withdrawal E2E test: the real OP-deployer Portal proves against a WIP-1006 game, rejects blacklisted and unresolved games, and finalizes after the game becomes valid. Because the WIP contracts are deployed after the local L1 genesis is rendered, the devnet performs this retargeting with Anvil's local state API. Production deployment still requires an explicit, audited ProxyAdmin-governed Portal migration; the devnet shortcut is not a production migration mechanism.

## PBH Contracts

[Priority blockspace for humans (PBH)](https://github.com/worldcoin/world-chain?tab=readme-ov-file#world-chain-builder) enables verified World ID users to execute transactions with top of block priority, enabling a more frictionless user experience onchain. This mechanism is designed to ensure that ordinary users aren't unfairly disadvantaged by automated systems and greatly mitigates the negative impact of MEV.

[ERC-4337](https://eips.ethereum.org/EIPS/eip-4337) is designed to enable [account abstraction](https://ethereum.org/en/roadmap/account-abstraction/) via an “entry point” contract and “user operations”. A `UserOperation` is a payload that is defined by a user, specifying actions that a “bundler” can execute on their behalf. The `EntryPoint` contract conducts all of the necessary validation logic, executes the user operation onchain and manages any post execution logic (ex. paymaster logic). Users send their `UserOperation` to “bundlers”, which are services that maintain a mempool of many `UserOperation`s, bundling them together to submit them to the blockchain for inclusion.

4337 PBH features `PBHSignatureAggregator`, `PBHEntryPoint`, and `PBH4337Module` contracts.

*PBHEntryPoint*

The `PBHEntryPoint` acts as a proxy in front of the singleton 4337 EntryPoint contract onchain. The builder is able to identify a PBH transaction by the target. For a transaction to be considered PBH, the `to` address of the transaction must be set to the `PBHEntryPoint`. 

The `PBHEntryPoint` contract exposes two functions:

`handleAggregatedOps()` 
- Allows a Bundler to submit a Priority Bundle transaction where the [aggregated signature](https://github.com/eth-infinitism/account-abstraction/blob/b3bae63bd9bc0ed394dfca8668008213127adb62/contracts/interfaces/IEntryPoint.sol#L144) contains a vector encoding of WorldID proof's, and associated proof data to be verified onchain, or by the block builder ordering the block. 

`pbhMulticall()` 
- The PBH Multicall allows WorldID usrs to execute a multicall with top of block inclusion by attaching a valid WorldID proof in the calldata. The proof is verified either by block builder before transaction inclusion, or onchain. This mechanism enables non-4337 transactions to have top of block inclusion. 

*PBHSignatureAggregator*
- The `PBHSignatureAggregator` serves as a utility contract to the bundler to aggregate UserOperation proofs onto the aggregate signature of `handleAggregatedOps`. It also serves as a cryptographic link between the `PBHEntryPoint`, and the Priority UserOperation thereby guaranteeing a bundler cannot change the target address of a PBH Bundle to the EntryPoint yielding non-priority transaction ordering. 

*PBH4337Module*
- The `PBH4337Module` is an extension of the [Safe 4337 module](https://github.com/worldcoin/safe-modules/blob/9abf69ea1df673c1010aeb9bbbc6aa14124ba425/modules/4337/contracts/Safe4337Module.sol) that returns a custom validation path based on the [nonce key](https://github.com/worldcoin/world-chain/blob/6f0b018fdd937b0d023569755cb90f2a1f1abd65/contracts/src/PBH4337Module.sol#L16). The validation path returned from `_validateSignatures` allows the bundler to seamlessly group PBH UserOperations that specify the `PBHSignatureAggregator`.

Signature Scheme:
```
Bytes [0 : 12] Timestamp Validation Data
Bytes [12 : 65 * signatureThreshold + 12] ECDSA Signatures
Bytes [65 * signatureThreshold + 12 : 65 * signatureThreshold + 364] ABI Encoded Proof Data
```

## Fee Vault Contracts

The Fee Vault contracts manage the distribution and burning of sequencer fees on World Chain.

*FeeRecipient*

The `FeeRecipient` contract acts as the initial receiver of sequencer fees. When ETH is sent to this contract, it automatically splits the incoming funds between:
- A configurable portion sent to the `FeeEscrow` for WLD burns
- The remainder held for withdrawal to a fee vault recipient

The distribution ratio is configurable by the owner (default 50%).

*FeeEscrow*

The `FeeEscrow` contract handles the conversion of ETH to WLD for burning. Key features:
- Holds ETH received from the `FeeRecipient`
- Uses Chainlink oracles (WLD/USD and ETH/USD) to calculate fair exchange rates
- Implements a callback mechanism allowing any executor to perform the swap and burn
- Enforces a minimum interval between burns (default 24 hours)
- Burns WLD by sending it to a dead address (`0xDeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF`)
- Includes slippage protection (0.03%) to ensure fair execution

The burn mechanism requires executors to implement the `IBurnCallback` interface, providing flexibility in how the ETH-to-WLD swap is performed (e.g., via Uniswap V3).
