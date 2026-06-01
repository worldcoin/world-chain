# CI fork-compatibility manifests

Manifests consumed by [`.github/workflows/devnet-acceptance.yml`](../../.github/workflows/devnet-acceptance.yml).

Each file commits to one hardfork plus a feature set. The workflow spawns an
in-process devnet derived from the manifest (`xtask acceptance --target
spawned`) and holds it to exactly those commitments, so a passing run asserts the
network is compatible with the forks and features it claims. This mirrors
Optimism's `op-acceptance-tests`: one suite, fanned out over deployment variants,
behind a single required status check.

These are kept separate from the deployment manifests (`manifests/devnet.toml`,
`manifests/alphanet.toml`) so the CI matrix can grow without touching what the
devnet and alphanet commit to.

## Matrix

| Manifest | Hardfork | Features | Gating? | Notes |
|----------|----------|----------|---------|-------|
| `jovian.toml` | `jovian` | flashblocks, block_access_list | **yes** | Live World Chain hardfork; spawns from the built-in `dev` spec. |
| `tropo.toml` | `tropo` | flashblocks, block_access_list | wired (dormant) | First World Chain specific fork after Jovian. `dev` doesn't schedule it, so it commits to `tropo.genesis.json` (sets `tropoTime`). Spawnable — flip `run: true` in the workflow to gate on it. |
| `karst.toml` | `karst` | flashblocks, block_access_list | wired (dormant) | See limitation below. |

## Adding a fork or feature

1. Add a `<fork>.toml` here (and a `<fork>.genesis.json` if the `dev` spec
   doesn't schedule the fork — see below).
2. Add a matrix row in `devnet-acceptance.yml` pointing at it.
3. Confirm it loads: `cargo xtask acceptance --manifest manifests/ci/<fork>.toml
   --target spawned --preset minimal`.

## Spawned-devnet constraints

The spawned target derives its node config from
[`WorldChainHardforkConfig`](../../crates/devnet/src/hardforks.rs), which only
knows the `WorldChainHardfork` enum (`Bedrock`…`Jovian`, then the World Chain
specific `Tropo`, `Strato`). Two consequences:

- **Forks the `dev` spec doesn't schedule need a genesis file.** The built-in
  `dev` spec schedules through Jovian (and recognises Karst), but not Tropo /
  Strato. Manifests for those commit to a genesis that sets the fork's `*Time`
  field (`tropoTime`, `stratoTime`). `karstTime` is **not** parsed by
  `WorldChainSpec::from_genesis`, so Karst can only be committed via `spec =
  "dev"`, not via a genesis file.

- **Karst is not spawnable.** `Karst` is an `OpHardfork`, not a
  `WorldChainHardfork`, so `xtask acceptance --target spawned` rejects a Karst
  manifest with *"spawned devnet supports World Chain hardforks only"*
  ([`xtask/src/acceptance.rs`](../../xtask/src/acceptance.rs), `hardfork_config`).
  Until the devnet hardfork config maps Karst, the `karst` cell can only run with
  `--target alphanet` against a deployed network.

  Note the genesis-based Tropo cell does **not** cover Karst: since `karstTime`
  is unparsable, `tropo.genesis.json` cannot schedule Karst, so the cumulative
  `karst/` checks skip there (`manifest does not commit to: hardfork:karst`).
  Karst can only be committed via `spec = "dev"`, which in turn cannot commit to
  Tropo — so the two cannot currently be asserted by a single spawned cell.
