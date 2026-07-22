# Release lifecycle

How a world-chain commit gets from `main` to Mainnet. Three manual workflows in
this repo drive the cycle; every deployment itself happens by merging an
auto-opened PR in [crypto-apps](https://github.com/worldcoin/crypto-apps)
(Argo CD syncs whatever is on `main` there).

Detailed operational steps live in the
[Release Runbook](https://app.notion.com/p/3568614bdf8c80afa4a3de10fb9f1fc7).

## The cycle at a glance

```
main (every merge builds an immutable sha-* image, soaks on Alphanet/Betanet)
  │
  │ 1. prepare-release.yml   (source_sha, version X.Y.Z)
  ▼
release/vX.Y branch + image ghcr.io/worldcoin/world-chain:sha-<short>
  │
  │ 2. promote-release.yml   (environment=dev, version=sha-<short>)
  ▼
crypto-apps PR → merge → Alchemy Sepolia (dev)          ── soak ──
  │
  │ 3. finalize-release.yml  (version X.Y.Z)
  ▼
image re-tagged vX.Y.Z (no rebuild) + git tag vX.Y.Z + GitHub release draft
  │
  │ 4. promote-release.yml   (environment=stage, version=vX.Y.Z)
  ▼
crypto-apps PR → merge → World Chain Sepolia (stage)    ── soak ──
  │
  │ 5. promote-release.yml   (environment=prod, version=vX.Y.Z)
  ▼
crypto-apps PR (prod label) → merge → World Chain Mainnet
```

## Step by step

1. **Prepare a candidate — `prepare-release.yml`** (manual, gated by the
   `release` GitHub environment). Give it a full 40-char commit SHA on `main`
   that has already passed Alphanet/Betanet soak and acceptance tests, plus the
   target version `X.Y.Z`. It cuts (or, with `replace_prepared_release=true`,
   refreshes) the `release/vX.Y` branch, commits a `Cargo.toml` version bump if
   needed, and publishes the immutable multi-arch image
   `sha-<short>` for that exact branch head. Nothing is deployed yet.

2. **Deploy the candidate to dev — `promote-release.yml`** with
   `environment=dev`, `version=sha-<short>`. It verifies the tag exists in
   ghcr, then opens a PR in crypto-apps that pins the dev cluster's
   world-chain apps to that tag and re-renders the manifests. **Merging that
   PR is the deployment.** The PR's own CI is the safety net:
   `image-tag-promotion-gate` (promotion order), `verify-rendered-templates`,
   `manifest-diff`, `codex-review-gate`, and `datadog-monitor-gate`. The
   Notion tracker row for Alchemy Sepolia is set to "Rolling out".

3. **Soak, then finalize — `finalize-release.yml`** (gated by the `release`
   environment). Once the candidate has soaked on Alchemy Sepolia and its
   Notion row says "Deployed" with the matching tag (checked automatically;
   `skip_soak_check=true` is a documented incident override), it re-tags the
   **exact** `sha-<short>` manifest as `vX.Y.Z` and `latest`
   (`docker buildx imagetools create` — no rebuild, byte-for-byte the image
   that soaked) and pushes the `vX.Y.Z` git tag. The tag push triggers
   `release.yml`, which builds the signed binaries and drafts the GitHub
   release; it does not touch container images.

4. **Promote to stage — `promote-release.yml`** with `environment=stage`,
   `version=vX.Y.Z`. Same mechanics as dev: a crypto-apps PR against
   `env/stage/values.yaml`. The promotion gate enforces stage never gets ahead
   of what dev has validated.

5. **Promote to prod — `promote-release.yml`** with `environment=prod`,
   `version=vX.Y.Z`. The PR carries the `prod` label (required by
   crypto-apps' `prod-gate` for any rendered prod change), and the promotion
   gate enforces **prod == stage** — you can only ship to Mainnet exactly what
   stage is running.

## Rollback

There is no separate hotfix path: run `promote-release.yml` again for the
affected environment with a prior, already-validated version. The same gates
apply.

## Invariants

- **One artifact.** The image validated on dev is the image that ships to
  prod; `vX.Y.Z` is a re-tag of `sha-<short>`, never a rebuild.
- **Order is enforced, not remembered.** dev → stage → prod ordering is
  checked by crypto-apps CI on every promotion PR, not by convention.
- **Deploys are PRs.** Every environment change is a reviewable, revertible
  crypto-apps commit; this repo's workflows never touch a cluster directly.
- **Immutable inputs only.** Promotions accept `sha-*` (dev) or `vX.Y.Z`
  (stage/prod) tags — never mutable tags like `nightly` or `alphanet`.

## Configuration and tracking

- The environment map (values file, apps, expected tag shape, Notion row) is
  `.github/release-environments.json` in crypto-apps — the single source of
  truth used by both repos.
- The Notion "Release Deployment Tracker" is updated to "Rolling out"
  automatically on promotion. Moving a row to "Deployed" is currently manual
  (a reconciler that verifies the cluster is planned); finalize's soak check
  depends on that row being accurate.
