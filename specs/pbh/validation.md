# PBH Validation

Upon receiving new transactions, the World Chain Builder will first ensure that the payload is [a valid OP Stack transaction](https://github.com/paradigmxyz/reth/blob/1e965caf5fa176f244a31c0d2662ba1b590938db/crates/optimism/txpool/src/validator.rs#L136-L203). In addition to the default checks, the builder will also [evaluate transactions for PBH conditions](https://github.com/worldcoin/world-chain/blob/kit/docs/world-chain-builder/crates/world/pool/src/validator.rs#L180-L204).

Any transaction that calls the `pbhMulticall()` or `handleAggregatedOps()` function on the `PBHEntryPoint` will be considered a PBH transaction and must clear PBH Validation. PBH transactions must contain a valid `PBHPayload` or `PBHPayload[]` in the case of PBH 4337 bundles.

```solidity
    struct PBHPayload {
        uint256 root;
        uint256 pbhExternalNullifier;
        uint256 nullifierHash;
        uint256[8] proof;
    }
```

### Signal Hash
Transactions that target the `pbhMulticall()` function must provide a valid `PBHPayload` where included `proof` is generated with a `signalHash` specified as:
```solidity
uint256 signalHash = abi.encode(msg.sender, calls).hashToField();
```

Transactions that target the `handleAggregatedOps()`function (ie. PBH 4337 Bundles) must contain an aggregated signature consisting of an array of `PBHPayload` where there is a `PBHPayload` for each `UserOp` in the bundle. The included `proof` must be generated with a `signalHash` specified as:
```solidity
uint256 signalHash = abi.encodePacked(sender, userOp.nonce, userOp.callData).hashToField();
```

### External Nullifier
PBH transactions must contain a valid external nullifier where:
 - The `month` is the current month
 - The `year` is the current year (specified as `yyyy`)
 - The `pbhNonce` is < `pbhNonceLimit`. PBH nonces are `0` indexed, meaning if the `pbhNonce` limit is `29`, a user is allotted `30` PBH transactions per month.

### Root
The `root` provided must be a valid [World ID Root](https://github.com/worldcoin/world-id-contracts/blob/main/src/WorldIDIdentityManagerImplV1.sol#L67) with a timestamp less than 7 days old.

### Proof 
The `proof` must be a valid semaphore proof, proving inclusion in the World ID set associated with the specified `root`.

