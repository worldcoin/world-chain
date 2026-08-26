# ⚠️ Active Development Warning

> **This code is currently under active development and has not been audited.**
>
> Any external security audits conducted prior to the completion of development will not be valid. Please do not rely on this code for production use until a full audit has been completed and development is finalized.

# World Chain Contracts

This repository contains smart contracts for World Chain, including PBH (Priority Blockspace for Humans) and Fee Vault contracts.

## Proof System Bond Claims

WIP-1006 proposal and challenge bonds use WLD held one-to-one in the upgradeable `WLDStakingVault`. The stock `DisputeGameFactory` remains unchanged and its type-1006 ETH initialization bond is zero. A proposer keeps a standing WLD approval to the vault and calls the stock factory directly; during `initialize` the vault authenticates the clone against the factory's deterministic deployment address and pulls the bond from `gameCreator`.

`MultiProofGame.resolve()` records the outcome and payout credits without moving funds. After ASR finality, `closeGame()` selects normal or refund mode and atomically credits the complete game pot to recipients' reusable vault balances. Each account may later request a WLD withdrawal and transfer it after the vault delay; new requests reset the delay for the full pending amount.

The vault is WIP-1006-only, keeps exact liabilities, and supports old registered game implementations after upgrades. Its ProxyAdmin owner has DelayedWETH-equivalent break-glass `hold` and `recover` authority; `recover` can intentionally make the vault insolvent and therefore remains a production trust assumption. The DisputeGameFactory owner and vault ProxyAdmin owner must remain the same governance authority; bond pulls fail closed if they diverge.

## OP Stack Withdrawal Boundary

The compatibility target is `OptimismPortal2` 5.6.1 shipped by the devnet's version-tagged `op-deployer:v0.7.1` image. Solidity imports are pinned separately to [`op-contracts/v7.0.0` at `a7c88c8`](https://github.com/ethereum-optimism/optimism/tree/a7c88c8d636ceb9944ea0edaf7d033da258778ab/packages/contracts-bedrock), which exposes the same Portal version and the stock dispute interfaces compiled by this repository. `MultiProofGame` implements the Portal-facing `IDisputeGame` ABI and adds the WIP-1006 proof-lane API. The withdrawal E2E runs these compiled game contracts against the Portal, factory, and registry deployed from the pinned `op-deployer` image.

| Portal phase | Required calls | World Chain implementation |
| --- | --- | --- |
| Discover | `disputeGameFactory()`, `gameAtIndex(index)` | Stock OP `AnchorStateRegistry` and `DisputeGameFactory`, filtered to game type `1006` |
| Prove | `isGameProper`, `isGameRespected`, `status`, `createdAt`, `gameType`, `rootClaim` | A proper, respected game may be used while it is still in progress |
| Finalize | `isGameClaimValid` | Requires a proper, respected, non-blacklisted, finalized `DEFENDER_WINS` game after the registry finality delay |
| Emergency controls | `pause`, `blacklistDisputeGame`, `updateRetirementTimestamp` | Stock OP guardian controls; no World Chain registry fork |

`proveWithdrawalTransaction()` selects and records a dispute game, but does not finalize that game or advance the anchor. `finalizeWithdrawalTransaction()` later asks the registry whether the recorded game claim is valid. `closeGame()` is a separate permissionless maintenance call that attempts to advance the anchor used by future WIP-1006 games.

Blacklisting an individual game immediately makes it improper for Portal proofs. An in-progress WIP-1006 child also invalidates itself if its parent is blacklisted or invalid. Stock OP registry semantics do not recursively invalidate already-resolved descendants after a late parent blacklist. An incident affecting an already-resolved lineage therefore requires the guardian to pause and update the retirement timestamp through the approved governance procedure. That update retires every game created at or before the governance transaction, including all existing descendants; proposal activity resumes from games created after the cutover.

The full-stack withdrawal E2E test uses the real OP-deployer Portal, factory, and registry. It proves against an in-progress WIP-1006 game, verifies the proof-maturity and registry-finality delays, finalizes after `DEFENDER_WINS`, and checks that a blacklisted game is rejected.

The first devnet deployment creates the `MultiProofGame` implementation and its WLD staking vault. The separate activation validates the complete wiring, sets the factory ETH bond to zero, registers game type `1006`, and changes the stock registry's respected game type when needed. Later game-implementation rotations must reuse the existing vault, preserving one capital pool across versions. The scripts deploy mock verifier contracts and are not a production deployment procedure. A production activation must use the canonical WLD token, real audited verifier dependencies, and the audited OP governance process for registering and respecting the new game type; it does not upgrade or replace the factory or registry.

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
