'use strict';

const fs = require('node:fs');
const { createHash } = require('node:crypto');
const policy = require('./policy.cjs');

function fingerprint(value) {
  const canonical = (item) => Array.isArray(item) ? item.map(canonical)
    : item && typeof item === 'object' ? Object.fromEntries(Object.keys(item).sort().map((key) => [key, canonical(item[key])])) : item;
  return createHash('sha256').update(JSON.stringify(canonical(value))).digest('hex');
}

class Api {
  constructor(github, repo, teamGithub) { this.github = github; this.repo = repo; this.teamGithub = teamGithub; }
  async call(route, args = {}) {
    const teamRequest = route.startsWith('GET /orgs/{org}/teams/{team_slug}');
    const client = teamRequest ? this.teamGithub : this.github;
    if (!client) throw new Error('Configure REVIEW_POLICY_TEAM_TOKEN with organization Members: read for team ownership checks.');
    return (await client.request(route, { ...(!teamRequest ? this.repo : {}), ...args, request: { timeout: 20000 } })).data;
  }
  async all(route, args = {}, key) {
    const result = [];
    for (let page = 1; page <= 100; page++) {
      const data = await this.call(route, { ...args, per_page: 100, page });
      const items = key ? data[key] : data;
      if (!Array.isArray(items)) throw new Error(`Invalid paginated response: ${route}`);
      result.push(...items);
      if (items.length < 100) return result;
    }
    throw new Error(`Pagination limit exceeded: ${route}`);
  }
  async content(path, ref) {
    const result = await this.call('GET /repos/{owner}/{repo}/contents/{path}', { path, ref });
    if (result.type !== 'file' || result.encoding !== 'base64' || result.size > 1000000) throw new Error(`Cannot read trusted ${path}`);
    return Buffer.from(result.content, 'base64').toString('utf8');
  }
}

async function targets({ github, context }) {
  const api = new Api(github, context.repo);
  const event = context.payload;
  if (context.eventName === 'push') {
    return (await api.all('GET /repos/{owner}/{repo}/pulls', { state: 'open', base: 'main' })).map((pr) => pr.number);
  }
  if (event.pull_request) return [event.pull_request.number];
  if (event.issue?.pull_request) {
    const command = /^\/(?:un)?approve$/;
    return [event.comment?.body, event.changes?.body?.from].some((body) => command.test((body || '').trim())) ? [event.issue.number] : [];
  }
  if (context.eventName === 'workflow_dispatch') {
    const number = Number(event.inputs?.pr);
    if (!Number.isSafeInteger(number) || number < 1) throw new Error('A valid PR number is required.');
    return [number];
  }
  const check = event.check_run;
  if (check && check.app?.slug !== 'cursor') return [];
  const sha = check?.head_sha || event.workflow_run?.head_sha;
  if (!sha) return [];
  const pulls = await api.all('GET /repos/{owner}/{repo}/commits/{commit_sha}/pulls', { commit_sha: sha });
  return pulls.filter((pr) => pr.state === 'open' && pr.base.ref === 'main' && pr.head.sha === sha).map((pr) => pr.number);
}

async function readCodeowners(api, ref) {
  for (const path of ['.github/CODEOWNERS', 'CODEOWNERS', 'docs/CODEOWNERS']) {
    try { return policy.parseCodeowners(await api.content(path, ref)); }
    catch (error) { if (error.status !== 404) throw error; }
  }
  throw new Error('No CODEOWNERS found on the target branch.');
}

async function people(api, pr, reviews, paths) {
  const writable = new Set();
  const candidates = [...new Set([pr.user, ...reviews.map((review) => review.user)].filter(policy.human).map((user) => user.login.toLowerCase()))];
  for (const username of candidates) {
    try {
      const result = await api.call('GET /repos/{owner}/{repo}/collaborators/{username}/permission', { username });
      if (['admin', 'maintain', 'write'].includes(result.permission) || result.user?.permissions?.push) writable.add(username);
    } catch (error) { if (error.status !== 404) throw error; }
  }
  const ownerMembers = new Map();
  for (const owner of new Set(paths.flatMap((path) => path.requiredOwners))) {
    if (!owner.includes('/')) { ownerMembers.set(owner, new Set([owner.slice(1)])); continue; }
    const [org, team_slug] = owner.slice(1).split('/');
    try {
      const members = await api.all('GET /orgs/{org}/teams/{team_slug}/members', { org, team_slug });
      ownerMembers.set(owner, new Set(members.filter(policy.human).map((user) => user.login.toLowerCase())));
    } catch (error) {
      throw new Error(`Cannot verify team ${owner} membership: ${error.status ? `HTTP ${error.status}` : error.message}; check REVIEW_POLICY_TEAM_TOKEN (organization Members: read).`);
    }
  }
  return { writable, ownerMembers };
}

function summary(pr, classification, results, evidenceFingerprint) {
  const lines = [`PR #${pr.number}; head ${pr.head.sha}; base ${pr.base.sha}; risk: ${classification?.risk || 'unknown'}.`];
  if (evidenceFingerprint) lines.push(`Verified evidence fingerprint: ${evidenceFingerprint}`);
  if (classification) {
    lines.push('', 'Paths setting the risk:');
    for (const item of classification.details.filter((item) => item.risk === classification.risk).slice(0, 40)) lines.push(`- ${item.path}`);
  }
  for (const [name, result] of Object.entries(results)) {
    lines.push('', `${name}: ${result.state}`, result.reason || '');
    if (result.fastPath !== undefined) lines.push(`Owner fast path: ${result.fastPath}; human confirmations: ${result.confirmations.join(', ') || 'none'}; required: ${result.required}`);
    lines.push(...(result.reasons || []).slice(0, 40));
  }
  return lines.join('\n').slice(0, 60000);
}

async function evaluate({ github, context, core, number, teamGithub,
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)), now = Date.now }) {
  const api = new Api(github, context.repo, teamGithub);
  const fullName = `${context.repo.owner}/${context.repo.repo}`;
  const getPr = async () => {
    const [pull, branch] = await Promise.all([
      api.call('GET /repos/{owner}/{repo}/pulls/{pull_number}', { pull_number: number }),
      api.call('GET /repos/{owner}/{repo}/branches/{branch}', { branch: 'main' }),
    ]);
    // The PR API can retain an older base SHA after main advances.
    return pull.base.ref === 'main' && pull.base.repo.full_name === fullName
      ? { ...pull, base: { ...pull.base, sha: branch.commit.sha } } : pull;
  };
  const pr = await getPr();
  if (pr.state !== 'open' || pr.base.ref !== 'main' || pr.base.repo.full_name !== fullName) return;
  const sha = pr.head.sha;
  const external = `review-policy:${fullName}:${number}:${sha}`;
  const checks = await api.all('GET /repos/{owner}/{repo}/commits/{ref}/check-runs', { ref: sha, filter: 'all' }, 'check_runs');
  const own = (name) => checks.filter((check) => check.name === name && check.app?.slug === 'github-actions' && check.external_id === external).sort((a, b) => a.id - b.id)[0];
  const link = `${context.serverUrl || 'https://github.com'}/${fullName}/actions/runs/${context.runId}`;
  const gateChecks = new Map();
  async function create(name, extra) {
    return api.call('POST /repos/{owner}/{repo}/check-runs', { name, head_sha: sha, external_id: external, details_url: link, ...extra });
  }
  async function publish(name, result, text) {
    const current = await getPr();
    if (current.state !== 'open' || current.head.sha !== sha || current.base.ref !== pr.base.ref) return false;
    if (result.state === 'success' && current.base.sha !== pr.base.sha) return false;
    let active = gateChecks.get(name);
    if (result.state === 'pending' && active.status === 'completed') {
      active = await create(name, { status: 'in_progress' });
      gateChecks.set(name, active);
    }
    const all = await api.all('GET /repos/{owner}/{repo}/commits/{ref}/check-runs', { ref: sha, filter: 'all' }, 'check_runs');
    const matching = all.filter((check) => check.name === name && check.app?.slug === 'github-actions' && check.external_id === external);
    if (matching.some((check) => check.id > active.id)) return false;
    for (const check of [active, ...matching.filter((check) => check.id !== active.id)]) {
      const superseded = check.id !== active.id && result.state === 'pending';
      const conclusion = superseded ? 'failure' : result.state === 'pending' ? undefined : result.state;
      const status = conclusion ? 'completed' : 'in_progress';
      const updated = await api.call('PATCH /repos/{owner}/{repo}/check-runs/{check_run_id}', {
        check_run_id: check.id, details_url: link, status,
        ...(conclusion ? { conclusion, completed_at: new Date(now()).toISOString() } : {}),
        output: { title: `${name}: ${result.state}`, summary: superseded ? `Superseded; awaiting the current evaluation.\n\n${text}` : text },
      });
      if (updated.status !== status || (updated.conclusion || undefined) !== conclusion) throw new Error(`GitHub did not publish the expected ${name} result.`);
      if (check.id === active.id) gateChecks.set(name, updated);
    }
    return true;
  }
  let classification;
  let results = {};
  try {
    let observation = own('review-policy-observed');
    if (!observation) observation = await create('review-policy-observed', {
      status: 'completed', conclusion: 'neutral', started_at: new Date(now()).toISOString(),
      output: { title: 'First observation of this PR head', summary: `Head ${sha}; PR #${number}.` },
    });
    for (const name of policy.GATES) {
      gateChecks.set(name, await create(name, { status: 'in_progress' }));
      await publish(name, { state: 'pending' }, `Re-evaluating PR #${number}, head ${sha}.`);
    }
    const checkoutSha = process.env.REVIEW_POLICY_BASE_SHA;
    if (!checkoutSha || checkoutSha !== pr.base.sha) throw new Error('Target branch changed since checkout; rerun Review policy.');
    const config = JSON.parse(fs.readFileSync('.github/review-policy.json', 'utf8'));
    const files = await api.all('GET /repos/{owner}/{repo}/pulls/{pull_number}/files', { pull_number: number });
    if (files.length !== pr.changed_files) throw new Error('Incomplete PR file list (GitHub limits this API to 3000 files); split the PR.');
    const paths = policy.changedPaths(files);
    classification = policy.classify(paths, config);
    const rules = await readCodeowners(api, pr.base.sha);
    const owners = policy.ownership(paths, rules);
    const sameHead = (await api.all('GET /repos/{owner}/{repo}/pulls', { state: 'open', base: 'main' })).filter((pull) => pull.head.sha === sha);
    if (sameHead.length !== 1) throw new Error('Multiple open PRs share this head SHA; close duplicates to avoid sharing human gate results.');
    const groups = policy.requiredCi(paths, config);
    const workflowIds = {};
    for (const workflow of new Set(groups.map((group) => group.workflow))) {
      workflowIds[workflow] = (await api.call('GET /repos/{owner}/{repo}/actions/workflows/{workflow_id}', { workflow_id: workflow })).id;
    }
    function checkIdentity(current) {
      if (current.state !== 'open' || current.head.sha !== sha || current.base.ref !== pr.base.ref || current.base.sha !== pr.base.sha) {
        throw new Error('PR head or target branch changed while collecting evidence; rerun Review policy.');
      }
    }
    async function readEvidence() {
      checkIdentity(await getPr());
      const [reviews, comments, reactions, checks, allRuns] = await Promise.all([
        api.all('GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews', { pull_number: number }),
        api.all('GET /repos/{owner}/{repo}/issues/{issue_number}/comments', { issue_number: number }),
        api.all('GET /repos/{owner}/{repo}/issues/{issue_number}/reactions', { issue_number: number }),
        api.all('GET /repos/{owner}/{repo}/commits/{ref}/check-runs', { ref: sha, filter: 'all' }, 'check_runs'),
        api.all('GET /repos/{owner}/{repo}/actions/runs', { head_sha: sha, event: 'pull_request' }, 'workflow_runs'),
      ]);
      const runs = allRuns.filter((run) => Object.values(workflowIds).includes(run.workflow_id));
      const jobs = new Map();
      for (const id of new Set(Object.values(workflowIds))) {
        const run = runs.filter((run) => run.workflow_id === id).sort((a, b) => b.id - a.id)[0];
        if (run?.status === 'completed') jobs.set(run.id, await api.all('GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs', { run_id: run.id, filter: 'latest' }, 'jobs'));
      }
      const humans = await people(api, pr, reviews, owners);
      const cursorChecks = checks.filter((check) => check.name === 'Cursor Bugbot' && check.app?.slug === 'cursor');
      const evidenceResults = {
        'codex-review-gate': policy.codexGate(reviews, reactions, sha, observation.started_at),
        'bugbot-review-gate': policy.bugbotGate(cursorChecks, reviews, sha),
        'human-review-gate': policy.humanGate({ pr, risk: classification.risk, paths: owners, reviews, comments, ...humans }),
        'required-ci-gate': policy.ciGate(groups, runs, jobs, workflowIds, pr),
      };
      checkIdentity(await getPr());
      return { reviews, results: evidenceResults, fingerprint: fingerprint({
        head: sha, base: pr.base.sha, reviews, comments, reactions, checks: cursorChecks, runs,
        jobs: [...jobs], writable: [...humans.writable].sort(),
        ownerMembers: [...humans.ownerMembers].map(([owner, members]) => [owner, [...members].sort()]).sort(),
      }) };
    }
    async function finalEvidence() {
      let candidate = await readEvidence();
      for (let attempt = 0; attempt < 3; attempt++) {
        if (candidate.results['human-review-gate'].state !== 'success') return candidate;
        const rechecked = await readEvidence();
        if (candidate.fingerprint === rechecked.fingerprint) return { ...rechecked, verified: true };
        candidate = rechecked;
      }
      throw new Error('Evidence kept changing during final verification; no merge clearance published. Rerun Review policy.');
    }
    let published;
    const deadline = now() + 30 * 60 * 1000;
    while (true) {
      // Human clearance must stay current even when AI checks are bypassed.
      const evidence = await finalEvidence();
      results = evidence.results;
      const waitingForAI = () => ['codex-review-gate', 'bugbot-review-gate'].some((name) => results[name].state === 'pending');
      const aiWaiting = waitingForAI();
      const expired = now() + 30000 > deadline;
      if (aiWaiting && expired) for (const name of ['codex-review-gate', 'bugbot-review-gate']) {
        if (results[name].state === 'pending') results[name] = { state: 'failure', reason: 'No completed review after 30 minutes; rerun Review policy.' };
      }
      const text = summary(pr, classification, results, evidence.verified ? evidence.fingerprint : undefined);
      if (text !== published) {
        const current = await getPr();
        if (current.head.sha !== sha || current.base.ref !== pr.base.ref || current.state !== 'open') return;
        if (current.base.sha !== pr.base.sha) throw new Error('Target branch advanced; rerun Review policy against its current policy.');
        for (const name of policy.GATES) if (!await publish(name, results[name], text)) return;
        published = text;
      }
      if (!aiWaiting || expired) { await core.summary.addRaw(`<pre>${text.replaceAll('&', '&amp;').replaceAll('<', '&lt;')}</pre>`).write(); return; }
      core.info(`Waiting for AI reviews on PR #${number}, ${sha.slice(0, 7)}.`);
      await sleep(30000);
    }
  } catch (error) {
    const reason = `Review policy could not verify requirements: ${error.message}`;
    for (const name of policy.GATES) if (gateChecks.has(name)) await publish(name, { state: 'failure' }, reason);
    core.setFailed(reason);
  }
}

module.exports = { Api, targets, readCodeowners, people, summary, evaluate };
