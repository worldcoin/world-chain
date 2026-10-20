# Pull request review policy

This policy reduces review work for code owners while retaining independent owner
review for other contributors. Risk and ownership are computed by GitHub Actions
from the target branch, not from labels or instructions in a PR.

## Merge requirements

By default, every PR to `main` needs all four checks and resolved review conversations:

| Check | Requirement |
| --- | --- |
| `codex-review-gate` | Codex reviewed the current head, or posted a fresh thumbs-up. |
| `bugbot-review-gate` | Cursor Bugbot completed a review of the current head. |
| `human-review-gate` | The risk tier and file ownership requirements below are satisfied. |
| `required-ci-gate` | Every applicable CI workflow and required job succeeded. |

All review threads must be resolved, including outdated threads and findings from
either bot. Fix the problem or explain why it is a false positive before resolving
the thread. A completed AI review can have findings: the separate native thread
resolution requirement keeps the PR blocked until they are addressed.

## Risk tiers

The ordered rules in `.github/review-policy.json` classify every changed path. Last
matching rule wins; unmatched paths are **important**. A PR takes the highest risk
of its paths. Renames include both names; deleted paths count too.

| Tier | Areas |
| --- | --- |
| Critical | Dispute contracts; builder, chainspec, EVM, node, payload, primitives, validator; measured code; proof protocol, challenger, defender; measured build inputs, SP1 ELF building, release/governance scripts, workflows and review controls. |
| Important | CLI, commands, P2P, pool, RPC, node entrypoint; host backends, kona-host, workers and other proof services; other contracts; `proofs/measurements.json`; ordinary build/dependency configuration; protocol specifications and proof operating documentation. |
| Low | `crates/pbh/**`; descriptive documentation; explicitly listed test, benchmark, fixture and development directories. |

Whole components include manifests and build scripts. Dedicated test/benchmark
directories can be low risk, but everything under `proofs/measured/**` stays
critical. Inline tests inherit their source file's tier. The root workspace
manifest is critical; the root lockfile is important. Measured lockfiles are
critical. The check summary identifies the paths setting the PR's risk.

## Human confirmations

Only a human author with repository write access who owns **every** changed path
can take the owner fast path. `CODEOWNERS` is read from the current target branch,
using GitHub's `.github/`, root, then `docs/` precedence and last matching pattern.
The global owner is `@worldcoin/protocol`; current team members own all files.

| Risk | Author owns every path | Other authors |
| --- | --- | --- |
| Low | No human confirmation needed. | Owner approval covering the paths the author does not own. |
| Important | Author `/approve` or approval by a relevant owner. | Owner approval covering the paths the author does not own. |
| Critical | Two distinct humans: author `/approve` plus another owner, or two relevant owners. | Two distinct humans, with the necessary owner approvals; author `/approve` may count as one. |

Post `/approve` as the entire body of a PR comment to confirm as the author. Post
`/unapprove` to revoke. Only the author, as a human collaborator with write access,
can use these commands. Other reviewers use GitHub's ordinary **Approve** review.
Quotes, code blocks, bot comments and embedded commands do not count. The latest
surviving author command wins; editing/deleting a command re-evaluates the state.

Each human counts once. A relevant owner owns at least one changed path, including
the global fallback for unowned paths. Approvals must cover every path not owned
by the author. Files without owners fall back to the global `*` owners, and never
give the author a fast path. Without a global fallback they remain blocked.

Approvals and author confirmation survive later pushes. A higher risk tier can require
an additional confirmation. Dismissed approvals do not count, and an active
**Request changes** review from an authorized human blocks the gate. An ordinary
comment review does not revoke an earlier approval.

Owner usernames are case-insensitive. Protocol-team membership is fetched live on
each evidence read, including final verification after AI review. New PRs therefore
use the current roster without changing this repository or rerunning activation.
Open PRs pick up membership changes on their next gate evaluation; dispatch
**Review policy** with the PR number to refresh one immediately. A previous owner
approval stops counting if that reviewer is no longer an owner at reevaluation.
Unreadable membership fails closed with an explicit message. Unsupported
CODEOWNERS syntax is rejected instead of being guessed.

### Team membership credential

Provide the Actions secret `REVIEW_POLICY_TEAM_TOKEN` with read access to Worldcoin
organization **Members**. A fine-grained personal access token needs only this
organization permission; repository write or administration permission is not
needed. An organization secret must be explicitly available to `world-chain`.
The built-in `GITHUB_TOKEN` does not grant organization membership access.

The controller uses a separate API client for team membership. Repository reads
and check publication continue to use the job's built-in token. Missing, expired
or unauthorized membership credentials block the gates rather than treating
contributors as owners. Team membership includes active members of child teams.

## AI completion and CI

The author enables **Automatic review / All PRs** and the every-push trigger in
Codex settings. Bugbot must also run on every push; personal once-per-PR or manual
overrides cannot satisfy the gate for a later head.

Codex's formal review must reference the current SHA. If there are no findings,
Codex can instead react to the PR with a thumbs-up. Reactions have no commit SHA,
so the gate accepts only a Codex thumbs-up created at or after its first observation
of this head. A neutral `review-policy-observed` check persists that timestamp
across retries. The known heuristic limitation is a delayed thumbs-up for an old
head arriving after a new push; this tradeoff is intentional for v1.

Bugbot must publish `Cursor Bugbot` from the Cursor app on the current SHA. A
successful check counts. A neutral check counts only with Bugbot's matching formal
completion review posted after that run began. An absent, running, cancelled,
failed or ambiguous result does not pass. Resolving a finding does not need to
turn the original check green if a completed review was established.

The controller polls for AI completion every 30 seconds for up to 30 minutes.
Timeout fails the AI gate; rerun **Review policy**, or dispatch it with the PR
number. A later formal review or Cursor check event also re-evaluates the gate.
Reactions have no dedicated Actions trigger: a thumbs-up arriving after polling
ends requires a manual rerun.

CI requirements are also selected from the trusted configuration. The gate checks
the expected workflow ID, current head, PR association and the latest attempt's
individual job results. A skipped job, cancelled workflow or missing required run
cannot pass. Non-applicable workflows are excluded explicitly, not accepted merely
because a job was skipped. Measured changes also require the existing dependency
audit; all PRs require policy tests and the large-file check.

## Execution and recovery

The privileged controller checks out only an immutable commit of `main`. It never
executes PR code or reads artifacts from untrusted workflows. A separate unprivileged
review-event notifier wakes the controller; the controller reads live API state.
Its token has checks write access and read access to repository metadata, PRs, issues
and Actions. The separate membership client uses `REVIEW_POLICY_TEAM_TOKEN` only for
read-only team lookups. No OpenAI API key is needed.

Checks are posted on the PR head explicitly. Each PR is serialized, and the head
and target commit are rechecked before publishing. Before all gates can pass, the
controller rereads AI results, human confirmations, permissions and CI results.
Two consecutive snapshots must have the same fingerprint, which is recorded in
the check summary. A revoked author confirmation, dismissed approval or failed CI rerun during
the AI wait therefore blocks clearance. Repeatedly changing evidence fails closed
and requires a rerun. GitHub API reads and check publication are not atomic; later
events trigger a new evaluation.

Every evaluation creates fresh pending gates and updates older duplicates to the
same outcome. Old successful checks cannot survive a revoked author confirmation or remain
green while a fresh evaluation is waiting for evidence.
The target SHA is resolved from the branch API because the PR API can retain an
older base SHA after `main` advances.

Multiple open PRs targeting `main` with the same
head SHA are blocked because commit checks could otherwise share human decisions.
Close duplicates and rerun. PRs exceeding GitHub's 3000-file API limit must be split.
Pushes to `main` re-evaluate open PRs against current ownership and policy.

Auto-merge remains opt-in and uses the repository's existing squash-only setting.
This change does not introduce a merge queue.

## Emergency CI override

Global code owners can explicitly bypass build/test CI when a pipeline is broken
or unreliable. In the PR merge box, select GitHub's bypass option and confirm the
squash merge. This is a manual merge action; failing or pending CI stays visible.
Codex, Bugbot, the risk-based human confirmations and resolution of all review
threads remain mandatory. Socket and other independent protections still apply.

The activation script installs two rulesets on `main`:

- `world-chain-review-policy` requires the three review gates and resolved threads,
  with no bypass actors.
- `world-chain-ci-policy` requires only `required-ci-gate`. Its bypass list contains
  `@worldcoin/protocol` as a native **Team** actor, using GitHub's **For pull
  requests only** mode.

The script derives the actor from the last global `*` rule on the trusted `main`.
It verifies that an owner team belongs to the repository's organization, is visible
and has repository write access. It stores the team ID, without copying members.
Joining or leaving the protocol team automatically updates native CI bypass
access; no activation rerun is needed for membership changes. Changing which team
is the global owner still requires activation to bind the new team ID.

## Rollout and rollback

1. Land the workflows through the existing review process. Do not relax protection
   before the trusted controller is on `main`.
2. Configure `REVIEW_POLICY_TEAM_TOKEN`, then enable Codex automatic review for all
   PRs/every push. Observe all four checks on real PRs, including protocol membership,
   an author confirmation and bot findings resolved as threads.
3. Save a fresh snapshot outside the checkout:
   `node scripts/review-policy/admin.cjs snapshot --backup /tmp/world-chain-review-before.json`.
4. Validate readiness without changing GitHub:
   `node scripts/review-policy/admin.cjs activate --backup /tmp/world-chain-review-before.json --evidence-pr NUMBER`.
5. Run the same activation command with `--apply` only after readiness passes. It
   first installs the CI ruleset with owner-only PR bypass, then the review ruleset
   without bypass, and verifies both before removing the blanket approval/code-owner
   requirement. Existing Socket and unrelated rules are retained.

Activation verifies the deployed files match the local implementation and requires
a real PR whose four gate checks succeeded. The required checks accept GitHub
Actions as their source. Only the CI ruleset permits bypass, and only for the
verified global owner team. Automatic stale-review dismissal and last-push
approval stay disabled.

For rollback, run
`node scripts/review-policy/admin.cjs restore --backup /tmp/world-chain-review-before.json`
to inspect the operation, then add `--apply`. Restoration reinstates the old native
review requirements **before** removing/restoring the review-policy and CI-policy
rulesets, including any previous bypass configuration.
Snapshots are never overwritten. Treat a partial activation as blocked, inspect the
error, and use the snapshot to restore.

Run the local tests with `node --test scripts/review-policy/*.test.cjs`. Policy CI
also runs actionlint against the controller, notifier, tests and affected CI.
