# Upgrading the game type 1006 implementation

Swaps the `MultiProofGame` implementation registered on an existing `DisputeGameFactory`
without redeploying the proof system around it. Written against alphanet; the same sequence
applies anywhere `just proof-*` is configured.

## What an in-place swap does and does not change

`DisputeGameFactory.create` clones the implementation registered *at creation time*, and a
clone keeps pointing at the address it was cloned from. So `setImplementation(1006, newImpl)`
only affects games created after the call:

- Games already in flight keep the old code, the old ABI, and the old bond accounting. They
  resolve normally.
- New games use the new code immediately.
- For as long as both exist, every service that reads or writes games must handle both ABIs.
  That window is bounded by the longest live game: one `proofPeriod` (7 days on alphanet)
  worst case, one `challengePeriod` (24 h) for an unchallenged proposal.

`domainHash` must not move. It is committed into each game's `extraData` and every prover
derives `rootId` from it, so a changed domain hash silently invalidates every unresolved
proposal and every queued proof instead of failing anywhere visible.
`UpgradeGameImplementation.s.sol` compares it against the outgoing implementation and reverts
on drift — that check is the reason the script reads its parameters off chain rather than
taking them from a config file.

## Before you start

```bash
source scripts/proof-envs/alphanet.env
export L1_RPC_URL=<sepolia rpc>
export DISPUTE_GAME_FACTORY=$(jq -r .disputeGameFactory pkg/contracts/deployments/alphanet-proof-system.json)
export ROLLUP_CONFIG_HASH=$(jq -r .rollupConfigHash pkg/contracts/deployments/alphanet-proof-system.json)
```

`DGF_OWNER_KEY` must be the key for `DisputeGameFactory.owner()`. Confirm before signing:

```bash
cast call $DISPUTE_GAME_FACTORY 'owner()(address)' --rpc-url $L1_RPC_URL
```

## 1. Re-key the validity lane if the vkeys moved

`aggregationVKey` and `rangeVKeyCommitment` are immutable on `SP1ValidityVerifier`, and the
verifier address is immutable on `MultiProofGame`. A vkey change is therefore a two-contract
operation, and it has no loud failure mode: a game wired to a stale verifier accepts proposals
and rejects every proof the workers produce, surfacing only as challenge windows timing out.

Compare first:

```bash
VERIFIER=$(jq -r .validityProofVerifier pkg/contracts/deployments/alphanet-proof-system.json)
cast call $VERIFIER 'aggregationVKey()(bytes32)'      --rpc-url $L1_RPC_URL
cast call $VERIFIER 'rangeVKeyCommitment()(bytes32)'  --rpc-url $L1_RPC_URL
jq -r '.aggregation_vkey, .range_vkey_commitment' proofs/backends/sp1/elfs/vkeys.json
```

If they differ, deploy a replacement and carry its address into step 2:

```bash
export SP1_VERIFIER_GATEWAY=$(cast call $VERIFIER 'sp1Verifier()(address)' --rpc-url $L1_RPC_URL)
just dry_run=true proof-deploy-validity-verifier alphanet
just proof-deploy-validity-verifier alphanet
export VALIDITY_PROOF_VERIFIER=<address printed above>
```

If they match, skip this step and leave `VALIDITY_PROOF_VERIFIER` unset — the upgrade then
reuses the registered verifier.

## 2. Deploy and register the new implementation

Dry run first. It simulates the deploy, the `domainHash` check and the `setImplementation`
call, and writes nothing:

```bash
just dry_run=true proof-upgrade-game alphanet
```

Check the printed `domainHash (unchanged)` against the live value before broadcasting:

```bash
cast call $(jq -r .gameImplementation pkg/contracts/deployments/alphanet-proof-system.json) \
  'domainHash()(bytes32)' --rpc-url $L1_RPC_URL
```

Then broadcast. This is the only irreversible step; everything after it is a config rollout.

```bash
just proof-upgrade-game alphanet
```

The script updates `gameImplementation` and records `previousGameImplementation` in
`pkg/contracts/deployments/alphanet-proof-system.json`. Commit that file.

## 3. Roll out the services

The prover services must be on a build that speaks the new ABI *before* the first game is
created against the new implementation — the proposer creates games on a timer, so this
window is short. Bump all six images together in `crypto-apps`:
`world-chain-proof-{proposer,challenger,defender,prover-service,sp1-worker,nitro-worker}`.

The defender additionally accepts `PROOF_REWARD_RECIPIENT`, the address credited each
submitted lane's share of a forfeited challenger bond. It defaults to the defender signer;
set it only if the reward should land somewhere else.

## 4. Verify

```bash
NEW=$(cast call $DISPUTE_GAME_FACTORY 'gameImpls(uint32)(address)' 1006 --rpc-url $L1_RPC_URL)
cast call $NEW 'domainHash()(bytes32)'    --rpc-url $L1_RPC_URL   # unchanged
cast call $NEW 'laneRecipient(uint8)(address)' 0 --rpc-url $L1_RPC_URL   # new ABI present
cast call $DISPUTE_GAME_FACTORY 'initBonds(uint32)(uint256)' 1006 --rpc-url $L1_RPC_URL
```

Then watch a full proposal cycle: a game created after the swap should reach
`DEFENDER_WINS` through the normal path. Until one does, the upgrade is unproven.

## Rollback

Re-register the previous implementation. Nothing else is needed — the old address still holds
code, and games created against the new implementation keep resolving under it.

```bash
NEW_GAME_IMPLEMENTATION=$(jq -r .previousGameImplementation pkg/contracts/deployments/alphanet-proof-system.json) \
  just proof-upgrade-game alphanet
```

Roll the service images back in the same direction, since the reverted implementation speaks
the old ABI.

## Kill switch

Stops new game creation without touching games in flight:

```bash
cast send $DISPUTE_GAME_FACTORY 'setImplementation(uint32,address)' 1006 \
  0x0000000000000000000000000000000000000000 --rpc-url $L1_RPC_URL --private-key $DGF_OWNER_KEY
```

Proposals stop; existing games still resolve and pay out. Re-register an implementation to
resume.
