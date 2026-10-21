'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const p = require('./policy.cjs');
const config = require('../../.github/review-policy.json');
const sha = 'head';
const at = '2026-10-19T10:00:00Z';
const user = (login) => ({ login, type: 'User' });
const bot = (login) => ({ login, type: 'Bot' });
const review = (login, state = 'APPROVED', extra = {}) => ({ id: 1, user: user(login), state, submitted_at: at, commit_id: sha, ...extra });
const command = (body, extra = {}) => ({ id: 1, user: user('author'), body, created_at: at, ...extra });

function humans({ risk = 'low', owned = true, reviews = [], comments = [], writable = new Set(['author', 'owner', 'second']), ...extra } = {}) {
  return p.humanGate({ pr: { user: user('author') }, risk,
    paths: p.ownership(['src/a.rs'], p.parseCodeowners(`* @owner @second ${owned ? '@author' : ''}`)),
    ownerMembers: new Map(['author', 'owner', 'second'].map((login) => [`@${login}`, new Set([login])])),
    reviews, comments, writable, ...extra });
}

test('agreed risks include exceptions, manifests, defaults and measured fixtures', () => {
  for (const [path, expected] of [
    ['pkg/contracts/src/dispute/lib/GameTypes.sol', 'critical'], ['pkg/contracts/src/fees/FeeEscrow.sol', 'important'],
    ['crates/pbh/src/lib.rs', 'low'], ['proofs/measurements.json', 'important'],
    ['crates/builder/Cargo.toml', 'critical'], ['crates/chainspec/src/hardfork.rs', 'critical'],
    ['proofs/protocol/src/types.rs', 'critical'], ['proofs/services/defender/src/lane.rs', 'critical'],
    ['proofs/services/proposer/src/lib.rs', 'important'], ['crates/builder/tests/regression.rs', 'low'],
    ['proofs/measured/core/tests/case.rs', 'critical'], ['docs/proof/release.md', 'important'],
    ['specs/protocol.md', 'important'], ['docs/introduction.md', 'low'], ['brand-new-service/main.rs', 'important'],
    ['scripts/review-policy/policy.test.cjs', 'critical'], ['.github/workflows/example.yml', 'critical'],
    ['pkg/contracts/scripts/mainnet/Deploy.s.sol', 'critical'], ['Cargo.toml', 'critical'], ['Cargo.lock', 'important'],
  ]) assert.equal(p.classify([path], config).risk, expected, path);
  const paths = p.changedPaths([{ filename: 'docs/old.md', previous_filename: 'crates/evm/src/lib.rs' }, { filename: 'README.md', status: 'removed' }]);
  assert.equal(p.classify(paths, config).risk, 'critical');
  assert.throws(() => p.classify([], config));
});

test('CODEOWNERS precedence, anchored and recursive patterns, empty owners and fallback', () => {
  const rules = p.parseCodeowners('* @global\n/src/ @src\n/src/private/ @private @other\n*.js @javascript\n/src/unowned/\n# comment\n');
  const entries = p.ownership(['src/a.rs', 'src/private/a.rs', 'other/src/a.rs', 'other/deep/a.js', 'src/unowned/a.rs'], rules);
  assert.deepEqual(entries.map((entry) => entry.owners), [['@src'], ['@private', '@other'], ['@global'], ['@javascript'], []]);
  assert.deepEqual(entries.at(-1).requiredOwners, ['@global']);
  assert.throws(() => p.parseCodeowners('[ab]* @owner'));
  assert.throws(() => p.parseCodeowners('!foo @owner'));
  assert.throws(() => p.parseCodeowners('* owner@example.org'));
  assert.equal(p.matches('a/b/c.rs', '**/*.rs'), true);
  assert.equal(p.matches('Cargo.toml', '**/Cargo.toml'), true);
  assert.equal(p.matches('a/x.rs', 'a/*.rs'), true);
  assert.equal(p.matches('a/b/x.rs', 'a/*.rs'), false);
});

test('owner fast path only applies to low risk; important needs confirmation', () => {
  assert.equal(humans().state, 'success');
  assert.equal(humans({ risk: 'important' }).state, 'failure');
  assert.equal(humans({ risk: 'important', comments: [command('/approve')] }).state, 'success');
  assert.equal(humans({ risk: 'important', reviews: [review('owner')] }).state, 'success');
});

test('non owners always need covering owner approval, including on low-risk PRs', () => {
  assert.equal(humans({ owned: false, comments: [command('/approve')] }).state, 'failure');
  assert.equal(humans({ owned: false, reviews: [review('owner')] }).state, 'success');
  const paths = p.ownership(['a/x', 'b/y'], p.parseCodeowners('* @global\na/ @author\nb/ @owner'));
  assert.equal(humans({ paths, reviews: [review('owner')] }).state, 'success');
  assert.equal(humans({ paths }).state, 'failure');
});

test('critical needs two distinct humans and owner coverage; risk escalation preserves confirmations', () => {
  assert.equal(humans({ risk: 'critical', comments: [command('/approve')], reviews: [review('owner')] }).state, 'success');
  assert.equal(humans({ risk: 'critical', owned: false, comments: [command('/approve')], reviews: [review('owner')] }).state, 'success');
  assert.equal(humans({ risk: 'critical', reviews: [review('owner'), review('second')] }).state, 'success');
  assert.equal(humans({ risk: 'critical', reviews: [review('owner'), review('owner', 'APPROVED', { id: 2 })] }).state, 'failure');
  assert.equal(humans({ risk: 'critical', comments: [command('/approve')] }).state, 'failure');
});

test('human approvals persist after push, comments do not dismiss, changes requested and dismissal revoke', () => {
  const older = review('owner', 'APPROVED', { commit_id: 'old-head' });
  assert.equal(humans({ risk: 'important', reviews: [older, review('owner', 'COMMENTED', { id: 2 })] }).state, 'success');
  assert.equal(humans({ reviews: [older, review('owner', 'CHANGES_REQUESTED', { id: 2 })] }).state, 'failure');
  assert.equal(humans({ risk: 'important', reviews: [review('owner', 'DISMISSED')] }).state, 'failure');
  assert.equal(humans({ reviews: [review('owner', 'CHANGES_REQUESTED'), review('owner', 'APPROVED', { id: 2 })] }).state, 'success');
});

test('author approval is an exact standalone author command; revocation, deletion and bot exclusion', () => {
  for (const text of ['> /approve', 'please /approve', '/approve extra', '```\n/approve\n```', '/signoff', '/unsignoff']) assert.equal(p.authorApproved([command(text)], 'author'), false);
  assert.equal(p.authorApproved([command('  /approve\n')], 'author'), true);
  assert.equal(p.authorApproved([command('/approve', { user: user('other') })], 'author'), false);
  assert.equal(p.authorApproved([command('/approve', { user: bot('author') })], 'author'), false);
  assert.equal(p.authorApproved([command('/approve'), command('/unapprove', { id: 2 })], 'author'), false);
  assert.equal(p.authorApproved([command('/approve'), command('/unapprove', { id: 2 }), command('/approve', { id: 3 })], 'author'), true);
  assert.equal(p.authorApproved([], 'author'), false);
  assert.equal(humans({ risk: 'important', comments: [command('/approve')], writable: new Set() }).state, 'failure');
  assert.equal(humans({ pr: { user: bot('author') } }).state, 'failure');
});

test('CODEX only accepts current SHA reviews or its own fresh thumbs-up', () => {
  const codex = (extra) => review('chatgpt-codex-connector[bot]', 'COMMENTED', extra);
  assert.equal(p.codexGate([codex()], [], sha, at).state, 'success');
  for (const extra of [{ commit_id: 'old' }, { state: 'PENDING' }, { state: 'DISMISSED' }]) assert.equal(p.codexGate([codex(extra)], [], sha, at).state, 'pending');
  const reaction = { user: bot('chatgpt-codex-connector[bot]'), content: '+1', created_at: at };
  assert.equal(p.codexGate([], [reaction], sha, at).state, 'success');
  assert.equal(p.codexGate([], [{ ...reaction, created_at: '2026-10-18T10:00:00Z' }], sha, at).state, 'pending');
  assert.equal(p.codexGate([], [{ ...reaction, user: user('author') }], sha, at).state, 'pending');
  assert.equal(p.codexGate([], [{ ...reaction, content: 'eyes' }], sha, at).state, 'pending');
});

test('Bugbot neutral requires a matching completed review, errors and cancellations block', () => {
  const check = { id: 1, name: 'Cursor Bugbot', app: { slug: 'cursor' }, head_sha: sha, status: 'completed', conclusion: 'success', started_at: at };
  const findings = review('cursor[bot]', 'COMMENTED', { body: '<!-- BUGBOT_REVIEW -->\nCursor Bugbot has reviewed your changes using default effort and found 1 potential issue.' });
  assert.equal(p.bugbotGate([check], [], sha).state, 'success');
  assert.equal(p.bugbotGate([{ ...check, conclusion: 'neutral' }], [findings], sha).state, 'success');
  assert.equal(p.bugbotGate([{ ...check, conclusion: 'neutral' }], [], sha).state, 'failure');
  assert.equal(p.bugbotGate([{ ...check, conclusion: 'neutral' }], [{ ...findings, commit_id: 'old' }], sha).state, 'failure');
  assert.equal(p.bugbotGate([{ ...check, conclusion: 'cancelled' }], [findings], sha).state, 'failure');
  assert.equal(p.bugbotGate([{ ...check, app: { slug: 'fake' } }], [], sha).state, 'pending');
  assert.equal(p.bugbotGate([{ ...check, head_sha: 'old' }], [], sha).state, 'pending');
  assert.equal(p.bugbotGate([check, { ...check, id: 2, status: 'in_progress' }], [findings], sha).state, 'pending');
});

test('CI requires the current PR workflow and successful individual jobs, not skipped jobs', () => {
  const pr = { number: 1, head: { sha, ref: 'branch', repo: { id: 5 } } };
  const groups = [{ id: 'rust', workflow: 'rust-ci.yml', jobs: ['tests', 'clippy'] }];
  const ids = { 'rust-ci.yml': 10 };
  const run = { id: 1, workflow_id: 10, head_sha: sha, event: 'pull_request', pull_requests: [{ number: 1 }], status: 'completed', conclusion: 'success' };
  const jobs = new Map([[1, ['tests', 'clippy'].map((name) => ({ name, status: 'completed', conclusion: 'success' }))]]);
  const evaluate = (runs, data = jobs) => p.ciGate(groups, runs, data, ids, pr).state;
  assert.equal(evaluate([run]), 'success');
  assert.equal(evaluate([]), 'pending');
  assert.equal(evaluate([{ ...run, conclusion: 'cancelled' }]), 'failure');
  assert.equal(evaluate([{ ...run, head_sha: 'old' }]), 'pending');
  assert.equal(evaluate([{ ...run, workflow_id: 999 }]), 'pending');
  assert.equal(evaluate([{ ...run, event: 'workflow_dispatch' }]), 'pending');
  assert.equal(evaluate([run], new Map([[1, [{ name: 'tests', status: 'completed', conclusion: 'skipped' }]]])), 'failure');
  assert.equal(evaluate([{ ...run, pull_requests: [], head_branch: 'branch', head_repository: { id: 5 } }]), 'success');
  assert.equal(evaluate([{ ...run, pull_requests: [], head_branch: 'branch', head_repository: { id: 6 } }]), 'pending');
});

test('CI is selected from both rename paths and shared build inputs', () => {
  for (const path of ['Cargo.toml', 'justfiles/proof.just', 'rust-toolchain.toml']) assert(p.requiredCi([path], config).some((group) => group.id === 'sp1'));
  for (const path of ['Justfile', 'justfiles/contracts.just', '.gitmodules']) assert(p.requiredCi([path], config).some((group) => group.id === 'contracts'));
  assert.deepEqual(p.requiredCi(['README.md'], config).map((group) => group.id), ['policy', 'large-files']);
});

test('every configured CI selector is present in its workflow, including measurement job selectors', () => {
  for (const group of config.ci) {
    if (['policy', 'large-files'].includes(group.id)) continue;
    const text = fs.readFileSync(`.github/workflows/${group.workflow}`, 'utf8');
    const triggers = text.split('\njobs:')[0];
    const selectors = [...triggers.matchAll(/^\s+- ["']([^"']+)["']/gm)].map((match) => match[1]);
    for (const pattern of group.patterns) assert(selectors.some((trigger) => p.matches(pattern, trigger)), `${group.id}: missing trigger ${pattern}`);
    if (['sp1', 'nitro'].includes(group.id)) {
      const section = text.split(`            ${group.id}:\n`)[1]?.split(/\n            \w+:\n/)[0];
      assert(section, `${group.id}: no measurement selector`);
      for (const pattern of group.patterns) assert(section.includes(`'${pattern}'`), `${group.id}: missing job selector ${pattern}`);
    }
  }
});
