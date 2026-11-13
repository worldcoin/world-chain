'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { createHash } = require('node:crypto');
const { evaluate, targets, Api, readCodeowners, people } = require('./github.cjs');
const { GATES } = require('./policy.cjs');
const { rulesetBody, ciRulesetBody, reviewSettings } = require('./admin.cjs');

const head = 'a'.repeat(40);
const base = 'b'.repeat(40);
const now = Date.parse('2026-10-19T10:00:00Z');
const at = new Date(now).toISOString();
const repo = { owner: 'worldcoin', repo: 'world-chain' };
const context = { repo, runId: 123, payload: {}, eventName: 'workflow_dispatch' };
const codexReview = { id: 1, user: { login: 'chatgpt-codex-connector[bot]', type: 'Bot' },
  state: 'COMMENTED', commit_id: head, submitted_at: at };
const authorApproval = { id: 1, user: { login: 'owner', type: 'User' }, body: '/approve', created_at: at };

function gitTrees(versions = {}) {
  const trees = new Map();
  const entry = (path, type, sha) => ({ path, type, sha, mode: type === 'tree' ? '040000' : '100644' });
  for (const [ref, changes] of Object.entries({ [base]: {}, 'ordinary-main': {},
    'new-main': { CODEOWNERS: 'new-owners' }, changed: { CODEOWNERS: 'old-owners' }, ...versions })) {
    const paths = { CODEOWNERS: 'owners', '.github/review-policy.json': 'config',
      '.github/workflows': 'workflows', '.github/actions': 'actions', 'scripts/review-policy': 'scripts', ...changes };
    const root = [];
    for (const directory of ['', '.github', 'scripts', 'docs']) {
      const entries = Object.entries(paths).filter(([path, sha]) => sha !== null
        && (path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '') === directory)
        .map(([path, sha]) => entry(path.slice(directory ? directory.length + 1 : 0),
          ['.github/workflows', '.github/actions', 'scripts/review-policy'].includes(path) ? 'tree' : 'blob', sha));
      if (!directory) root.push(...entries);
      else {
        const sha = createHash('sha1').update(JSON.stringify(entries)).digest('hex');
        trees.set(sha, { tree: entries, truncated: false });
        root.push(entry(directory, 'tree', sha));
      }
    }
    trees.set(ref, { tree: root, truncated: false });
  }
  return trees;
}

function fake(options = {}) {
  const trees = gitTrees(options.policyVersions);
  const pr = { number: 1, state: 'open', changed_files: 1, user: { login: 'owner', type: 'User' },
    head: { sha: head, ref: 'branch', repo: { id: 1 } }, base: { ref: 'main', sha: options.prBase || base, repo: { full_name: 'worldcoin/world-chain' } } };
  const checks = [
    { id: 1, name: 'Cursor Bugbot', head_sha: head, status: 'completed', conclusion: 'success', app: { slug: 'cursor' } },
    ...(options.checks || []),
  ];
  const calls = [];
  const statuses = [];
  let ticks = 0;
  let nextCheckId = Math.max(...checks.map((check) => check.id)) + 1;
  const github = { request: async (route, args) => {
    calls.push({ route, args });
    let data;
    if (route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}') {
      data = structuredClone(pr);
      if (options.moveHead && ++ticks > 3) data.head.sha = 'c'.repeat(40);
    } else if (route === 'GET /repos/{owner}/{repo}/branches/{branch}') data = { commit: { sha: base } };
    else if (route === 'GET /repos/{owner}/{repo}/git/trees/{tree_sha}') {
      data = trees.get(args.tree_sha);
      if (!data) throw new Error(`Unknown Git tree: ${args.tree_sha}`);
    }
    else if (route === 'GET /repos/{owner}/{repo}/commits/{ref}/check-runs') data = { check_runs: structuredClone(checks) };
    else if (route === 'POST /repos/{owner}/{repo}/statuses/{sha}') {
      data = { id: statuses.length + 1, ...args };
      statuses.unshift(data);
    } else if (route === 'POST /repos/{owner}/{repo}/check-runs') {
      data = { id: nextCheckId++, app: { slug: 'github-actions' }, ...args };
      checks.push(data);
    } else if (route === 'PATCH /repos/{owner}/{repo}/check-runs/{check_run_id}') {
      data = checks.find((check) => check.id === args.check_run_id);
      Object.assign(data, args);
    } else if (route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}/files') data = options.files || [{ filename: 'README.md' }];
    else if (route === 'GET /repos/{owner}/{repo}/contents/{path}') {
      if (args.path === '.github/CODEOWNERS') throw Object.assign(new Error('Not found'), { status: 404 });
      data = { type: 'file', encoding: 'base64', size: 20, content: Buffer.from(options.codeowners || '* @owner @reviewer').toString('base64') };
    } else if (route === 'GET /repos/{owner}/{repo}/pulls') data = options.duplicate ? [pr, { ...pr, number: 2 }] : [pr];
    else if (route === 'GET /repos/{owner}/{repo}/actions/workflows/{workflow_id}') data = { id: args.workflow_id === 'review-policy-tests.yml' ? 1 : 2 };
    else if (route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews') data = options.reviews || (options.noCodex ? [] : [codexReview]);
    else if (route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/comments') data = options.comments || [];
    else if (route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/reactions') data = options.reactions || [];
    else if (route === 'GET /repos/{owner}/{repo}/actions/runs') data = { workflow_runs: options.noCI ? [] : [1, 2].map((id) => ({
      id, workflow_id: id, run_attempt: 1, event: 'pull_request', head_sha: head, pull_requests: [{ number: 1 }], status: 'completed', conclusion: 'success',
    })) };
    else if (route === 'GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs') data = { jobs: [{ name: args.run_id === 1 ? 'review-policy-tests' : 'check-large-files', status: 'completed', conclusion: 'success' }] };
    else if (route === 'GET /repos/{owner}/{repo}/collaborators/{username}/permission') data = { permission: 'write' };
    else if (route === 'GET /orgs/{org}/teams/{team_slug}/members') data = options.members || [];
    else throw new Error(`Unexpected API: ${route}`);
    if (options.respond) data = options.respond(route, args, structuredClone(data));
    return { data: structuredClone(data) };
  } };
  const failures = [];
  const messages = [];
  const core = { info: (text) => messages.push(text), setFailed: (text) => failures.push(text), summary: { addRaw: () => ({ write: async () => {} }) } };
  return { github, core, checks, statuses, failures, calls, messages, pr };
}

async function run(options = {}) {
  process.env.REVIEW_POLICY_BASE_SHA = options.base || base;
  const fixture = fake(options);
  let clock = now;
  await evaluate({ github: fixture.github, teamGithub: options.teamAccess ? fixture.github : undefined,
    context: { ...context, ...options.context }, core: fixture.core, number: 1, now: () => clock,
    sleep: async (ms) => { clock += ms; options.afterSleep?.(fixture, ms); } });
  return fixture;
}

test('PR events dispatch the controller on trusted main without attaching its cancellable evaluator to the PR', async () => {
  const workflow = fs.readFileSync('.github/workflows/review-policy.yml', 'utf8');
  const request = workflow.match(/^  request:\n([\s\S]*?)^  targets:/m)?.[1];
  assert.match(request, /if: github\.event_name == 'pull_request_target'/);
  assert.match(request, /permissions:\n      actions: write/);
  assert(!request.includes('actions/checkout'));
  assert.match(workflow, /targets:\n    if: >-\n      github\.event_name != 'pull_request_target' &&/);
  assert.match(workflow, /group: review-policy-pr-\$\{\{ matrix\.pr \}\}\n      cancel-in-progress: true/);
  const script = request.split('          script: |\n')[1].replace(/^ {12}/gm, '');
  const calls = [];
  await vm.runInNewContext(`(async () => { ${script} })()`, {
    github: { rest: { actions: { createWorkflowDispatch: async (args) => calls.push(args) } } },
    context: { repo, payload: { pull_request: { number: 1204, head: { ref: 'untrusted-pr-branch' } } } },
  });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].owner, repo.owner);
  assert.equal(calls[0].repo, repo.repo);
  assert.equal(calls[0].workflow_id, 'review-policy.yml');
  assert.equal(calls[0].ref, 'main');
  assert.equal(calls[0].inputs.pr, '1204');
  assert.equal(calls[0].request.timeout, 20000);
});

test('trusted controller publishes four successful checks on PR head, never on main', async () => {
  const result = await run();
  assert.deepEqual(result.failures, []);
  for (const name of GATES) {
    const check = result.checks.find((check) => check.name === name);
    assert.equal(check.head_sha, head);
    assert.equal(check.conclusion, 'success', name);
    assert.match(check.output.summary, /Verified human evidence fingerprint: [a-f0-9]{64}/);
  }
  assert(result.calls.every((call) => call.args.request.timeout === 20000));
});

test('background events publish merge statuses on the PR head with matching diagnostic checks', async (t) => {
  const workflow = fs.readFileSync('.github/workflows/review-policy.yml', 'utf8');
  assert.match(workflow.split('  evaluate:')[1], /      statuses: write/);
  for (const eventName of ['workflow_dispatch', 'workflow_run', 'issue_comment']) await t.test(eventName, async () => {
    const result = await run({ context: { eventName } });
    assert.deepEqual(result.failures, []);
    assert.equal(result.statuses.length, GATES.length * 2);
    for (const name of GATES) {
      const check = result.checks.find((check) => check.name === name);
      const statuses = result.statuses.filter((status) => status.context === name);
      assert.deepEqual(statuses.map((status) => status.state), ['success', 'pending']);
      assert(statuses.every((status) => status.sha === head && status.target_url === `https://github.com/worldcoin/world-chain/runs/${check.id}`));
      const pending = result.calls.findIndex((call) => call.args.context === name && call.args.state === 'pending');
      const completed = result.calls.findIndex((call) => call.args.check_run_id === check.id && call.args.conclusion === 'success');
      const success = result.calls.findIndex((call) => call.args.context === name && call.args.state === 'success');
      assert(pending < completed && completed < success);
    }
  });
});

test('head, policy or evaluator replacement during check publication prevents a successful commit status', async (t) => {
  for (const change of ['head', 'policy', 'evaluator']) await t.test(change, async () => {
    let changed = false;
    const result = await run({ respond: (route, args, data) => {
      if (route.startsWith('PATCH ') && args.conclusion === 'success') changed = true;
      if (!changed) return data;
      if (change === 'head' && route.endsWith('/pulls/{pull_number}')) data.head.sha = 'new-head';
      if (change === 'policy' && route.endsWith('/branches/{branch}')) data.commit.sha = 'new-main';
      if (change === 'evaluator' && route.endsWith('/check-runs')) {
        data.check_runs.push({ ...data.check_runs.find((check) => check.name === GATES[0]), id: 10000 });
      }
      return data;
    } });
    assert.deepEqual(result.failures, []);
    assert(result.statuses.every((status) => status.state === 'pending'));
  });
});

test('commit status publication errors cannot report a successful evaluation', async (t) => {
  for (const failure of ['denied', 'mismatched response']) await t.test(failure, async () => {
    const result = await run({ respond: (route, args, data) => {
      if (route.endsWith('/statuses/{sha}') && args.state === 'success') {
        if (failure === 'denied') throw Object.assign(new Error('Forbidden'), { status: 403 });
        return { ...data, context: 'wrong-context' };
      }
      return data;
    } });
    assert.equal(result.failures.length, 1);
    for (const name of GATES) {
      assert.equal(result.statuses.find((status) => status.context === name).state, 'failure');
      assert.equal(result.checks.find((check) => check.name === name).conclusion, 'failure');
    }
  });
});

test('the controller accepts Cursor reusing its reviewed patch after a rebase', async () => {
  const result = await run({ checks: [{ id: 2, name: 'Cursor Bugbot', head_sha: head,
    app: { slug: 'cursor' }, status: 'completed', conclusion: 'neutral', started_at: at,
    output: { title: 'Bugbot Review', summary: 'Bugbot Analysis Progress (3s elapsed)\n\n✅ Reviewed patch is unchanged (3s)',
      text: 'Bugbot skipped this commit because its changes match the last review.' } }] });
  assert.deepEqual(result.failures, []);
  const check = result.checks.find((item) => item.name === 'bugbot-review-gate');
  assert.equal(check.head_sha, head);
  assert.equal(check.conclusion, 'success');
  assert.match(check.output.summary, /Bugbot confirms the reviewed patch is unchanged/);
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

test('ordinary main advances before startup or during AI polling do not interrupt the PR', async (t) => {
  for (const phase of ['startup', 'polling']) await t.test(phase, async () => {
    let advanced = phase === 'startup';
    let sleeps = 0;
    const options = { noCodex: true,
      respond: (route, _args, data) => route === 'GET /repos/{owner}/{repo}/branches/{branch}' && advanced
        ? { commit: { sha: 'ordinary-main' } } : data,
      afterSleep: () => { advanced = true; sleeps++; options.noCodex = false; },
    };
    const result = await run(options);
    assert.deepEqual(result.failures, []);
    assert.equal(sleeps, 1);
    for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'success', name);
    const treeCalls = result.calls.filter((call) => call.route.endsWith('/git/trees/{tree_sha}'));
    assert.equal(treeCalls.length, 5, 'immutable tree reads are shared and cached across identity checks');
  });
});

test('policy changes during AI polling invalidate prior human clearance', async () => {
  let advanced = false;
  const result = await run({ noCodex: true,
    respond: (route, _args, data) => route === 'GET /repos/{owner}/{repo}/branches/{branch}' && advanced
      ? { commit: { sha: 'new-main' } } : data,
    afterSleep: () => { advanced = true; },
  });
  assert.match(result.failures.join('\n'), /target review policy changed/);
  assert.equal(result.checks.find((check) => check.name === 'human-review-gate').conclusion, 'failure');
  assert.equal(result.statuses.find((status) => status.context === 'human-review-gate').state, 'failure');
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
      comments: [structuredClone(authorApproval)] };
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
    const statuses = result.statuses.filter((status) => status.context === 'human-review-gate');
    assert.deepEqual(statuses.map((status) => status.state), ['failure', 'success', 'pending']);
    const revoked = result.calls.findIndex((call) => call.args.context === 'human-review-gate' && call.args.state === 'failure');
    const failedCheck = result.calls.findIndex((call) => call.args.output?.title === 'human-review-gate: failure');
    assert(revoked < failedCheck);
  });
});

test('human clearance is independently verified and revoked while both AI reviewers remain unavailable', async () => {
  let sleeps = 0;
  let unavailable = true;
  const options = { files: [{ filename: 'ordinary-config.yaml' }], noCodex: true,
    comments: [structuredClone(authorApproval)],
    respond: (route, _args, data) => {
      if (unavailable && route === 'GET /repos/{owner}/{repo}/commits/{ref}/check-runs') {
        data.check_runs = data.check_runs.filter((check) => check.name !== 'Cursor Bugbot');
      }
      return data;
    },
  };
  options.afterSleep = (fixture) => {
    for (const name of ['codex-review-gate', 'bugbot-review-gate']) {
      assert.equal(fixture.checks.find((check) => check.name === name).status, 'in_progress');
    }
    const human = fixture.checks.find((check) => check.name === 'human-review-gate');
    if (++sleeps === 1) {
      assert.equal(human.conclusion, 'success');
      assert.match(human.output.summary, /Verified human evidence fingerprint: [a-f0-9]{64}/);
      options.comments.push({ ...authorApproval, id: 2, body: '/unapprove', created_at: new Date(now + 30000).toISOString() });
    } else {
      assert.equal(human.conclusion, 'failure');
      options.noCodex = false;
      unavailable = false;
    }
  };
  const result = await run(options);
  assert.deepEqual(result.failures, []);
  assert.equal(sleeps, 2);
  assert.equal(result.checks.find((check) => check.name === 'human-review-gate').conclusion, 'failure');
});

test('AI and CI progress updates cannot invalidate unchanged human clearance', async () => {
  let revision = 0;
  let pending = true;
  let sleeps = 0;
  const result = await run({ files: [{ filename: 'ordinary-config.yaml' }], comments: [authorApproval],
    respond: (route, _args, data) => {
      const progress = `Progress update ${++revision}`;
      const updatedAt = new Date(now + revision).toISOString();
      if (route === 'GET /repos/{owner}/{repo}/actions/runs') {
        for (const run of data.workflow_runs) Object.assign(run, { updated_at: updatedAt,
          ...(pending ? { status: 'in_progress', conclusion: null } : {}) });
      }
      if (route === 'GET /repos/{owner}/{repo}/commits/{ref}/check-runs') {
        const cursor = data.check_runs.find((check) => check.name === 'Cursor Bugbot');
        Object.assign(cursor, { output: { summary: progress }, ...(pending ? { status: 'in_progress', conclusion: null } : {}) });
      }
      if (route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews') {
        return [{ ...codexReview, body: progress, ...(pending ? { state: 'PENDING', submitted_at: null } : {}) },
          { id: 2, user: { login: 'reviewer', type: 'User' }, state: 'APPROVED', submitted_at: at, body: progress }];
      }
      if (route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/reactions') {
        return [{ id: revision, user: { login: 'reviewer', type: 'User' }, content: '+1', created_at: updatedAt }];
      }
      if (route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/comments') {
        return [...data, { id: 2, user: { login: 'cursor[bot]', type: 'Bot' }, body: progress, created_at: at }];
      }
      return data;
    },
    afterSleep: (fixture) => {
      sleeps++;
      assert.equal(fixture.checks.find((check) => check.name === 'human-review-gate').conclusion, 'success');
      for (const name of ['codex-review-gate', 'bugbot-review-gate', 'required-ci-gate']) {
        assert.equal(fixture.checks.find((check) => check.name === name).status, 'in_progress');
      }
      pending = false;
    },
  });
  assert.deepEqual(result.failures, []);
  assert.equal(sleeps, 1);
  for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'success', name);
});

test('dismissal or changes requested during the AI wait replaces the earlier approval', async (t) => {
  for (const state of ['DISMISSED', 'CHANGES_REQUESTED']) await t.test(state, async () => {
    const approval = { id: 2, user: { login: 'reviewer', type: 'User' }, state: 'APPROVED', commit_id: head, submitted_at: at };
    const options = { files: [{ filename: 'ordinary-config.yaml' }], reviews: [approval] };
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
  const options = { noCodex: true,
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
  const options = { noCodex: true, noCI: true };
  options.afterSleep = () => { options.noCodex = false; options.noCI = false; };
  const result = await run(options);
  assert.deepEqual(result.failures, []);
  for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'success', name);
});

test('an author approval removed between final snapshots blocks merge clearance', async () => {
  let reads = 0;
  const result = await run({ files: [{ filename: 'ordinary-config.yaml' }], comments: [authorApproval],
    respond: (route, _args, data) => route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/comments' && ++reads >= 2 ? [] : data });
  assert.deepEqual(result.failures, []);
  assert.equal(result.checks.find((check) => check.name === 'human-review-gate').conclusion, 'failure');
  assert(!result.calls.some((call) => call.args.output?.title === 'human-review-gate: success'));
});

test('discussion-only comment updates cannot invalidate human clearance', async () => {
  let reads = 0;
  const result = await run({ respond: (route, _args, data) => route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/comments'
    ? [{ ...authorApproval, body: `Discussion update ${++reads}` }] : data });
  assert.deepEqual(result.failures, []);
  for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'success', name);
});

test('human confirmation evidence that keeps changing cannot produce successful gates', async () => {
  let reads = 0;
  const result = await run({ respond: (route, _args, data) => route === 'GET /repos/{owner}/{repo}/issues/{issue_number}/comments'
    ? (++reads % 2 ? [authorApproval] : []) : data });
  assert.equal(result.failures.length, 1);
  assert.match(result.failures[0], /Human evidence kept changing/);
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
  assert.equal(result.statuses.find((status) => status.context === 'required-ci-gate').state, 'pending');
});

test('head movement never publishes success after the movement', async () => {
  const result = await run({ moveHead: true });
  assert(!result.calls.some((call) => call.route.startsWith('PATCH ') && call.args.conclusion === 'success'));
  assert(!result.statuses.some((status) => status.state === 'success'));
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
  assert.deepEqual(await targets({ github, context: { ...context, eventName: 'workflow_run', payload: { action: 'completed', workflow_run: { head_sha: head } } } }), [1]);
});

test('CI completion and rerun start wake the controller; initial CI start does not restart it', async () => {
  const workflow = fs.readFileSync('.github/workflows/review-policy.yml', 'utf8');
  assert.match(workflow, /types: \[in_progress, completed\]/);
  assert.match(workflow, /github\.event\.workflow_run\.run_attempt > 1/);
  let calls = 0;
  const github = { request: async () => {
    calls++;
    return { data: [{ number: 1, state: 'open', base: { ref: 'main' }, head: { sha: head } }] };
  } };
  for (const [action, run_attempt, expected] of [
    ['requested', 1, []], ['in_progress', 1, []], ['in_progress', 2, [1]], ['completed', 1, [1]],
  ]) {
    const payload = { action, workflow_run: { head_sha: head, run_attempt } };
    assert.deepEqual(await targets({ github, context: { ...context, eventName: 'workflow_run', payload } }), expected);
  }
  assert.equal(calls, 2);
});

test('conditional reads validate cached data with GitHub and isolate pagination and caller mutations', async () => {
  let version = 1;
  let thrown = false;
  const github = { request: async (_route, args) => {
    const etag = `"${args.page}-${version}"`;
    if (args.headers?.['if-none-match'] === etag) {
      if (thrown) throw Object.assign(new Error('Not modified'), { status: 304 });
      thrown = true;
      return { status: 304 };
    }
    return { status: 200, headers: { etag }, data: [{ page: args.page, version }] };
  } };
  const api = new Api(github, repo);
  const route = 'GET /repos/{owner}/{repo}/pulls';
  (await api.call(route, { page: 1 }))[0].version = 99;
  assert.deepEqual(await api.call(route, { page: 1 }), [{ page: 1, version: 1 }]);
  assert.deepEqual(await api.call(route, { page: 1 }), [{ page: 1, version: 1 }]);
  assert.deepEqual(await api.call(route, { page: 2 }), [{ page: 2, version: 1 }]);
  version++;
  assert.deepEqual(await api.call(route, { page: 1 }), [{ page: 1, version: 2 }]);
});

test('requests are serialized and an ETag never hides an authorization error', async () => {
  let active = 0;
  let maximum = 0;
  let denied = false;
  const api = new Api({ request: async () => {
    maximum = Math.max(maximum, ++active);
    await Promise.resolve();
    active--;
    if (denied) throw Object.assign(new Error('Forbidden'), { status: 403 });
    return { data: [], headers: { etag: '"same"' } };
  } }, repo);
  await Promise.all([api.call('GET one'), api.call('GET two'), api.call('GET one')]);
  assert.equal(maximum, 1);
  denied = true;
  await assert.rejects(() => api.call('GET one'), /Forbidden/);
});

test('polling avoids duplicate AI/CI reads and unchanged historical check writes', async () => {
  const checks = Array.from({ length: 10 }, (_, index) => GATES.map((name, id) => ({
    id: 10 + index * 4 + id, name, head_sha: head, app: { slug: 'github-actions' },
    external_id: `review-policy:worldcoin/world-chain:1:${head}`, status: 'completed', conclusion: 'success',
  }))).flat();
  let elapsed = 0;
  const options = { checks, noCodex: true,
    afterSleep: (_fixture, ms) => {
      assert.equal(ms, 60000);
      elapsed += ms;
      if (elapsed >= 5 * 60000) options.noCodex = false;
    } };
  const result = await run(options);
  assert.deepEqual(result.failures, []);
  const count = (suffix) => result.calls.filter((call) => call.route.endsWith(suffix)).length;
  assert.equal(count('/actions/runs'), 6);
  assert.equal(count('/reactions'), 6);
  assert.equal(count('/reviews'), 12, 'human decisions still get a second fresh read');
  assert.equal(count('/permission'), 12);
  assert.equal(count('/jobs'), 2, 'completed jobs are immutable for the observed run attempt');
  const oldCodex = checks.find((check) => check.name === 'codex-review-gate');
  const writes = result.calls.filter((call) => call.route.startsWith('PATCH ') && call.args.check_run_id === oldCodex.id);
  assert.deepEqual(writes.map((call) => call.args.conclusion), ['failure', 'success']);
  // Four gates add eight status writes and twelve final identity reads.
  assert(result.calls.length < 270, `API budget exceeded: ${result.calls.length} requests`);
  for (const name of GATES) assert.equal(result.statuses.filter((status) => status.context === name).length, 2);
});

test('one-minute polling detects a new Codex thumbs-up without requiring a webhook', async () => {
  let sleeps = 0;
  const options = { noCodex: true, afterSleep: (_fixture, ms) => {
    assert.equal(ms, 60000);
    assert.equal(++sleeps, 1);
    options.reactions = [{ content: '+1', user: codexReview.user, created_at: new Date(now + 59000).toISOString() }];
  } };
  const result = await run(options);
  assert.deepEqual(result.failures, []);
  assert.equal(sleeps, 1);
  assert.equal(result.checks.find((check) => check.name === 'codex-review-gate').conclusion, 'success');
});

test('the last poll uses the remaining time without shortening the 30-minute AI budget', async () => {
  process.env.REVIEW_POLICY_BASE_SHA = base;
  const fixture = fake({ noCodex: true });
  const sleeps = [];
  let clock = now;
  await evaluate({ github: fixture.github, context, core: fixture.core, number: 1, now: () => clock,
    sleep: async (ms) => {
      assert(ms > 0 && ms <= 60000);
      sleeps.push(ms);
      clock += ms;
      if (sleeps.length === 1) clock += 25000;
    },
  });
  assert.deepEqual(fixture.failures, []);
  assert.equal(sleeps.length, 30);
  assert.equal(sleeps.at(-1), 35000);
  assert.equal(clock - now, 30 * 60000);
  assert.equal(fixture.checks.find((check) => check.name === 'codex-review-gate').conclusion, 'failure');
});

test('completed job cache is invalidated by a new CI attempt even with the same run conclusion', async () => {
  let rerun = false;
  const options = { noCodex: true,
    afterSleep: () => { rerun = true; options.noCodex = false; },
    respond: (route, args, data) => {
      if (rerun && route.endsWith('/actions/runs')) data.workflow_runs[0].run_attempt = 2;
      if (rerun && route.endsWith('/jobs') && args.run_id === 1) data.jobs[0].conclusion = 'skipped';
      return data;
    },
  };
  const result = await run(options);
  assert.deepEqual(result.failures, []);
  assert.equal(result.calls.filter((call) => call.route.endsWith('/jobs')).length, 3);
  assert.equal(result.checks.find((check) => check.name === 'required-ci-gate').conclusion, 'failure');
});

function rateLimit(headers = {}, status = 403) {
  return Object.assign(new Error('API rate limit exceeded'), { status, response: { headers } });
}

test('rate-limit reset restarts from fresh evidence instead of retrying a stale success publication', async () => {
  let throttled = false;
  let waits = 0;
  const options = { files: [{ filename: 'ordinary-config.yaml' }], comments: [authorApproval],
    respond: (route, args, data) => {
      if (!throttled && route.startsWith('PATCH ') && args.output.title === 'human-review-gate: success') {
        throttled = true;
        throw rateLimit({ 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': String(now / 1000 + 120) });
      }
      return data;
    },
    afterSleep: (_fixture, ms) => {
      waits++;
      assert.equal(ms, 121000);
      options.comments = [];
    },
  };
  const result = await run(options);
  assert.equal(waits, 1);
  assert.deepEqual(result.failures, []);
  assert(result.messages.some((message) => message.includes('rate limit')));
  assert.equal(result.checks.filter((check) => check.name === 'review-policy-observed').length, 1);
  for (const check of result.checks.filter((check) => check.name === 'human-review-gate')) assert.equal(check.conclusion, 'failure');
  for (const name of GATES.filter((name) => name !== 'human-review-gate')) {
    assert.equal(result.checks.filter((check) => check.name === name).at(-1).conclusion, 'success');
  }
});

test('rate-limit recovery respects head changes and does not publish clearance on the old head', async () => {
  let throttled = false;
  const result = await run({ respond: (route, _args, data) => {
    if (!throttled && route.endsWith('/reviews')) { throttled = true; throw rateLimit({ 'retry-after': '90' }, 429); }
    return data;
  }, afterSleep: (fixture, ms) => {
    assert.equal(ms, 90000);
    fixture.pr.state = 'closed';
    fixture.pr.head.sha = 'new-head';
  } });
  assert.deepEqual(result.failures, []);
  assert(!result.calls.some((call) => call.route.startsWith('PATCH ') && call.args.conclusion === 'success'));
});

test('a throttled commit-status success is re-evaluated after an approval is revoked', async () => {
  let throttled = false;
  const options = { files: [{ filename: 'ordinary-config.yaml' }], comments: [authorApproval],
    respond: (route, args, data) => {
      if (!throttled && route.endsWith('/statuses/{sha}') && args.context === 'human-review-gate' && args.state === 'success') {
        throttled = true;
        throw rateLimit({ 'retry-after': '60' }, 429);
      }
      return data;
    }, afterSleep: () => { options.comments = []; },
  };
  const result = await run(options);
  assert(throttled);
  assert.deepEqual(result.failures, []);
  assert.equal(result.statuses.find((status) => status.context === 'human-review-gate').state, 'failure');
  assert(result.checks.filter((check) => check.name === 'human-review-gate').every((check) => check.conclusion === 'failure'));
});

test('team-token rate limits recover separately from missing membership permissions', async () => {
  let reads = 0;
  let waits = 0;
  const result = await run({ codeowners: '* @worldcoin/protocol', members: [{ login: 'owner', type: 'User' }], teamAccess: true,
    respond: (route, _args, data) => {
      if (route.endsWith('/members') && ++reads === 1) throw rateLimit({ 'retry-after': '60' }, 429);
      return data;
    }, afterSleep: (_fixture, ms) => { waits++; assert.equal(ms, 60000); },
  });
  assert.deepEqual(result.failures, []);
  assert.equal(waits, 1);
  assert(result.messages.some((message) => message.includes('team membership API rate limit')));
  assert.equal(result.checks.filter((check) => check.name === 'human-review-gate').at(-1).conclusion, 'success');
});

test('persistent throttling is bounded and permission errors do not trigger quota retries', async (t) => {
  for (const [name, error, expectedWaits, message] of [
    ['secondary', rateLimit({}, 429), [60000, 120000, 240000], /recovery budget exhausted/],
    ['too distant reset', rateLimit({ 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': String(now / 1000 + 7200) }), [], /recovery budget exhausted/],
    ['forbidden', Object.assign(new Error('Forbidden'), { status: 403 }), [], /Forbidden/],
  ]) await t.test(name, async () => {
    const waits = [];
    const result = await run({ respond: (route, _args, data) => {
      if (route.endsWith('/reviews')) throw error;
      return data;
    }, afterSleep: (_fixture, ms) => waits.push(ms) });
    assert.deepEqual(waits, expectedWaits);
    assert.equal(result.failures.length, 1);
    assert.match(result.failures[0], message);
  });
});

test('rate-limit waiting preserves the remaining AI budget instead of expiring or resetting it', async () => {
  let polls = 0;
  let throttled = false;
  let rateWaits = 0;
  const result = await run({ noCodex: true,
    respond: (route, _args, data) => {
      if (!throttled && polls === 1 && route.endsWith('/reviews')) {
        throttled = true;
        throw rateLimit({ 'retry-after': '2400' }, 429);
      }
      return data;
    }, afterSleep: (_fixture, ms) => {
      if (ms === 60000) assert(++polls <= 30, 'recovery must not restart the AI timeout');
      else { assert.equal(ms, 2400000); rateWaits++; }
    },
  });
  assert.deepEqual(result.failures, []);
  assert.equal(rateWaits, 1);
  assert.equal(polls, 30, 'quota waiting must not consume the AI timeout');
  const gate = result.checks.filter((check) => check.name === 'codex-review-gate').at(-1);
  assert.equal(gate.conclusion, 'failure');
  assert.match(gate.output.summary, /No completed review after 30 minutes/);
});

test('a rate-limit response stops already queued requests until recovery', async () => {
  let calls = 0;
  const api = new Api({ request: async () => { calls++; throw rateLimit({}, 429); } }, repo);
  const results = await Promise.allSettled([api.call('GET one'), api.call('GET two'), api.call('POST three')]);
  assert(results.every((result) => result.status === 'rejected'));
  assert.equal(calls, 1);
});

test('target discovery also waits for the quota reset before retrying', async () => {
  let attempts = 0;
  let clock = now;
  const github = { request: async () => {
    if (++attempts === 1) throw rateLimit({ 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': String(now / 1000 + 600) });
    return { data: [{ number: 1 }] };
  } };
  const result = await targets({ github, context: { ...context, eventName: 'push', payload: { before: '0'.repeat(40), after: base } }, now: () => clock,
    sleep: async (ms) => { assert.equal(ms, 601000); clock += ms; } });
  assert.deepEqual(result, [1]);
  assert.equal(attempts, 2);
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

test('ordinary main pushes do not list or re-evaluate open PRs', async () => {
  const fixture = fake();
  const result = await targets({ github: fixture.github, context: { ...context, eventName: 'push',
    payload: { before: base, after: 'ordinary-main' } } });
  assert.deepEqual(result, []);
  assert.equal(fixture.calls.length, 5);
  assert(fixture.calls.every((call) => call.route.endsWith('/git/trees/{tree_sha}')));
});

test('policy, ownership and workflow changes refresh open PRs, including additions and deletions', async (t) => {
  for (const path of ['CODEOWNERS', '.github/CODEOWNERS', 'docs/CODEOWNERS', '.github/review-policy.json',
    '.github/workflows', '.github/actions', 'scripts/review-policy']) {
    for (const change of ['replacement', null]) await t.test(`${path}: ${change}`, async () => {
      const fixture = fake({ policyVersions: { [base]: { [path]: 'original' }, updated: { [path]: change } } });
      const result = await targets({ github: fixture.github, context: { ...context, eventName: 'push',
        payload: { before: base, after: 'updated' } } });
      assert.deepEqual(result, [1]);
    });
  }
  const fixture = fake({ policyVersions: { added: { '.github/CODEOWNERS': 'new-owner-file' } } });
  assert.deepEqual(await targets({ github: fixture.github, context: { ...context, eventName: 'push',
    payload: { before: base, after: 'added' } } }), [1]);
});

test('incomplete policy trees fail closed rather than skipping a required refresh', async () => {
  const fixture = fake({ respond: (route, _args, data) => route.endsWith('/git/trees/{tree_sha}')
    ? { ...data, truncated: true } : data });
  await assert.rejects(() => targets({ github: fixture.github, context: { ...context, eventName: 'push',
    payload: { before: base, after: 'ordinary-main' } } }), /Incomplete Git tree/);
  assert(!fixture.calls.some((call) => call.route.endsWith('/pulls')));
});

test('unreadable team membership cannot grant owner fast path', async () => {
  const api = { call: async (route) => {
    if (route.includes('collaborators')) return { permission: 'write' };
  }, all: async () => {
    throw Object.assign(new Error('Forbidden'), { status: 403 });
  } };
  await assert.rejects(() => people(api, { user: { login: 'owner', type: 'User' } }, [], [{ requiredOwners: ['@worldcoin/team'] }]), /Cannot verify team/);
});

test('repository operations and team membership use separate authenticated clients', async () => {
  const repoCalls = [];
  const teamCalls = [];
  const api = new Api({ request: async (route, args) => { repoCalls.push({ route, args }); return { data: {} }; } }, repo,
    { request: async (route, args) => { teamCalls.push({ route, args }); return { data: [] }; } });
  await api.call('POST /repos/{owner}/{repo}/check-runs', { name: 'human-review-gate' });
  await api.all('GET /orgs/{org}/teams/{team_slug}/members', { org: 'worldcoin', team_slug: 'protocol' });
  assert.equal(repoCalls.length, 1);
  assert.equal(teamCalls.length, 1);
  assert.equal(repoCalls[0].args.repo, 'world-chain');
  assert.equal(teamCalls[0].args.repo, undefined);
  assert.equal(teamCalls[0].args.request.timeout, 20000);
  await assert.rejects(() => new Api({}, repo).call('GET /orgs/{org}/teams/{team_slug}/members'), /REVIEW_POLICY_TEAM_TOKEN/);
});

test('protocol members get the owner fast path and outsiders need a protocol owner approval', async () => {
  const members = ['owner', 'reviewer'].map((login) => ({ login, type: 'User' }));
  const result = await run({ codeowners: '* @worldcoin/protocol', members, teamAccess: true });
  assert.deepEqual(result.failures, []);
  const gate = (value) => value.checks.find((check) => check.name === 'human-review-gate');
  assert.equal(gate(result).conclusion, 'success');
  assert.match(gate(result).output.summary, /Owner fast path: true/);
  const outsider = await run({ codeowners: '* @worldcoin/protocol', members: members.slice(1), teamAccess: true });
  assert.deepEqual(outsider.failures, []);
  assert.equal(gate(outsider).conclusion, 'failure');
  const covered = await run({ codeowners: '* @worldcoin/protocol', members: members.slice(1), teamAccess: true,
    reviews: [codexReview, { id: 2, user: members[1], state: 'APPROVED', submitted_at: at }] });
  assert.equal(gate(covered).conclusion, 'success');
});

test('critical protocol changes still need two distinct humans from the agreed confirmation policy', async () => {
  const result = await run({ codeowners: '* @worldcoin/protocol', teamAccess: true,
    members: ['owner', 'reviewer'].map((login) => ({ login, type: 'User' })),
    files: [{ filename: 'CODEOWNERS' }], comments: [authorApproval],
    reviews: [codexReview, { id: 2, user: { login: 'reviewer', type: 'User' }, state: 'APPROVED', submitted_at: at }] });
  const gate = result.checks.find((check) => check.name === 'human-review-gate');
  assert.equal(gate.conclusion, 'success');
  assert.match(gate.output.summary, /human confirmations: reviewer, owner; required: 2/);
});

test('membership removals during AI review invalidate earlier owner privileges', async () => {
  const members = [{ login: 'owner', type: 'User' }];
  const result = await run({ codeowners: '* @worldcoin/protocol', members, teamAccess: true, noCodex: true,
    afterSleep: () => { members.length = 0; },
    respond: (route, args, data) => route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews' && !members.length ? [codexReview] : data,
  });
  assert.deepEqual(result.failures, []);
  assert.equal(result.checks.find((check) => check.name === 'human-review-gate').conclusion, 'failure');
});

test('new team members gain owner privileges on the next evidence read without activation', async () => {
  const members = [];
  const result = await run({ codeowners: '* @worldcoin/protocol', members, teamAccess: true, noCodex: true,
    afterSleep: () => { members.push({ login: 'owner', type: 'User' }); },
    respond: (route, args, data) => route === 'GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews' && members.length ? [codexReview] : data,
  });
  assert.deepEqual(result.failures, []);
  assert.equal(result.checks.find((check) => check.name === 'human-review-gate').conclusion, 'success');
});

test('team ownership without a membership credential fails closed with a configuration error', async () => {
  const result = await run({ codeowners: '* @worldcoin/protocol' });
  assert.match(result.failures.join('\n'), /REVIEW_POLICY_TEAM_TOKEN/);
  for (const name of GATES) assert.equal(result.checks.find((check) => check.name === name).conclusion, 'failure');
});

test('activation permits bypass of AI and CI checks while protecting human approvals and threads', () => {
  const rule = rulesetBody();
  assert.deepEqual(rule.bypass_actors, []);
  const ci = ciRulesetBody([]);
  assert.deepEqual(rule.rules[0].parameters.required_status_checks.map((check) => check.context), ['human-review-gate']);
  assert.deepEqual(ci.rules[0].parameters.required_status_checks.map((check) => check.context), ['codex-review-gate', 'bugbot-review-gate', 'required-ci-gate']);
  assert.equal(rule.rules[1].parameters.required_review_thread_resolution, true);
  assert.equal(rule.rules[1].parameters.dismiss_stale_reviews_on_push, false);
  assert.deepEqual(reviewSettings({ required_pull_request_reviews: { required_approving_review_count: 1, require_code_owner_reviews: true, dismiss_stale_reviews: false, require_last_push_approval: false } }), {
    required_approving_review_count: 1, require_code_owner_reviews: true, dismiss_stale_reviews: false, require_last_push_approval: false,
  });
});
