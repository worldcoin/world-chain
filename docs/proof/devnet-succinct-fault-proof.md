# World OP Succinct Lite devnet rollout

This is the shipping plan for the World Chain zk fault proof devnet deployment. It follows OP
Succinct Lite's architecture: existing OP Stack services keep producing the chain, while separate
Succinct proposer and challenger services participate in a new fault dispute game type.

## What can stay vanilla

- `op-geth`, `op-node`, and `op-batcher` can stay on the normal devnet deployment path.
- The OP Succinct challenger can be vanilla if the deployed game contract ABI remains
  `OPSuccinctFaultDisputeGame`. The challenger does not generate proofs; it watches games, compares
  proposed output roots against the local L2 RPC, challenges invalid games, resolves games, and
  claims bonds.

## What must be World-specific

- The proposer cannot be the vanilla OP Succinct image. Upstream embeds upstream ELFs, computes the
  upstream rollup config hash, and builds the upstream witness. World needs:
  - `world-chain-range-ethereum` as the range ELF.
  - `world-chain-aggregation` as the aggregation ELF.
  - Rollup config hashing that appends World-only `tropo_time` and `strato_time` to Kona's
    `RollupConfig` before SHA-256 hashing.
  - Witness generation that carries the World hardfork schedule into the range guest.
- The deployed fault dispute game config must use the World aggregation vkey, World range vkey
  commitment, and World rollup config hash. If any of these differ from the proposer, the proposer
  will classify the games as foreign and must not prove them.

## Contract deploy inputs

Generate these from the exact ELFs and rollup config that will run on devnet:

- `aggregation_vkey`: SP1 aggregation verifying key for `world-chain-aggregation`.
- `range_vkey_commitment`: SP1 range verifying key commitment for `world-chain-range-ethereum`.
- `rollup_config_hash`: pretty-JSON SHA-256 of Kona `RollupConfig` plus World `tropo_time` and
  `strato_time`.
- `starting_l2_block_number` and `starting_root`: anchor state for the new game.
- `game_type`: use the OP Succinct Lite game type selected for devnet, expected to be `42` unless
  the devnet factory reserves another type.
- `verifier_address`: SP1 verifier gateway on the devnet L1, or the mock verifier only for a
  throwaway devnet.
- `proposer_addresses` and `challenger_addresses`: addresses funded on Sepolia and backed by
  Kubernetes secrets.

Current ELF identity values:

- `aggregation_vkey`: `0x0024d9242b5308dd205220f33ac1196131904ceb7aa3f95d0d788c0b3dd40fd6`
- `range_vkey_commitment`: `0x00821da4d0ba868e5eaa4fd2d6c486161b7bfc0ce3d0644ce79d3317f4f94c50`

The current devnet `rollup.json` only contains the Jovian schedule. For a Tropo/Strato-compatible
deployment, do not deploy the contracts until the final `tropo_time` and `strato_time` are known
and included in the rollup config hash used by both the contracts and proposer.

## crypto-apps rollout

Use separate apps instead of replacing the existing Cannon challenger first:

1. Add `world-chain-zk-proposer` in `../crypto-apps`.
   - Image: custom World OP Succinct proposer image.
   - Required env: `L1_RPC`, `L2_RPC`, `ANCHOR_STATE_REGISTRY_ADDRESS`, `FACTORY_ADDRESS`,
     `GAME_TYPE`, `PROPOSAL_INTERVAL_IN_BLOCKS`, `FETCH_INTERVAL`, `SAFE_DB_FALLBACK`,
     `RANGE_SPLIT_COUNT`, proof provider env, and metrics port.
   - Secret: proposer private key, or KMS requester env if using KMS.
2. Add `world-chain-zk-challenger` in `../crypto-apps`.
   - Image: vanilla OP Succinct Lite challenger is acceptable if we keep the upstream game ABI.
   - Required env: `L1_RPC`, `L2_RPC`, `ANCHOR_STATE_REGISTRY_ADDRESS`, `FACTORY_ADDRESS`,
     `GAME_TYPE`, `FETCH_INTERVAL`, and metrics port.
   - Secret: challenger private key, or KMS requester env if using KMS.
3. Keep existing `world-chain-challenger` and `op-proposer` running until the zk game type is
   deployed, funded, and producing valid games.
4. Enable the zk apps only after contract deployment writes the final ASR/factory addresses into
   `values-dev-crypto-dev-us-east-1.yaml`.
5. Verify in Argo/CD:
   - proposer creates games for the zk game type;
   - challenger sees the same game tree;
   - proposer proves and resolves at least one game;
   - challenger stays idle for valid games and can challenge a forced-invalid game on a throwaway
     deployment.

## Service work remaining

Copy OP Succinct's fault-proof proposer implementation rather than writing a new lifecycle from
scratch. Keep the patch narrow:

- Replace embedded ELF imports with `world-chain-proof-succinct-elfs`.
- Replace upstream `hash_rollup_config(fetcher.rollup_config)` with the World hash helper that
  includes `tropo_time` and `strato_time`.
- Replace upstream ETH witness generation with the World ETH witness data that carries the schedule.
- Keep OP Succinct's proposer state machine, game sync, proof submission, backup, metrics, and
  transaction handling unchanged.

The challenger should be left vanilla unless the contract ABI changes.

## References

- OP Succinct overview: https://succinctlabs.github.io/op-succinct/
- OP Succinct fault proof architecture:
  https://succinctlabs.github.io/op-succinct/fault_proofs/fault_proof_architecture.html
- OP Succinct proposer:
  https://succinctlabs.github.io/op-succinct/fault_proofs/proposer.html
- OP Succinct deploy guide:
  https://succinctlabs.github.io/op-succinct/fault_proofs/deploy.html
