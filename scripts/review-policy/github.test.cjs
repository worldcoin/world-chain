'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { evaluate, targets, Api, readCodeowners, people } = require('./github.cjs');
const { GATES } = require('./policy.cjs');
const { rulesetBody, reviewSettings } = require('./admin.cjs');

const head = 'a'.repeat(40);
const base = 'b'.repeat(40);
const now = Date.parse('2026-10-19T10:00:00Z');
const at = new Date(now).toISOString();
const repo = { owner: 'worldcoin', repo: 'world-chain' };
const context = { repo, runId: 123, payload: {}, eventName: 'workflow_dispatch' };
const codexReview = { id: 1, user: { login: 'chatgpt-codex-connector[bot]', type: 'Bot' },
  state: 'COMMENTED', commit_id: head, submitted_at: at };
const authorApproval = { id: 1, user: { login: 'owner', type: 'User' }, body: '/approve', created_at: at };

function fake(options = {}) {
  const pr = { number: 1, state: 'open', changed_files: 1, user: { login: 'owner', type: 'User' },
    head: { sha: head, ref: 'branch', repo: { id: 1 } }, base: { ref: 'main', sha: options.prBase || base, repo: { full_name: 'worldcoin/world-chain' } } };
  const checks = [
    { id: 1, name: 'Cursor Bugbot', head_sha: head, status: 'completed', conclusion: 'success', app: { slug: 'cursor' } },
    ...(options.checks || []),
  ];
  const calls = [];
  let ticks = 0;
  let nextCheckId = Math.max(...checks.map((check) => check.id)) + 1;
  const github = { request: async (route, args) => {
    calls.push({ route, args });
    let data;
    if (route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}') {
      data = structuredClone(pr);
      if (options.moveHead && ++ticks > 3) data.head.sha = 'c'.repeat(40);
    } else if (route === 'GET /repos/{owner}/{repo}/branches/{branch}') data = { commit: { sha: base } };
    else if (route === 'GET /repos/{owner}/{repo}/commits/{ref}/check-runs') data = { check_runs: structuredClone(checks) };
    else if (route === 'POST /repos/{owner}/{repo}/check-runs') {
      data = { id: nextCheckId++, app: { slug: 'github-actions' }, ...args };
      checks.push(data);
    } else if (route === 'PATCH /repos/{owner}/{repo}/check-runs/{check_run_id}') {
      data = checks.find((check) => check.id === args.check_run_id);
      Object.assign(data, args);
    } else if (route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}/files') data = options.files || [{ filename: 'README.md' }];
    else if (route === 'GET /repos/{owner}/{repo}/contents/{path}') {
      if (args.path === '.github/CODEOWNERS') throw Object.assign(new Error('Not found'), { status: 404 });
      data = { type: 'file', encoding: 'base64', size: 20, content: Buffer.from('* @owner @reviewer').toString('base64') };
    } else if (route === 'GET /repos/{owner}/{repo}/pulls') data = options.duplicate ? [pr, { ...pr, number: 2 }] : [pr];
    else if (route === 'GET /repos/{owner}/{repo}/actions/workflows/{workflow_id}') data = { id: args.workflow_id === 'review-policy-tests.yml' ? 1 : 2 };
    else if (route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews') data = options.reviews || (options.noCodex ? [] : [codexReview]);
    else if (route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/comments') data = options.comments || [];
    else if (route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/reactions') data = options.reactions || [];
    else if (route === 'GET /repos/{owner}/{repo}/actions/runs') data = { workflow_runs: options.noCI ? [] : [1, 2].map((id) => ({
      id, workflow_id: id, event: 'pull_request', head_sha: head, pull_requests: [{ number: 1 }], status: 'completed', conclusion: 'success',
    })) };
    else if (route === 'GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs') data = { jobs: [{ name: args.run_id === 1 ? 'review-policy-tests' : 'check-large-files', status: 'completed', conclusion: 'success' }] };
    else if (route === 'GET /repos/{owner}/{repo}/collaborators/{username}/permission') data = { permission: 'write' };
    else throw new Error(`Unexpected API: ${route}`);
    if (options.respond) data = options.respond(route, args, structuredClone(data));
    return { data: structuredClone(data) };
  } };
  const failures = [];
  const core = { info: () => {}, setFailed: (text) => failures.push(text), summary: { addRaw: () => ({ write: async () => {} }) } };
  return { github, core, checks, failures, calls, pr };
}

async function run(options = {}) {
  process.env.REVIEW_POLICY_BASE_SHA = options.base || base;
  const fixture = fake(options);
  let clock = now;
  await evaluate({ github: fixture.github, context, core: fixture.core, number: 1, now: () => clock,
    sleep: async () => { clock += options.sleepMs ?? 31 * 60 * 1000; options.afterSleep?.(fixture); } });
  return fixture;
}

test('trusted controller publishes four successful checks on PR head, never on main', async () => {
  const result = await run();
  assert.deepEqual(result.failures, []);
  for (const name of GATES) {
    const check = result.checks.find((check) => check.name === name);
    assert.equal(check.head_sha, head);
    assert.equal(check.conclusion, 'success', name);
    assert.match(check.output.summary, /Verified evidence fingerprint: [a-f0-9]{64}/);
  }
  assert(result.calls.every((call) => call.args.request.timeout === 20000));
});

test('reruns find completed reviewer checks hidden by the default latest-attempt filter', async () => {
  let sleeps = 0;
  const result = await run({
    respond: (route, args, data) => route === 'GET /repos/{owner}/{repo}/commits/{ref}/check-runs' && args.filter !== 'all'
      ? { check_runs: data.check_runs.filter((check) => check.name !== 'Cursor Bugbot') } : data,
    afterSleep: () => { sleeps++; },
  });
  assert.deepEqual(result.failures, []);
  assert.equal(sleeps, 0, 'completed reviews must not wait for the AI timeout');
  for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'success', name);
});

test('unapprove updates every duplicate gate, including the newer green check GitHub displays', async () => {
  const external_id = `review-policy:worldcoin/world-chain:1:${head}`;
  const checks = [10, 20].map((id) => ({ id, name: 'human-review-gate', head_sha: head, external_id,
    app: { slug: 'github-actions' }, status: 'completed', conclusion: 'success' }));
  const result = await run({ checks, files: [{ filename: 'ordinary-config.yaml' }],
    comments: [authorApproval, { ...authorApproval, id: 2, body: '/unapprove', created_at: new Date(now + 1000).toISOString() }] });
  assert.deepEqual(result.failures, []);
  const humanChecks = result.checks.filter((check) => check.name === 'human-review-gate');
  assert(humanChecks.length > 2);
  for (const check of humanChecks) assert.equal(check.conclusion, 'failure', `check ${check.id}`);
});

test('an older base SHA cached by the PR API does not replace current target policy', async () => {
  const result = await run({ prBase: 'older-main' });
  assert.deepEqual(result.failures, []);
  for (const name of GATES) {
    const check = result.checks.find((check) => check.name === name);
    assert.equal(check.conclusion, 'success', name);
    assert.match(check.output.summary, new RegExp(`base ${base};`));
  }
  assert(result.calls.filter((call) => call.route.includes('/contents/')).every((call) => call.args.ref === base));
});

test('the target branch advancing during evaluation blocks clearance even if the PR API stays unchanged', async () => {
  let reads = 0;
  const result = await run({ respond: (route, _args, data) => route === 'GET /repos/{owner}/{repo}/branches/{branch}' && ++reads > 5
    ? { commit: { sha: 'new-main' } } : data });
  assert(!result.calls.some((call) => call.route.startsWith('PATCH ') && call.args.conclusion === 'success'));
});

test('re-evaluation invalidates old green checks while fresh AI evidence is pending', async () => {
  const checks = GATES.map((name, id) => ({ id: id + 10, name, head_sha: head,
    external_id: `review-policy:worldcoin/world-chain:1:${head}`, app: { slug: 'github-actions' }, status: 'completed', conclusion: 'success' }));
  let observed = false;
  const result = await run({ checks, noCodex: true, afterSleep: (fixture) => {
    observed = true;
    const codexChecks = fixture.checks.filter((check) => check.name === 'codex-review-gate');
    assert(codexChecks.every((check) => check.conclusion !== 'success'));
    assert(codexChecks.some((check) => check.status === 'in_progress'));
  } });
  assert(observed);
  assert.deepEqual(result.failures, []);
  assert(result.checks.filter((check) => check.name === 'codex-review-gate').every((check) => check.conclusion === 'failure'));
});

test('author confirmation changes during the AI wait cannot leave a successful human gate', async (t) => {
  for (const change of ['revoke', 'edit', 'delete']) await t.test(change, async () => {
    const options = { files: [{ filename: 'ordinary-config.yaml' }], noCodex: true,
      comments: [structuredClone(authorApproval)], sleepMs: 30000 };
    options.afterSleep = () => {
      options.noCodex = false;
      if (change === 'revoke') options.comments.push({ ...authorApproval, id: 2, body: '/unapprove', created_at: new Date(now + 30000).toISOString() });
      if (change === 'edit') options.comments[0].body = 'Still reviewing';
      if (change === 'delete') options.comments = [];
    };
    const result = await run(options);
    assert.deepEqual(result.failures, []);
    assert(result.calls.some((call) => call.args.output?.title === 'human-review-gate: success'));
    assert.equal(result.checks.find((check) => check.name === 'codex-review-gate').conclusion, 'success');
    assert.equal(result.checks.find((check) => check.name === 'human-review-gate').conclusion, 'failure');
  });
});

test('dismissal or changes requested during the AI wait replaces the earlier approval', async (t) => {
  for (const state of ['DISMISSED', 'CHANGES_REQUESTED']) await t.test(state, async () => {
    const approval = { id: 2, user: { login: 'reviewer', type: 'User' }, state: 'APPROVED', commit_id: head, submitted_at: at };
    const options = { files: [{ filename: 'ordinary-config.yaml' }], reviews: [approval], sleepMs: 30000 };
    options.afterSleep = () => { options.reviews = [codexReview, { ...approval, state }]; };
    const result = await run(options);
    assert.deepEqual(result.failures, []);
    assert(result.calls.some((call) => call.args.output?.title === 'human-review-gate: success'));
    assert.equal(result.checks.find((check) => check.name === 'codex-review-gate').conclusion, 'success');
    assert.equal(result.checks.find((check) => check.name === 'human-review-gate').conclusion, 'failure');
  });
});

test('a failed CI rerun during the AI wait replaces the earlier successful CI result', async () => {
  let rerun = false;
  const options = { noCodex: true, sleepMs: 30000,
    afterSleep: () => { options.noCodex = false; rerun = true; },
    respond: (route, _args, data) => {
      if (rerun && route === 'GET /repos/{owner}/{repo}/actions/runs') {
        data.workflow_runs[0] = { ...data.workflow_runs[0], run_attempt: 2, conclusion: 'failure' };
      }
      return data;
    } };
  const result = await run(options);
  assert.deepEqual(result.failures, []);
  assert(result.calls.some((call) => call.args.output?.title === 'required-ci-gate: success'));
  assert.equal(result.checks.find((check) => check.name === 'codex-review-gate').conclusion, 'success');
  assert.equal(result.checks.find((check) => check.name === 'required-ci-gate').conclusion, 'failure');
});

test('CI completed during the AI wait can pass without another controller run', async () => {
  const options = { noCodex: true, noCI: true, sleepMs: 30000 };
  options.afterSleep = () => { options.noCodex = false; options.noCI = false; };
  const result = await run(options);
  assert.deepEqual(result.failures, []);
  for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'success', name);
});

test('an author approval removed between final snapshots blocks merge clearance', async () => {
  let reads = 0;
  const result = await run({ files: [{ filename: 'ordinary-config.yaml' }], comments: [authorApproval],
    respond: (route, _args, data) => route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/comments' && ++reads >= 3 ? [] : data });
  assert.deepEqual(result.failures, []);
  assert.equal(result.checks.find((check) => check.name === 'human-review-gate').conclusion, 'failure');
  assert(!result.calls.some((call) => call.args.output?.title === 'human-review-gate: success'));
});

test('evidence that keeps changing cannot produce successful gates', async () => {
  let reads = 0;
  const result = await run({ respond: (route, _args, data) => route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/comments'
    ? [{ ...authorApproval, body: `Discussion update ${++reads}` }] : data });
  assert.equal(result.failures.length, 1);
  assert.match(result.failures[0], /Evidence kept changing/);
  for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'failure', name);
});

test('first observation survives re-evaluation; old thumbs-up cannot become fresh', async () => {
  const observation = { id: 10, name: 'review-policy-observed', app: { slug: 'github-actions' },
    external_id: `review-policy:worldcoin/world-chain:1:${head}`, started_at: at };
  const reaction = { content: '+1', user: { login: 'chatgpt-codex-connector[bot]' }, created_at: '2026-10-19T09:00:00Z' };
  const result = await run({ checks: [observation], noCodex: true, reactions: [reaction] });
  assert.equal(result.checks.filter((check) => check.name === 'review-policy-observed').length, 1);
  assert.equal(result.checks.find((check) => check.name === 'codex-review-gate').conclusion, 'failure');
  assert(result.calls.some((call) => call.args.output?.summary?.includes('30 minutes')));
});

test('missing CI remains pending rather than reporting success', async () => {
  const result = await run({ noCI: true });
  assert.equal(result.checks.find((check) => check.name === 'required-ci-gate').status, 'in_progress');
});

test('head movement never publishes success after the movement', async () => {
  const result = await run({ moveHead: true });
  assert(!result.calls.some((call) => call.route.startsWith('PATCH ') && call.args.conclusion === 'success'));
});

test('base movement, incomplete file list and duplicate PR heads fail closed', async () => {
  for (const options of [{ base: 'changed' }, { files: [{ filename: 'a' }, { filename: 'b' }] }, { duplicate: true }]) {
    const result = await run(options);
    assert.equal(result.failures.length, 1);
    for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'failure', name);
  }
});

test('API pagination reads all pages and CODEOWNERS falls back only on 404', async () => {
  let pages = 0;
  const api = new Api({ request: async (_route, args) => {
    pages++;
    return { data: Array.from({ length: args.page === 1 ? 100 : 3 }, (_, i) => i) };
  } }, repo);
  assert.equal((await api.all('GET some-route')).length, 103);
  assert.equal(pages, 2);
  await assert.rejects(() => readCodeowners({ content: async () => { throw Object.assign(new Error('Forbidden'), { status: 403 }); } }, base), /Forbidden/);
});

test('event resolution ignores untrusted checks and locates PRs when webhook PR arrays are empty', async () => {
  assert.deepEqual(await targets({ github: {}, context: { ...context, eventName: 'check_run', payload: { check_run: { app: { slug: 'github-actions' } } } } }), []);
  const github = { request: async () => ({ data: [{ number: 1, state: 'open', base: { ref: 'main' }, head: { sha: head } },
    { number: 2, state: 'closed', base: { ref: 'main' }, head: { sha: head } }] }) };
  assert.deepEqual(await targets({ github, context: { ...context, eventName: 'workflow_run', payload: { workflow_run: { head_sha: head } } } }), [1]);
});

test('author commands wake the controller on create, revoke, edit and delete; chatter does not', async () => {
  const issue = { number: 1, pull_request: {} };
  for (const payload of [
    { comment: { body: '/approve' } }, { comment: { body: '/unapprove' } },
    { comment: { body: 'removed' }, changes: { body: { from: '/approve' } } },
  ]) assert.deepEqual(await targets({ github: {}, context: { ...context, eventName: 'issue_comment', payload: { issue, ...payload } } }), [1]);
  for (const body of ['looks good', '/signoff', '/unsignoff', '> /approve', '/approve extra']) {
    assert.deepEqual(await targets({ github: {}, context: { ...context, eventName: 'issue_comment', payload: { issue, comment: { body } } } }), []);
  }
});

test('main advances re-evaluate all open PRs', async () => {
  const github = { request: async () => ({ data: [{ number: 1 }, { number: 2 }] }) };
  assert.deepEqual(await targets({ github, context: { ...context, eventName: 'push' } }), [1, 2]);
});

test('unreadable team membership cannot grant owner fast path', async () => {
  const api = { call: async (route) => {
    if (route.includes('collaborators')) return { permission: 'write' };
    throw Object.assign(new Error('Forbidden'), { status: 403 });
  } };
  await assert.rejects(() => people(api, { user: { login: 'owner', type: 'User' } }, [], [{ requiredOwners: ['@worldcoin/team'] }]), /Cannot verify team/);
});

test('activation requires all gates and resolved threads with no bypass, preserves native settings on restore', () => {
  const rule = rulesetBody();
  assert.deepEqual(rule.bypass_actors, []);
  assert.deepEqual(rule.rules[0].parameters.required_status_checks.map((check) => check.context), GATES);
  assert.equal(rule.rules[1].parameters.required_review_thread_resolution, true);
  assert.equal(rule.rules[1].parameters.dismiss_stale_reviews_on_push, false);
  assert.deepEqual(reviewSettings({ required_pull_request_reviews: { required_approving_review_count: 1, require_code_owner_reviews: true, dismiss_stale_reviews: false, require_last_push_approval: false } }), {
    required_approving_review_count: 1, require_code_owner_reviews: true, dismiss_stale_reviews: false, require_last_push_approval: false,
  });
});
