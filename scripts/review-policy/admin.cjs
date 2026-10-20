'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { isDeepStrictEqual } = require('node:util');
const { GATES, human } = require('./policy.cjs');
const { readCodeowners } = require('./github.cjs');

const REPO = 'worldcoin/world-chain';
const RULESET = 'world-chain-review-policy';
const CI_RULESET = 'world-chain-ci-policy';
const REVIEW_GATES = GATES.filter((gate) => gate !== 'required-ci-gate');

function request(method, path, body) {
  try {
    return JSON.parse(execFileSync('gh', ['api', `repos/${REPO}/${path}`, '--method', method, ...(body === undefined ? [] : ['--input', '-'])], {
      input: body === undefined ? undefined : JSON.stringify(body), encoding: 'utf8', stdio: 'pipe', timeout: 30000, maxBuffer: 16000000,
    }) || 'null');
  } catch (error) {
    error.status = Number(String(error.stderr || '').match(/\(HTTP (\d{3})\)/)?.[1]) || undefined;
    throw error;
  }
}

function list(path, key) {
  const result = [];
  for (let page = 1; page <= 100; page++) {
    const data = request('GET', `${path}${path.includes('?') ? '&' : '?'}per_page=100&page=${page}`);
    const items = key ? data[key] : data;
    result.push(...items);
    if (items.length < 100) return result;
  }
  throw new Error(`Pagination limit: ${path}`);
}

function reviewSettings(protection) {
  const reviews = protection.required_pull_request_reviews;
  if (!reviews) throw new Error('Existing PR review protection is missing; inspect manually before changing rules.');
  return Object.fromEntries(['required_approving_review_count', 'require_code_owner_reviews', 'dismiss_stale_reviews', 'require_last_push_approval'].map((key) => [key, reviews[key]]));
}

function requiredChecks(gates) {
  return { type: 'required_status_checks', parameters: {
    strict_required_status_checks_policy: false, do_not_enforce_on_create: false,
    required_status_checks: gates.map((context) => ({ context, integration_id: 15368 })),
  } };
}

function rulesetBody() {
  return {
    name: RULESET, target: 'branch', enforcement: 'active', bypass_actors: [],
    conditions: { ref_name: { include: ['refs/heads/main'], exclude: [] } },
    rules: [
      requiredChecks(REVIEW_GATES),
      { type: 'pull_request', parameters: {
        required_approving_review_count: 0, require_code_owner_review: false,
        dismiss_stale_reviews_on_push: false, require_last_push_approval: false,
        required_review_thread_resolution: true, allowed_merge_methods: ['squash'],
      } },
    ],
  };
}

function ciRulesetBody(bypassActors) {
  return {
    name: CI_RULESET, target: 'branch', enforcement: 'active', bypass_actors: bypassActors,
    conditions: { ref_name: { include: ['refs/heads/main'], exclude: [] } },
    rules: [requiredChecks(['required-ci-gate'])],
  };
}

async function ciBypassActors(request, ref) {
  const rules = await readCodeowners({
    async content(path, sha) {
      const file = request('GET', `contents/${path}?ref=${sha}`);
      if (file.type !== 'file' || file.encoding !== 'base64' || file.size > 1000000) throw new Error(`Cannot read trusted ${path}`);
      return Buffer.from(file.content, 'base64').toString('utf8');
    },
  }, ref);
  const owners = [...new Set([...rules].reverse().find((rule) => rule.pattern === '*')?.owners || [])];
  if (!owners.length) throw new Error('CI bypass requires a nonempty global * CODEOWNERS rule on main.');
  const actors = [];
  for (const owner of owners) {
    if (owner.includes('/')) throw new Error(`CI bypass requires individual global code owners; cannot expand ${owner}.`);
    const login = owner.slice(1);
    const result = request('GET', `collaborators/${login}/permission`);
    if (!human(result.user) || !Number.isSafeInteger(result.user.id) || result.user.id < 1
      || result.user.login.toLowerCase() !== login
      || !(['admin', 'maintain', 'write'].includes(result.permission) || result.user.permissions?.push)) {
      throw new Error(`CI bypass owner ${owner} must be a human collaborator with write access.`);
    }
    actors.push({ actor_id: result.user.id, actor_type: 'User', bypass_mode: 'pull_request' });
  }
  return actors;
}

function verifyRuleset(actual, expected) {
  const actors = (items) => items?.map(({ actor_id, actor_type, bypass_mode }) => ({ actor_id, actor_type, bypass_mode }))
    .sort((a, b) => a.actor_id - b.actor_id);
  const checks = (rules) => rules?.find((rule) => rule.type === 'required_status_checks')?.parameters?.required_status_checks
    ?.map(({ context, integration_id }) => ({ context, integration_id })).sort((a, b) => a.context.localeCompare(b.context));
  const pull = actual.rules?.find((rule) => rule.type === 'pull_request')?.parameters;
  const expectedPull = expected.rules.find((rule) => rule.type === 'pull_request')?.parameters;
  if (actual.name !== expected.name || actual.target !== 'branch' || actual.enforcement !== 'active'
    || !isDeepStrictEqual(actual.conditions, expected.conditions)
    || !isDeepStrictEqual(actors(actual.bypass_actors), actors(expected.bypass_actors))
    || !isDeepStrictEqual(checks(actual.rules), checks(expected.rules))
    || actual.rules?.length !== expected.rules.length
    || (expectedPull && Object.entries(expectedPull).some(([key, value]) => !isDeepStrictEqual(pull?.[key], value)))) {
    throw new Error(`Ruleset ${expected.name} was not installed as expected; keeping the native review requirement.`);
  }
}

async function main(args = process.argv.slice(2), api = { request, list }, log = console.log) {
  const { request, list } = api;
  const [command, ...options] = args;
  const option = (name) => { const i = options.indexOf(name); return i < 0 ? undefined : options[i + 1]; };
  const apply = options.includes('--apply');
  const backupPath = option('--backup');
  if (!['snapshot', 'activate', 'restore'].includes(command) || !backupPath) {
    throw new Error('Usage: node scripts/review-policy/admin.cjs snapshot|activate|restore --backup /absolute/file.json [--evidence-pr N] [--apply]');
  }
  if (!path.isAbsolute(backupPath)) throw new Error('--backup must be an absolute path outside the checkout.');
  const relative = path.relative(process.cwd(), backupPath);
  if (!relative.startsWith(`..${path.sep}`) && relative !== '..') throw new Error('Save the backup outside the checkout.');
  if (command === 'snapshot') {
    const protection = request('GET', 'branches/main/protection');
    const rulesets = list('rulesets');
    const ownRulesets = rulesets.filter((rule) => rule.source_type === 'Repository').map((rule) => request('GET', `rulesets/${rule.id}`));
    fs.writeFileSync(backupPath, JSON.stringify({ repo: REPO, takenAt: new Date().toISOString(), protection, rulesets, ownRulesets }, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
    log(`Saved protection snapshot to ${backupPath}; GitHub settings unchanged.`);
    return;
  }
  const backup = JSON.parse(fs.readFileSync(backupPath, 'utf8'));
  if (backup.repo !== REPO) throw new Error('Snapshot repository mismatch.');
  const current = list('rulesets').filter((rule) => rule.source_type === 'Repository');
  if (command === 'restore') {
    log('Restore the previous native review requirements first, then restore/remove only the review-policy and CI-policy rulesets. Socket and unrelated rules remain untouched.');
    if (!apply) { log('Dry run. Add --apply to restore.'); return; }
    request('PATCH', 'branches/main/protection/required_pull_request_reviews', reviewSettings(backup.protection));
    for (const name of [RULESET, CI_RULESET]) {
      const previous = backup.ownRulesets.find((rule) => rule.name === name);
      const installed = current.find((rule) => rule.name === name);
      if (previous) {
        const { target, enforcement, conditions, rules, bypass_actors } = previous;
        request(installed ? 'PUT' : 'POST', installed ? `rulesets/${installed.id}` : 'rulesets', { name, target, enforcement, conditions, rules, bypass_actors });
      } else if (installed) request('DELETE', `rulesets/${installed.id}`);
    }
    log('Previous review protection restored.');
    return;
  }
  const evidence = Number(option('--evidence-pr'));
  if (!Number.isSafeInteger(evidence) || evidence < 1) throw new Error('--evidence-pr must identify a real PR with all four gates successful.');
  const main = request('GET', 'branches/main').commit.sha;
  for (const path of [
    '.github/review-policy.json', '.github/workflows/review-policy.yml', '.github/workflows/review-policy-events.yml',
    '.github/workflows/review-policy-tests.yml', 'scripts/review-policy/policy.cjs', 'scripts/review-policy/github.cjs',
    'scripts/review-policy/admin.cjs', 'scripts/review-policy/policy.test.cjs', 'scripts/review-policy/github.test.cjs', 'scripts/review-policy/admin.test.cjs',
    '.github/actionlint.yaml', '.github/workflows/rust-ci.yml', '.github/workflows/foundry.yml', '.github/workflows/verify-measurements.yml',
  ]) {
    const remote = request('GET', `contents/${path}?ref=${main}`);
    if (Buffer.from(remote.content || '', 'base64').toString('utf8') !== fs.readFileSync(path, 'utf8')) {
      throw new Error(`${path} is not deployed identically on main; activation refused.`);
    }
  }
  const pr = request('GET', `pulls/${evidence}`);
  if (pr.base.ref !== 'main') throw new Error('Evidence PR must target main.');
  const checks = list(`commits/${pr.head.sha}/check-runs`, 'check_runs');
  for (const name of GATES) {
    const result = checks.find((check) => check.name === name && check.app?.slug === 'github-actions'
      && check.external_id === `review-policy:${REPO}:${evidence}:${pr.head.sha}`);
    if (result?.status !== 'completed' || result.conclusion !== 'success') throw new Error(`Evidence gate ${name} is not successful on the PR head.`);
    if (!result.output?.summary?.includes(`base ${main};`)) throw new Error(`Evidence gate ${name} used an older target policy; re-evaluate the PR.`);
  }
  const bypassActors = await ciBypassActors(request, main);
  log(`Ready to require ${GATES.join(', ')} and resolved threads, then replace the blanket review count with dynamic ownership. Only required-ci-gate permits explicit PR-only bypass by ${bypassActors.length} global code owners. Backup: ${backupPath}`);
  if (!apply) { log('Dry run. Add --apply to activate.'); return; }
  const liveProtection = request('GET', 'branches/main/protection');
  if (JSON.stringify(reviewSettings(liveProtection)) !== JSON.stringify(reviewSettings(backup.protection))) {
    throw new Error('Review settings changed since the snapshot; take a fresh snapshot before activation.');
  }
  if (request('GET', 'branches/main').commit.sha !== main) throw new Error('Main changed during readiness checks; rerun activation.');
  for (const desired of [ciRulesetBody(bypassActors), rulesetBody()]) {
    const existing = current.find((rule) => rule.name === desired.name);
    const installed = request(existing ? 'PUT' : 'POST', existing ? `rulesets/${existing.id}` : 'rulesets', desired);
    verifyRuleset(request('GET', `rulesets/${installed.id}`), desired);
  }
  request('PATCH', 'branches/main/protection/required_pull_request_reviews', {
    required_approving_review_count: 0, require_code_owner_reviews: false, dismiss_stale_reviews: false, require_last_push_approval: false,
  });
  const active = request('GET', 'branches/main/protection');
  if (active.required_pull_request_reviews.required_approving_review_count !== 0 || active.required_pull_request_reviews.require_code_owner_reviews) throw new Error('Native review requirements were not updated.');
  log('Review policy activated. Socket Security and unrelated protections preserved.');
}

if (require.main === module) main().catch((error) => { console.error(error.message); process.exitCode = 1; });
module.exports = { rulesetBody, ciRulesetBody, ciBypassActors, reviewSettings, main };
