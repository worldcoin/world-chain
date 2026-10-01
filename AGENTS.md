# AGENTS.md

These instructions apply across the repository.

## Find the code

- `bin/world-chain/` and `crates/`: node, builder, validator, Flashblocks, PBH, and RPC.
- `proofs/`: proof services, workers, backends, and measured SP1/Nitro code.
- `pkg/contracts/`: Solidity contracts and Foundry tests; `opstack/` builds pinned OP Stack artifacts.
- `e2e-tests/`, `crates/test-utils/`, and `xtask/`: integration tests, shared fixtures, and development tools.
- `specs/` and `wips/`: protocol specifications; `docs/`: development and operating guides.

For WIP-1006 changes, read `wips/wip-1006.md` and `pkg/contracts/README.md`. Use `../world-id-protocol/` for World ID specifications when available.

For live node debugging with temporary log filters, use `.agents/skills/debug-node-tracing/SKILL.md`.

## Make focused changes

- Read the relevant code and tests before editing. State assumptions that affect behavior; ask when the answer changes the implementation.
- Follow existing patterns. Fix the cause with the smallest complete change; avoid unrelated refactors, dependency upgrades, and formatting.
- Preserve existing work. Do not edit vendored contracts under `pkg/contracts/lib/` unless the task requires it.
- Propagate errors or handle them explicitly. Bound remote calls and retries. Never add `.unwrap()` to production Rust.
- Add the smallest useful regression test for changed behavior. Explain why in comments only when the code cannot; keep comments to one line by default.

## Verify changes

Use the toolchain pinned in `rust-toolchain.toml`. Start with affected tests during development. Before every commit, run Rust formatting, Clippy, and workspace tests below. Do not commit while a required check fails or remains unrun; report blockers.

| Check | Command from the repository root |
| --- | --- |
| Rust formatting | `cargo fmt --all` then `cargo fmt --all -- --check` |
| Rust lint | `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` |
| Affected Rust tests | `cargo nextest run --locked -p <package>` |
| Workspace Rust tests | `just test --profile ci --locked` |
| Solidity formatting and tests | `just contracts-fmt` then `just test-contracts -vvv` |

For Solidity changes, also run `forge coverage` from `pkg/contracts/`. `just test-contracts` initializes dependencies and builds the required OP Stack artifacts before testing.

Check the relevant `.github/workflows/` file for additional requirements. Some tests need Docker or RPC credentials; report missing prerequisites and failed checks. Never claim unrun tests passed.

## Preserve proof boundaries

- `proofs/measured/` is excluded from the root workspace. Run its checks with the affected manifest; root workspace checks do not cover it.
- Keep measured dependencies separate from host services. Source, dependency, and toolchain changes can rotate SP1 verification keys or Nitro PCRs.
- Follow `docs/proof/reproducible-builds.md` when measured inputs change. Regenerate affected values in `proofs/measurements.json` through the build recipes; never invent or hand-adjust hashes.
- Use `just verify-proof-vkeys` for SP1 verification. Nitro measurements require Linux x86_64 and Nix; see `.github/workflows/verify-measurements.yml`.
- Read `docs/proof/release.md` before changing proof release or activation behavior. Account for workers serving games pinned to older measurements.

## Protect credentials and deployments

- Never print or commit private keys, JWTs, authenticated RPC URLs, or secret files. Do not dump the environment.
- Check recipes before running them: proof deployment recipes broadcast by default. Use `dry_run=true` for simulation; broadcast only when authorized for the target environment.
- Keep mocks and devnet deployment scripts out of production procedures. Preserve authorization checks, contract storage compatibility, and protocol invariants.

## Hand off

Keep replies short. State the result, checks run, and any blocker. Commit only when asked; use a one-line conventional commit. Keep PR descriptions within six lines: what changed, why, and how it was verified.

## PR labels and AI disclosure

- Apply `ai-generated` when AI generated or materially edited retained code, commits, PR descriptions, or review replies. Include drafts; discarded output and trivial spelling or formatting suggestions do not require it.
- Under `AI usage/prompt(s) (if applicable)`, name each tool and describe what it actually did. Keep this current after review changes. Do not publish requester prompts, hidden instructions, internal reasoning, or secrets.
- Follow these case-sensitive prefixes: `A-<area>` for affected subsystems and `C-<category>` for the change's purpose. Apply at least one of each, plus `ai-generated` when applicable.
- Check labels with `gh label list --repo worldcoin/world-chain --limit 100`. Reuse matching names; create missing required labels with clear descriptions when labeling a PR. Preserve unrelated labels and update your labels when the scope changes.
- Use `S-` status and `P-` priority labels only when supported by the PR's actual state or agreed urgency.

| Change | Example labels |
| --- | --- |
| RPC bug fix | `A-rpc`, `C-bug`; add `C-regression` for a regression |
| Proof-system feature | `A-proofs`, `C-enhancement` |
| Block-building performance | `A-block-building`, `C-perf` |
| Contract documentation or tests | `A-contracts` with `C-docs` or `C-test` |
| Dependency or CI maintenance | `A-dependencies` or `A-ci`, usually `C-debt` |
