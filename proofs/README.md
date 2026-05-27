# World Chain Proofs

This directory is the root-level home for World Chain proof-system offchain components.

The first migration slice keeps only the pieces needed to test the WIP-1006 lifecycle end to end:

- proof contracts live in the existing Foundry package at `pkg/contracts/src/proofs`;
- mocked proof lane verifiers and staking live under `pkg/contracts/test/proofs`;
- the e2e contract test exercises proofless proposal, staked challenge, two distinct supporting lanes, duplicate-lane rejection, invalidity, and unchallenged finality.

Run the current slice with:

```sh
cd pkg/contracts
forge test --match-contract ProofSystemGameTest
```

Next components should land here once the contract surface is stable:

- `proposer`: proofless OP/Base-compatible root proposal service;
- `challenger`: watcher that recomputes roots and submits proofless challenges;
- `orchestrator`: challenged-root lane job coordination;
- `zk`: validity proof service adapter;
- `tee`: TEE attestation service adapter;
- `registrar`: TEE signer/image registration.
