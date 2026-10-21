'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { main, rulesetBody, ciRulesetBody } = require('./admin.cjs');
const { GATES } = require('./policy.cjs');
const base = 'b'.repeat(40);
const previousBase = 'a'.repeat(40);

async function fixture(t, options = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'world-chain-policy-test-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const backup = path.join(dir, 'backup.json');
  const original = { required_pull_request_reviews: { required_approving_review_count: 1, require_code_owner_reviews: true, dismiss_stale_reviews: false, require_last_push_approval: false } };
  const live = structuredClone(original);
  const mutations = [];
  const reads = [];
  const socket = { id: 99, name: 'require-socket-pr-alerts', source_type: 'Organization' };
  const rulesets = new Map((options.existingRulesets || []).map((rule) => [rule.id, structuredClone(rule)]));
  const originalRulesets = structuredClone([...rulesets.values()]);
  let nextId = 10;
  let branchReads = 0;
  const content = (text) => ({ type: 'file', encoding: 'base64', size: Buffer.byteLength(text), content: Buffer.from(text).toString('base64') });
  const api = {
    request(method, resource, body) {
      if (method !== 'GET') mutations.push({ method, resource, body });
      else reads.push(resource);
      if (resource === 'branches/main/protection') return structuredClone(live);
      if (resource === 'branches/main') return { commit: { sha: options.mainChanged && branchReads++ > 0 ? 'new-base' : base } };
      if (resource.startsWith('git/trees/')) {
        if (options.treeError) throw Object.assign(new Error('Cannot read policy tree'), { status: 403 });
        const ref = resource.slice('git/trees/'.length);
        assert([base, previousBase].includes(ref), `Unexpected policy reference: ${ref}`);
        return { truncated: Boolean(options.truncatedTree), tree: [{ path: 'CODEOWNERS', type: 'blob', mode: '100644',
          sha: options.changedPolicy && ref === previousBase ? 'previous-owners' : 'current-owners' }] };
      }
      if (resource.startsWith('contents/')) {
        if (options.notDeployed) throw new Error('Not deployed on main');
        const filename = resource.slice('contents/'.length).split('?')[0];
        if (filename.endsWith('CODEOWNERS')) {
          if (options.codeownersError) throw Object.assign(new Error('CODEOWNERS access denied'), { status: 403 });
          const source = filename === '.github/CODEOWNERS' ? options.githubCodeowners
            : filename === 'CODEOWNERS' ? options.codeowners ?? '* @Alice @Bob\n' : undefined;
          if (source === undefined) throw Object.assign(new Error('Not found'), { status: 404 });
          return content(source);
        }
        return content(fs.readFileSync(filename, 'utf8'));
      }
      if (resource.startsWith('collaborators/')) {
        const login = resource.split('/')[1];
        const id = { alice: 101, bob: 102, carol: 103 }[login];
        if (!id) throw new Error(`Unexpected owner: ${login}`);
        return { permission: options.readOnlyOwner ? 'read' : 'write', user: { login, id, type: options.botOwner ? 'Bot' : 'User' } };
      }
      if (resource === '/orgs/worldcoin/teams/protocol') {
        if (options.missingTeam) throw Object.assign(new Error('CI bypass team not found'), { status: 404 });
        return { id: 14554659, slug: 'protocol', privacy: options.secretTeam ? 'secret' : 'closed' };
      }
      if (resource === '/orgs/worldcoin/teams/protocol/repos/worldcoin/world-chain') {
        return { full_name: 'worldcoin/world-chain', permissions: { push: !options.readOnlyTeam } };
      }
      if (resource === 'pulls/1') return { base: { ref: 'main', sha: options.stalePrBase ? previousBase : base }, head: { sha: 'head' } };
      if (resource === 'rulesets' && method === 'POST') {
        const rule = { id: nextId++, ...body }; rulesets.set(rule.id, structuredClone(rule)); return rule;
      }
      if (resource.startsWith('rulesets/')) {
        const id = Number(resource.split('/')[1]);
        if (method === 'PUT') { const rule = { id, ...body }; rulesets.set(id, structuredClone(rule)); return rule; }
        if (method === 'DELETE') { rulesets.delete(id); return null; }
        if (method === 'GET') {
          const rule = structuredClone(rulesets.get(id));
          if (options.reorderedConditions) rule.conditions = { ref_name: { exclude: [], include: ['refs/heads/main'] } };
          if (options.badRuleset === true || options.badRuleset === rule.name) rule.enforcement = 'disabled';
          if (options.reviewBypass && rule.name === 'world-chain-review-policy') rule.bypass_actors = [{ actor_id: 5, actor_type: 'RepositoryRole', bypass_mode: 'pull_request' }];
          if (options.wrongCiOwner && rule.name === 'world-chain-ci-policy') rule.bypass_actors[0].actor_id = 999;
          return rule;
        }
      }
      if (resource === 'branches/main/protection/required_pull_request_reviews' && method === 'PATCH') {
        Object.assign(live.required_pull_request_reviews, body); return live.required_pull_request_reviews;
      }
      throw new Error(`Unexpected admin API: ${method} ${resource}`);
    },
    list(resource) {
      if (resource === 'rulesets') return [socket, ...[...rulesets.values()].map((rule) => ({ ...rule, source_type: 'Repository' }))];
      if (resource === 'commits/head/check-runs') return GATES.map((name) => ({ name, status: 'completed', conclusion: options.badGate ? 'failure' : 'success',
        app: { slug: 'github-actions' }, external_id: 'review-policy:worldcoin/world-chain:1:head',
        output: { summary: options.gateSummary ?? `PR #1; head head; base ${options.gateBases?.[name] || options.evidenceBase || base}; risk: critical.` } }));
      throw new Error(`Unexpected admin list: ${resource}`);
    },
  };
  const run = (command, extra = []) => main([command, '--backup', backup, ...extra], api, () => {});
  await run('snapshot');
  return { backup, original, live, mutations, reads, rulesets, originalRulesets, run };
}

test('activation dry-run checks readiness without GitHub mutations; snapshot cannot be overwritten', async (t) => {
  const f = await fixture(t);
  await f.run('activate', ['--evidence-pr', '1']);
  assert.deepEqual(f.mutations, []);
  await assert.rejects(() => f.run('snapshot'), /EEXIST/);
});

test('activation verifies current target policy from gate evidence despite a cached PR base SHA', async (t) => {
  const f = await fixture(t, { stalePrBase: true });
  await f.run('activate', ['--evidence-pr', '1']);
  assert.deepEqual(f.mutations, []);
});

test('activation accepts successful evidence from an older commit with identical policy', async (t) => {
  for (const apply of [false, true]) await t.test(apply ? 'apply' : 'dry run', async (t) => {
    const f = await fixture(t, { evidenceBase: previousBase });
    await f.run('activate', ['--evidence-pr', '1', ...(apply ? ['--apply'] : [])]);
    assert.equal(f.mutations.length, apply ? 3 : 0);
    assert.deepEqual(f.reads.filter((resource) => resource.startsWith('git/trees/')),
      [`git/trees/${previousBase}`, `git/trees/${base}`], 'all four gates share immutable policy reads');
  });
});

test('incompatible or unverifiable evidence policy cannot change protections', async (t) => {
  for (const options of [{ changedPolicy: true }, { treeError: true }, { truncatedTree: true }]) {
    const f = await fixture(t, { gateBases: { 'required-ci-gate': previousBase }, ...options });
    await assert.rejects(() => f.run('activate', ['--evidence-pr', '1', '--apply']),
      /different target policy|Cannot read policy tree|Incomplete Git tree/);
    assert.deepEqual(f.mutations, []);
    assert.deepEqual(f.live, f.original);
  }
});

test('missing or malformed evidence headers cannot select a policy reference', async (t) => {
  for (const gateSummary of ['', `Unrelated text; base ${base};`, 'PR #1; head head; base main; risk: low.',
    `PR #2; head head; base ${base}; risk: low.`, `PR #1; head other; base ${base}; risk: low.`]) {
    const f = await fixture(t, { gateSummary });
    await assert.rejects(() => f.run('activate', ['--evidence-pr', '1', '--apply']), /missing or invalid target commit/);
    assert.deepEqual(f.mutations, []);
    assert(!f.reads.some((resource) => resource.startsWith('git/trees/')));
  }
});

test('activation installs CI before review gates; rollback restores reviews before deleting both rulesets', async (t) => {
  const f = await fixture(t);
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  assert.deepEqual(f.mutations.map((item) => item.resource), ['rulesets', 'rulesets', 'branches/main/protection/required_pull_request_reviews']);
  assert.equal(f.live.required_pull_request_reviews.required_approving_review_count, 0);
  await f.run('restore', ['--apply']);
  assert.deepEqual(f.mutations.slice(3).map((item) => item.resource), ['branches/main/protection/required_pull_request_reviews', 'rulesets/11', 'rulesets/10']);
  assert.deepEqual(f.live, f.original);
  assert(!f.mutations.some((item) => item.resource === 'rulesets/99'));
});

test('undeployed code and unsuccessful pilot gates cannot change protection', async (t) => {
  for (const options of [{ notDeployed: true }, { badGate: true }]) {
    const f = await fixture(t, options);
    await assert.rejects(() => f.run('activate', ['--evidence-pr', '1', '--apply']));
    assert.deepEqual(f.mutations, []);
    assert.deepEqual(f.live, f.original);
  }
});

test('a ruleset not enforced by GitHub cannot relax the original approval requirement', async (t) => {
  const f = await fixture(t, { badRuleset: true });
  await assert.rejects(() => f.run('activate', ['--evidence-pr', '1', '--apply']), /keeping the native review requirement/);
  assert.deepEqual(f.live, f.original);
  assert.equal(f.mutations.length, 1);
});

test('build/test CI and both AI gates permit PR-only bypass while human approvals and threads remain mandatory', async (t) => {
  const f = await fixture(t, { codeowners: '* @Alice @Bob @Alice\n/docs/ @Carol\n' });
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  const [ci, review] = f.mutations.map((item) => item.body);
  assert.deepEqual(ci.bypass_actors, [
    { actor_id: 101, actor_type: 'User', bypass_mode: 'pull_request' },
    { actor_id: 102, actor_type: 'User', bypass_mode: 'pull_request' },
  ]);
  assert.deepEqual(ci.rules.map((rule) => rule.type), ['required_status_checks']);
  assert.deepEqual(ci.rules[0].parameters.required_status_checks, [
    { context: 'codex-review-gate', integration_id: 15368 },
    { context: 'bugbot-review-gate', integration_id: 15368 },
    { context: 'required-ci-gate', integration_id: 15368 },
  ]);
  assert.deepEqual(review.bypass_actors, []);
  assert.deepEqual(review.rules[0].parameters.required_status_checks, [{ context: 'human-review-gate', integration_id: 15368 }]);
  assert.equal(review.rules[1].parameters.required_review_thread_resolution, true);
  assert(f.reads.includes(`contents/CODEOWNERS?ref=${base}`));
  assert(!f.reads.some((resource) => resource.includes('CODEOWNERS?ref=head')));
});

test('CI bypass honors CODEOWNERS precedence and the last global rule', async (t) => {
  const f = await fixture(t, { githubCodeowners: '* @Alice\n* @Carol\n', codeowners: '* @Bob\n' });
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  assert.deepEqual(f.mutations[0].body.bypass_actors, [{ actor_id: 103, actor_type: 'User', bypass_mode: 'pull_request' }]);
  assert(!f.reads.includes(`contents/CODEOWNERS?ref=${base}`));
});

test('a global owner team becomes a native team bypass actor without copying its members', async (t) => {
  const f = await fixture(t, { codeowners: '* @worldcoin/protocol\n' });
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  assert.deepEqual(f.mutations[0].body.bypass_actors, [{ actor_id: 14554659, actor_type: 'Team', bypass_mode: 'pull_request' }]);
  assert.deepEqual(f.mutations[1].body.bypass_actors, []);
  assert(!f.reads.some((resource) => resource.includes('/members')));
  assert(!f.reads.some((resource) => resource.startsWith('collaborators/')));
  await f.run('restore', ['--apply']);
  assert.deepEqual(f.live, f.original);
  assert.equal(f.rulesets.size, 0);
});

test('GitHub response key order does not change verification of ruleset conditions', async (t) => {
  const f = await fixture(t, { reorderedConditions: true });
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  assert.equal(f.live.required_pull_request_reviews.required_approving_review_count, 0);
});

test('missing global owners, non-writable owners, bots and unreadable CODEOWNERS cannot grant bypass', async (t) => {
  for (const options of [{ codeowners: '/docs/ @Alice' },
    { codeowners: '*' }, { readOnlyOwner: true }, { botOwner: true }, { codeownersError: true }]) {
    const f = await fixture(t, options);
    await assert.rejects(() => f.run('activate', ['--evidence-pr', '1', '--apply']), /CI bypass|CODEOWNERS access denied/);
    assert.deepEqual(f.mutations, []);
  }
});

test('foreign, secret, missing or non-writable teams cannot receive CI bypass', async (t) => {
  for (const options of [{ codeowners: '* @another-org/protocol' }, { secretTeam: true }, { readOnlyTeam: true }, { missingTeam: true }]) {
    const f = await fixture(t, { codeowners: '* @worldcoin/protocol', ...options });
    await assert.rejects(() => f.run('activate', ['--evidence-pr', '1', '--apply']), /CI bypass team/);
    assert.deepEqual(f.mutations, []);
  }
});

test('an unexpected review bypass or CI owner cannot relax the original approval requirement', async (t) => {
  for (const options of [{ reviewBypass: true }, { wrongCiOwner: true }, { badRuleset: 'world-chain-review-policy' }]) {
    const f = await fixture(t, options);
    await assert.rejects(() => f.run('activate', ['--evidence-pr', '1', '--apply']), /keeping the native review requirement/);
    assert.deepEqual(f.live, f.original);
    assert(!f.mutations.some((item) => item.resource === 'branches/main/protection/required_pull_request_reviews'));
  }
});

test('rollback restores both previous rulesets, including their previous bypass actors', async (t) => {
  const f = await fixture(t, { existingRulesets: [
    { id: 50, ...rulesetBody() },
    { id: 60, ...ciRulesetBody([{ actor_id: 103, actor_type: 'User', bypass_mode: 'pull_request' }]) },
  ] });
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  await f.run('restore', ['--apply']);
  assert.deepEqual(f.live, f.original);
  assert.deepEqual([...f.rulesets.values()], f.originalRulesets);
  assert.deepEqual(f.mutations.slice(3).map((item) => item.resource), ['branches/main/protection/required_pull_request_reviews', 'rulesets/50', 'rulesets/60']);
});

test('migration from the combined ruleset installs CI protection before removing its old CI requirement', async (t) => {
  const old = { id: 50, ...rulesetBody() };
  old.rules[0].parameters.required_status_checks = GATES.map((context) => ({ context, integration_id: 15368 }));
  const f = await fixture(t, { existingRulesets: [old] });
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  assert.deepEqual(f.mutations.map((item) => item.resource), ['rulesets', 'rulesets/50', 'branches/main/protection/required_pull_request_reviews']);
  await f.run('restore', ['--apply']);
  assert.deepEqual([...f.rulesets.values()], f.originalRulesets);
});

test('extending an existing CI-only bypass installs AI checks before removing their old requirements and restores the old split', async (t) => {
  const review = { id: 50, ...rulesetBody() };
  review.rules[0].parameters.required_status_checks = GATES.filter((gate) => gate !== 'required-ci-gate')
    .map((context) => ({ context, integration_id: 15368 }));
  const ci = { id: 60, ...ciRulesetBody([{ actor_id: 14554659, actor_type: 'Team', bypass_mode: 'pull_request' }]) };
  ci.rules[0].parameters.required_status_checks = [{ context: 'required-ci-gate', integration_id: 15368 }];
  const f = await fixture(t, { codeowners: '* @worldcoin/protocol', existingRulesets: [review, ci] });
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  assert.deepEqual(f.mutations.map((item) => item.resource), ['rulesets/60', 'rulesets/50', 'branches/main/protection/required_pull_request_reviews']);
  assert.deepEqual(f.rulesets.get(60), { id: 60, ...ciRulesetBody(ci.bypass_actors) });
  assert.deepEqual(f.rulesets.get(50), { id: 50, ...rulesetBody() });
  await f.run('restore', ['--apply']);
  assert.deepEqual([...f.rulesets.values()], f.originalRulesets);
  assert.deepEqual(f.live, f.original);
});

test('a target-branch change during owner lookup requires a fresh readiness check', async (t) => {
  const f = await fixture(t, { mainChanged: true });
  await assert.rejects(() => f.run('activate', ['--evidence-pr', '1', '--apply']), /Main changed/);
  assert.deepEqual(f.mutations, []);
});
