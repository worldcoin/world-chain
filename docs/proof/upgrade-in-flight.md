# In-flight measurement upgrade (vkeys + PCRs)

How to move a live deployment to a proof release with new measurements while games are in
flight. Performed for alphanet `proofs/v1.0.0-rc.1` on 2026-08-25; every command below is the
one actually run, with alphanet's addresses. Adjust addresses per environment from
`pkg/contracts/deployments/`.

The model is blue-green at the game-implementation boundary: games pin their measurements at
creation, workers lease only games matching their own identity (`verifier_id` routing in the
prover-service), so both generations run in parallel until the old one drains. Nothing is
mutated in flight — the "upgrade" is admitting a new generation and letting the old one end.

Key handling: the contract admin key stays in 1Password (`op://devnet/devnet-admin/private_key`).
Read it into a variable at call time; never echo it, never write it to a file.

## Phase 0 — prerequisites

1. The release ran to completion: `verify reproducible` passed, images + signed
   `manifest.json` published. Get the digests from the manifest — never from a mutable tag.
2. The deployed prover-service and workers already speak `verifier_id` routing
   (`GetNextProofRequest.verifier_id`). Anything older cannot coexist; upgrade it first.
3. Compute the on-chain identifiers from the release measurements:

   ```bash
   # keccak256 of each raw 48-byte PCR — the registry's hashing convention
   cast keccak <pcr0>   # also the new game's TEE_IMAGE_ID
   cast keccak <pcr1>
   cast keccak <pcr2>
   ```

## Phase 1 — on-chain admission (additive, no behavior change)

4. Approve the new PCR set on the `NitroAttestationVerifier`
   (`0xbabc1a3B2b239C3b73008a8f8B7D2800e40911BA`):

   ```bash
   PK=$(op read "op://devnet/devnet-admin/private_key")
   cast send 0xbabc1a3B2b239C3b73008a8f8B7D2800e40911BA \
     "approvePCRSet(bytes32,bytes32,bytes32)" <k0> <k1> <k2> \
     --private-key "$PK" --rpc-url "$SEPOLIA_RPC"
   ```

   Verify both generations are admitted (`isPCRSetApproved` → `true` for old and new).
   Done for rc.1 in tx `0x9246b46eae555019952121ea65fa473547c41dd07ae92198e5b4ba8a6c452b1c`.
5. Merge the release's allowlist PR (rc.1: world-chain#1083) — only after step 4 lands; the
   deployments file mirrors on-chain state.

## Phase 2 — parallel worker generation (crypto-apps)

6. Add the new-generation workers alongside the old ones; touch nothing else. Done for rc.1
   in crypto-apps#881:
   - New Argo apps `…-sp1-worker-rc1` / `…-nitro-worker-rc1` in `bootstrap/values.yaml`,
     sharing the old apps' namespaces (secret reuse) and differentiated by
     `fullnameOverride`.
   - Images pinned by the release-manifest digests (`imageDigest`, not a tag) in
     `clusters/<cluster>/values-cluster-apps-bootstrap.yaml`.
   - The nitro rc-values are a full copy of the old generation's with the EIF sidecar
     repointed (`world-chain-nitro-enclave-eif@<manifest digest>`) — helm replaces lists, so
     the sidecar block cannot be partially overridden.
7. Merge, let Argo sync, then verify before proceeding:
   - Both worker generations Running; old workers still leasing old-generation jobs.
   - The nitro rc worker registered its enclave key (`AUTO_REGISTER=true` self-registration —
     requires Phase 1's approval; a rejected registration here means a PCR/keccak mismatch).
   - Proposer working capital ≥ ~1.5–2 ETH (bond refunds lag ~24h).

## Phase 3 — activate the new generation (the cutover)

8. Deploy + register the new `MultiProofGame` implementation. Registration on the factory IS
   the cutover for new games — run this only after Phase 2 verifies. All config comes from
   `pkg/contracts/deployments/alphanet-proof-system.json` except the three new measurement
   values (from `proofs/measurements.json`) and `TEE_IMAGE_ID` (= `keccak256(pcr0)`):

   ```bash
   cd pkg/contracts
   PK=$(op read "op://devnet/devnet-admin/private_key")
   PRIVATE_KEY=$PK DGF_OWNER_KEY=$PK OP_CHAIN_PROXY_ADMIN_OWNER_PRIVATE_KEY=$PK \
   WORLD_CHAIN_L2_CHAIN_ID=5496749 \
   ROLLUP_CONFIG_HASH=0x6327a7390749c89670cbd5530c689eed8289590b6fccd22dcd3a0a0c8c0a5c9b \
   AGGREGATION_VKEY=0x00e9ee2c9771b4a3596809af947148b803be6c9989bc98dded1c185ebffe18c9 \
   RANGE_VKEY_COMMITMENT=0x729e953b5968b34c75ab76a94e819fdb4000c24422f695181a1e29e522d13fa0 \
   TEE_IMAGE_ID=0x8a8a069637d5bf25f817d05d211e1c943cd122c38d3b554ae2d87153cd515ff1 \
   VALIDITY_PROOF_VERIFIER=0x1DAB10b47703e775720Af3a845E9d94723A0c241 \
   TEE_VERIFIER=0x1E21Ac0cdC246ED3A6f705d2a1EE923d02213649 \
   SECURITY_COUNCIL_VERIFIER=0x9Ea4EF54aF8eBC385D915FF60B27a614EC942674 \
   DISPUTE_GAME_FACTORY=0x1f01418A80F67850ecc4d2cAd238654eF2266451 \
   ANCHOR_STATE_REGISTRY=0xc9971FAeabF36c8C027Bef7De99A0c3495a3fD91 \
   SYSTEM_CONFIG=0x2F712710390eE73C065ec198C236061f9de89C83 \
   OP_CHAIN_PROXY_ADMIN=0xf3F239ef9aF0af0A5B18DEa1cCfB4e1460fd9Fb0 \
   PROTOCOL_FEE_RECIPIENT=0x629D8810D0177cB7f2315272F8237E68A8E2017f \
   PROOF_THRESHOLD=2 \
   forge script scripts/devnet/DeployProofSystem.s.sol --rpc-url "$SEPOLIA_RPC" --broadcast
   ```

   The script deploys a generation-scoped DelayedWETH, deploys the game implementation, and
   `setImplementation` + `setInitBond` on the factory. It refuses to finish inconsistent.
9. Kill switch and rollback: `setImplementation(MULTI_PROOF_GAME_TYPE, address(0))` stops new
   game creation; re-registering the previous implementation address restores the old
   generation for new games. Neither touches in-flight games.
10. Verify: the next proposed game reports the new implementation and measurements; rc
    workers lease its proof requests; old workers keep proving old games only.
11. Commit the fresh `deployments/alphanet-proof-system.json` the script wrote.

## Phase 4 — drain and retire

12. Old-generation games resolve on their own schedule (challenge period + proof period).
    When none remain unresolved and bonds are claimed, remove the old worker apps from
    crypto-apps.
13. Optionally `revokePCRSet` on the old triple. Revocation is admission control only: it
    blocks new key registrations, it does not deactivate already-registered signers or
    resolve-in-flight games (see `NitroAttestationVerifier.revokePCRSet` docs).
14. If any coordinator (prover-service, proposer, challenger, defender) is due a version
    bump, do it now, after the cutover has proved out — they are generation-agnostic, so
    this is a routine rollout, deliberately not batched with the cutover.

## Failure modes worth knowing

- Nitro registration: a stale certificate cache manifests as "inverse hint underflow"; a pod
  on a non-release EIF drifts off the allowlist; self-registration retries rather than
  crashing, so a persistent registration failure is a config problem, not a transient.
- The pod annotation `worldcoin.org/rollup-config-hash` is a cache-buster for the config
  download, not the game's `ROLLUP_CONFIG_HASH` — do not copy one into the other.
- A `verifier_id` mismatch shows up as one generation's queue simply never draining, not as
  an error.
