# ⚠️ Active Development Warning

> **This code is currently under active development and has not been audited.**
>
> Any external security audits conducted prior to the completion of development will not be valid. Please do not rely on this code for production use until a full audit has been completed and development is finalized.

# World Chain Contracts

This repository contains smart contracts for World Chain, including PBH (Priority Blockspace for Humans) and Fee Vault contracts.

## Proof System Bond Claims

WIP-1006 proposal and challenge bonds use a deployment-selected token held one-to-one in the upgradeable `ERC20StakingVault`; World Chain configures WLD as that bond token. Participants deposit the token once, without leaving a standing allowance. The stock `DisputeGameFactory` remains unchanged and its type-1006 ETH initialization bond is zero. A proposer calls the stock factory directly; during `initialize` the vault authenticates the clone against the factory's deterministic deployment address and locks the bond from `gameCreator`'s available balance.

`MultiProofGame.resolve()` records the outcome and payout credits without moving funds. After ASR finality, `closeGame()` selects normal or refund mode and atomically credits the complete game pot to recipients' reusable vault balances. Each account may later request a token withdrawal and transfer it after the vault delay; new requests reset the delay for the full pending amount.

The vault is WIP-1006-only and supports old registered game implementations after upgrades. It exposes no administrative path for moving participant balances or extracting backing tokens directly. The DisputeGameFactory owner and vault ProxyAdmin owner must remain the same governance authority; new bond locks fail closed if they diverge.

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

The first devnet deployment creates the `MultiProofGame` implementation and its ERC-20 staking vault. The separate activation validates the complete wiring, sets the factory ETH bond to zero, registers game type `1006`, and changes the stock registry's respected game type when needed. Later game-implementation rotations must reuse the existing vault, preserving one capital pool across versions. The scripts deploy mock verifier contracts and are not a production deployment procedure. A production activation must use the intended canonical bond token, real audited verifier dependencies, and the audited OP governance process for registering and respecting the new game type; it does not upgrade or replace the factory or registry.

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

## Devnet governance

`GOVERNANCE_MODE=eoa` is the default. Existing EOA keys (`DGF_OWNER_KEY`,
`GUARDIAN_KEY`, `OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY`, Nitro `OWNER` / `OWNER_KEY`)
continue to work, and council deployment defaults to threshold one.

`GOVERNANCE_MODE=safe` uses a manually created `GOVERNANCE_SAFE` with two owners,
threshold two, no enabled modules, and the compatibility fallback handler for
ERC-1271 council attestations. Bootstrap requires its address and never needs either
owner's private key. `PRIVATE_KEY` funds deployments. The council verifier is bound
to the same Safe as the owners and guardian.

- `just proof-deploy-council <env>` binds a new verifier to an existing Safe. In EOA
  mode it deploys a council Safe from `COUNCIL_OWNERS` or `ADMIN_PRIVATE_KEY`, with
  threshold one by default.
- `just proof-deploy-nitro <env>` assigns the selected owner immediately to
  CertManager (including its revoker), NitroAttestationVerifier and NitroEnclaveKeyRegistry.
- In Safe mode, `proof-deploy-system` exports the new vault initialization and stops
  before deploying the game. After Safe execution, resume with `ERC20_STAKING_VAULT`
  set to the initialized vault. `proof-activate-system` exports registration,
  zero native bond and respected-game-type calls as a Transaction Builder batch.
- `proof-governance-call`, `proof-approve-pcrs`, and `proof-transfer-nitro-ownership`
  export Safe transactions; preparation never reports an approval as executed.
  Their EOA paths still broadcast by default unless `dry_run=true`.
- `just safe-operation --out /tmp/operation.json call <target> '<signature>' <args>`
  prepares a zero-value CALL for Transaction Builder. Supply `GOVERNANCE_SAFE` and
  `L1_RPC_URL`, and optionally `L1_CHAIN_ID` (default Sepolia). World-chain recipes
  do not load devnet's env files; devnet's `just safe-operation <network> ...` does.
- `council-submit --game <address>` prepares one atomic Safe transaction: approve
  the exact council attestation through `SignMessageLib`, then call the game's
  `submitProofLane` with an empty proof body. The tool validates game registration,
  council, active proof state, and canonical version-matched library code, then
  simulates the batch without persisting state. The output is a Safe transaction
  proposal, not a Transaction Builder import; the approval requires DELEGATECALL.
- `just safe-operation propose --transaction <file> --sender <owner> --browser`
  signs the prepared council transaction as one owner and publishes it to the Safe
  Transaction Service. The other owner confirms and executes in the Safe UI. Use
  `--interactive`, `--account <keystore>`, `--ledger` or `--trezor` instead of
  `--browser` when appropriate. No signature files or shared owner keys are needed.
  Nonce conflicts fail before signing; `council-submit --nonce <unused-nonce>` can
  queue after existing proposals. The tool never executes a transaction.
- Council proposals support Sepolia Safes 1.3.0, 1.4.1 and 1.5.0. Library addresses
  and code hashes are pinned to the official Safe deployment registry. Other
  versions fail explicitly until their audited library entries are added.
  `SAFE_TRANSACTION_SERVICE_URL` defaults to the official Sepolia API base URL;
  set `SAFE_TRANSACTION_SERVICE_API_KEY` for authenticated access if needed.
  Service calls have bounded timeouts and never automatically retry proposals.
- `proof-submit-council` retains the default EOA council's `COUNCIL_SIGNER_KEY` flow.
  Supplied combined `COUNCIL_SIGNATURES` can still be relayed with `PRIVATE_KEY`.

The direct Forge scripts retain their earlier two-key execution path for existing
callers without `SAFE_TX_OUT`. The recipes above always prepare transactions in Safe
mode. Use the manual workflow to test independent approvals.

Reused Nitro contracts require an explicit ownership handoff. Set
`CERT_MANAGER_ADDRESS`, `NITRO_ATTESTATION_VERIFIER`, `NITRO_ENCLAVE_KEY_REGISTRY`
(or supply the matching `<env>-nitro.json`), and `NEW_NITRO_OWNER`. Set
`GOVERNANCE_MODE` and keys for the **current** authority, then simulate:

```bash
just dry_run=true proof-transfer-nitro-ownership alphanet
```

In EOA mode the recipe broadcasts by default when `dry_run` is omitted. In Safe
mode it exports a batch for review and execution. It moves all three owners and
the CertManager revoker, checks the wiring, and supports resuming a partially
completed handoff. It preserves verifier addresses and existing games'
Nitro identities. Moving Nitro authority does not revoke approved PCRs or registered
keys. Existing games keep their council verifier; a new council address is selected
by deploying/activating a new game implementation.

The devnet repository orchestrates these settings before `just setup <network>`.
Safe mode pauses with exit status 75 and prints the file to approve. Execute it in
Safe, then use `just resume-setup <network>`; resume never resets the OP Stack state.
See [the full reset runbook](../../docs/proof/devnet-reset.md) for both modes.
Existing proof parameters and mock bond tokens remain development settings.
