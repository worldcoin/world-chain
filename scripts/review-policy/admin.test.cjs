'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { main } = require('./admin.cjs');
const { GATES } = require('./policy.cjs');

async function fixture(t, options = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'world-chain-policy-test-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const backup = path.join(dir, 'backup.json');
  const original = { required_pull_request_reviews: { required_approving_review_count: 1, require_code_owner_reviews: true, dismiss_stale_reviews: false, require_last_push_approval: false } };
  const live = structuredClone(original);
  const mutations = [];
  const socket = { id: 99, name: 'require-socket-pr-alerts', source_type: 'Organization' };
  let ruleset;
  const api = {
    request(method, resource, body) {
      if (method !== 'GET') mutations.push({ method, resource, body });
      if (resource === 'branches/main/protection') return structuredClone(live);
      if (resource === 'branches/main') return { commit: { sha: 'base' } };
      if (resource.startsWith('contents/')) {
        if (options.notDeployed) throw new Error('Not deployed on main');
        const filename = resource.slice('contents/'.length).split('?')[0];
        return { content: fs.readFileSync(filename).toString('base64') };
      }
      if (resource === 'pulls/1') return { base: { ref: 'main', sha: options.stalePrBase ? 'older-main' : 'base' }, head: { sha: 'head' } };
      if (resource === 'rulesets' && method === 'POST') { ruleset = { id: 10, ...body }; return ruleset; }
      if (resource === 'rulesets/10' && method === 'GET') return { ...ruleset, ...(options.badRuleset ? { enforcement: 'disabled' } : {}) };
      if (resource === 'rulesets/10' && method === 'DELETE') { ruleset = undefined; return null; }
      if (resource === 'branches/main/protection/required_pull_request_reviews' && method === 'PATCH') {
        Object.assign(live.required_pull_request_reviews, body); return live.required_pull_request_reviews;
      }
      throw new Error(`Unexpected admin API: ${method} ${resource}`);
    },
    list(resource) {
      if (resource === 'rulesets') return [socket, ...(ruleset ? [{ ...ruleset, source_type: 'Repository' }] : [])];
      if (resource === 'commits/head/check-runs') return GATES.map((name) => ({ name, status: 'completed', conclusion: options.badGate ? 'failure' : 'success',
        app: { slug: 'github-actions' }, external_id: 'review-policy:worldcoin/world-chain:1:head', output: { summary: 'PR #1; head head; base base; risk: critical.' } }));
      throw new Error(`Unexpected admin list: ${resource}`);
    },
  };
  const run = (command, extra = []) => main([command, '--backup', backup, ...extra], api, () => {});
  await run('snapshot');
  return { backup, original, live, mutations, run };
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

test('activation installs and verifies gates before relaxing reviews; rollback restores reviews before deleting gates', async (t) => {
  const f = await fixture(t);
  await f.run('activate', ['--evidence-pr', '1', '--apply']);
  assert.deepEqual(f.mutations.map((item) => item.resource), ['rulesets', 'branches/main/protection/required_pull_request_reviews']);
  assert.equal(f.live.required_pull_request_reviews.required_approving_review_count, 0);
  await f.run('restore', ['--apply']);
  assert.deepEqual(f.mutations.slice(2).map((item) => item.resource), ['branches/main/protection/required_pull_request_reviews', 'rulesets/10']);
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
