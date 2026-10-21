'use strict';

const fs = require('node:fs');
const { createHash } = require('node:crypto');
const policy = require('./policy.cjs');
const AI_POLL_INTERVAL_MS = 60000;
const POLICY_INPUTS = [
  ['', ['CODEOWNERS']],
  ['.github', ['CODEOWNERS', 'review-policy.json', 'workflows', 'actions']],
  ['scripts', ['review-policy']],
  ['docs', ['CODEOWNERS']],
];

function fingerprint(value) {
  const canonical = (item) => Array.isArray(item) ? item.map(canonical)
    : item && typeof item === 'object' ? Object.fromEntries(Object.keys(item).sort().map((key) => [key, canonical(item[key])])) : item;
  return createHash('sha256').update(JSON.stringify(canonical(value))).digest('hex');
}

class RateLimitError extends Error {
  constructor(error, scope) {
    super(`GitHub ${scope} API rate limit`, { cause: error });
    this.headers = error.response?.headers || {};
  }
}

async function withRateLimitRecovery(operation, { core, sleep, now }) {
  let pausedMs = 0;
  for (let attempt = 0; ; attempt++) {
    try { return await operation(pausedMs); }
    catch (error) {
      if (!(error instanceof RateLimitError)) throw error;
      const headers = error.headers;
      const reset = Number(headers['x-ratelimit-reset']) * 1000;
      const retryAfter = Number(headers['retry-after']) * 1000;
      const primaryWait = headers['x-ratelimit-remaining'] === '0' && Number.isFinite(reset) ? reset - now() + 1000 : 0;
      const delay = Math.max(primaryWait, Number.isFinite(retryAfter) ? retryAfter : 0, 60000 * 2 ** attempt);
      if (attempt >= 3 || pausedMs + delay > 65 * 60000) {
        throw new Error(`${error.message}; recovery budget exhausted. Rerun Review policy after the quota resets.`);
      }
      core.info(`${error.message}; waiting until ${new Date(now() + delay).toISOString()}, then re-reading current evidence.`);
      await sleep(delay);
      pausedMs += delay;
    }
  }
}

class Api {
  constructor(github, repo, teamGithub) {
    this.github = github; this.repo = repo; this.teamGithub = teamGithub;
    this.cache = new Map();
    this.trees = new Map();
    this.policyRevisions = new Map();
    this.queue = Promise.resolve();
    this.rateLimit = undefined;
  }
  call(route, args = {}) {
    const request = this.queue.then(() => this.request(route, args));
    this.queue = request.catch(() => {});
    return request;
  }
  async request(route, args) {
    if (this.rateLimit) throw this.rateLimit;
    const teamRequest = route.startsWith('GET /orgs/{org}/teams/{team_slug}');
    const client = teamRequest ? this.teamGithub : this.github;
    if (!client) throw new Error('Configure REVIEW_POLICY_TEAM_TOKEN with organization Members: read for team ownership checks.');
    const key = route.startsWith('GET ') ? fingerprint([route, args]) : undefined;
    const cached = this.cache.get(key);
    let response;
    try {
      response = await client.request(route, { ...(!teamRequest ? this.repo : {}), ...args,
        ...(cached ? { headers: { ...args.headers, 'if-none-match': cached.etag } } : {}), request: { timeout: 20000 } });
    } catch (error) {
      if (error.status === 304 && cached) return structuredClone(cached.data);
      const headers = error.response?.headers || {};
      if ([403, 429].includes(error.status) && (error.status === 429 || headers['x-ratelimit-remaining'] === '0'
          || headers['retry-after'] !== undefined || /rate limit/i.test(error.message))) {
        this.rateLimit = new RateLimitError(error, teamRequest ? 'team membership' : 'repository');
        throw this.rateLimit;
      }
      throw error;
    }
    if (response.status === 304 && cached) return structuredClone(cached.data);
    if (key) {
      if (response.headers?.etag) this.cache.set(key, { etag: response.headers.etag, data: structuredClone(response.data) });
      else this.cache.delete(key);
    }
    return response.data;
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
  async tree(sha) {
    if (!this.trees.has(sha)) {
      const data = await this.call('GET /repos/{owner}/{repo}/git/trees/{tree_sha}', { tree_sha: sha });
      if (data.truncated || !Array.isArray(data.tree)) throw new Error('Incomplete Git tree; cannot verify review policy.');
      this.trees.set(sha, data.tree);
    }
    return this.trees.get(sha);
  }
  async policyRevision(sha) {
    if (!this.policyRevisions.has(sha)) {
      const root = await this.tree(sha);
      const inputs = [];
      for (const [directory, names] of POLICY_INPUTS) {
        const entry = directory && root.find((item) => item.path === directory);
        if (entry && entry.type !== 'tree') throw new Error(`Invalid review-policy directory: ${directory}`);
        const entries = !directory ? root : entry ? await this.tree(entry.sha) : [];
        for (const name of names) {
          const item = entries.find((candidate) => candidate.path === name);
          inputs.push([directory ? `${directory}/${name}` : name, item ? [item.mode, item.type, item.sha] : null]);
        }
      }
      this.policyRevisions.set(sha, fingerprint(inputs));
    }
    return this.policyRevisions.get(sha);
  }
  async samePolicy(before, after) {
    return before === after || await this.policyRevision(before) === await this.policyRevision(after);
  }
}

async function targets({ github, context, core = { info: () => {} },
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)), now = Date.now }) {
  return withRateLimitRecovery(() => findTargets(github, context), { core, sleep, now });
}

async function findTargets(github, context) {
  const api = new Api(github, context.repo);
  const event = context.payload;
  if (context.eventName === 'workflow_run' && event.action !== 'completed'
      && !(event.action === 'in_progress' && event.workflow_run?.run_attempt > 1)) return [];
  if (context.eventName === 'push') {
    if (event.deleted) return [];
    if (!event.before || !event.after) throw new Error('Push is missing commit identities; cannot verify review-policy changes.');
    if (!/^0+$/.test(event.before) && await api.samePolicy(event.before, event.after)) return [];
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
      if (error instanceof RateLimitError) throw error;
      throw new Error(`Cannot verify team ${owner} membership: ${error.status ? `HTTP ${error.status}` : error.message}; check REVIEW_POLICY_TEAM_TOKEN (organization Members: read).`);
    }
  }
  return { writable, ownerMembers };
}

function summary(pr, classification, results, humanEvidenceFingerprint) {
  const lines = [`PR #${pr.number}; head ${pr.head.sha}; base ${pr.base.sha}; risk: ${classification?.risk || 'unknown'}.`];
  if (humanEvidenceFingerprint) lines.push(`Verified human evidence fingerprint: ${humanEvidenceFingerprint}`);
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
  const timing = {};
  try {
    await withRateLimitRecovery((pausedMs) => evaluateOnce({ github, context, core, number, teamGithub, sleep, now, timing, pausedMs }), { core, sleep, now });
  } catch (error) {
    core.setFailed(`Review policy could not verify requirements: ${error.message}`);
  }
}

async function evaluateOnce({ github, context, core, number, teamGithub, sleep, now, timing, pausedMs }) {
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
  const gateStatuses = new Map();
  async function create(name, extra) {
    return api.call('POST /repos/{owner}/{repo}/check-runs', { name, head_sha: sha, external_id: external, details_url: link, ...extra });
  }
  async function publishStatus(name, result, check) {
    const target_url = `${context.serverUrl || 'https://github.com'}/${fullName}/runs/${check.id}`;
    const previous = gateStatuses.get(name);
    if (previous?.state === result.state && previous.target_url === target_url) return;
    const status = await api.call('POST /repos/{owner}/{repo}/statuses/{sha}', {
      sha, context: name, state: result.state, target_url, description: `${name}: ${result.state}`,
    });
    if (status.state !== result.state || status.context !== name || status.target_url !== target_url) {
      throw new Error(`GitHub did not publish the expected ${name} commit status.`);
    }
    gateStatuses.set(name, status);
  }
  async function publish(name, result, text) {
    const current = await getPr();
    if (current.state !== 'open' || current.head.sha !== sha || current.base.ref !== pr.base.ref) return false;
    if (result.state === 'success' && !await api.samePolicy(pr.base.sha, current.base.sha)) return false;
    let active = gateChecks.get(name);
    if (result.state === 'pending' && active.status === 'completed') {
      active = await create(name, { status: 'in_progress' });
      gateChecks.set(name, active);
    }
    const all = await api.all('GET /repos/{owner}/{repo}/commits/{ref}/check-runs', { ref: sha, filter: 'all' }, 'check_runs');
    const matching = all.filter((check) => check.name === name && check.app?.slug === 'github-actions' && check.external_id === external);
    if (matching.some((check) => check.id > active.id)) return false;
    // Invalidate merge clearance before updating diagnostic checks.
    if (result.state !== 'success') await publishStatus(name, result, active);
    for (const check of [active, ...matching.filter((check) => check.id !== active.id)]) {
      const superseded = check.id !== active.id && result.state === 'pending';
      const conclusion = superseded ? 'failure' : result.state === 'pending' ? undefined : result.state;
      const status = conclusion ? 'completed' : 'in_progress';
      const output = { title: `${name}: ${result.state}`, summary: superseded ? `Superseded; awaiting the current evaluation.\n\n${text}` : text };
      if (check.status === status && (check.conclusion || undefined) === conclusion
          && (check.id !== active.id || (check.output?.title === output.title && check.output?.summary === output.summary))) continue;
      const updated = await api.call('PATCH /repos/{owner}/{repo}/check-runs/{check_run_id}', {
        check_run_id: check.id, details_url: link, status,
        ...(conclusion ? { conclusion, completed_at: new Date(now()).toISOString() } : {}),
        output,
      });
      if (updated.status !== status || (updated.conclusion || undefined) !== conclusion) throw new Error(`GitHub did not publish the expected ${name} result.`);
      if (check.id === active.id) gateChecks.set(name, updated);
    }
    if (result.state === 'success' && gateStatuses.get(name)?.state !== 'success') {
      const latest = await getPr();
      if (latest.state !== 'open' || latest.head.sha !== sha || latest.base.ref !== pr.base.ref
          || !await api.samePolicy(pr.base.sha, latest.base.sha)) return false;
      const latestChecks = await api.all('GET /repos/{owner}/{repo}/commits/{ref}/check-runs', { ref: sha, filter: 'all' }, 'check_runs');
      if (latestChecks.some((check) => check.name === name && check.app?.slug === 'github-actions'
          && check.external_id === external && check.id > active.id)) return false;
      await publishStatus(name, result, active);
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
    if (!checkoutSha || !await api.samePolicy(checkoutSha, pr.base.sha)) throw new Error('Target review policy changed since checkout; rerun Review policy.');
    const config = JSON.parse(fs.readFileSync('.github/review-policy.json', 'utf8'));
    const files = await api.all('GET /repos/{owner}/{repo}/pulls/{pull_number}/files', { pull_number: number });
    if (files.length !== pr.changed_files) throw new Error('Incomplete PR file list (GitHub limits this API to 3000 files); split the PR.');
    const paths = policy.changedPaths(files);
    if (paths.includes('.github/workflows/review-policy-events.yml')) {
      throw new Error('PRs cannot change the review-event notifier; restore it or use a controlled migration with native review protection enabled.');
    }
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
    async function checkIdentity(current) {
      if (current.state !== 'open' || current.head.sha !== sha || current.base.ref !== pr.base.ref
          || !await api.samePolicy(pr.base.sha, current.base.sha)) {
        throw new Error('PR head or target review policy changed while collecting evidence; rerun Review policy.');
      }
    }
    async function readHumanEvidence(reviews, comments) {
      if (!reviews) [reviews, comments] = await Promise.all([
        api.all('GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews', { pull_number: number }),
        api.all('GET /repos/{owner}/{repo}/issues/{issue_number}/comments', { issue_number: number }),
      ]);
      const humans = await people(api, pr, reviews, owners);
      const humanReviews = [...policy.latestReviews(reviews)].map(([login, review]) => ({ login, state: review.state }))
        .sort((a, b) => a.login.localeCompare(b.login));
      const humanParticipants = new Set([pr.user.login.toLowerCase(), ...humanReviews.map((review) => review.login)]);
      return {
        result: policy.humanGate({ pr, risk: classification.risk, paths: owners, reviews, comments, ...humans }),
        fingerprint: fingerprint({
          head: sha, base: pr.base.sha, risk: classification.risk, paths: owners,
          author: { login: pr.user.login.toLowerCase(), human: policy.human(pr.user) },
          authorApproved: policy.authorApproved(comments, pr.user.login), reviews: humanReviews,
          writable: [...humans.writable].filter((login) => humanParticipants.has(login)).sort(),
          ownerMembers: [...humans.ownerMembers].map(([owner, members]) => [owner, [...members].sort()]).sort(),
        }),
      };
    }
    const completedJobs = new Map();
    async function readEvidence() {
      await checkIdentity(await getPr());
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
        if (run?.status === 'completed') {
          const key = Number.isInteger(run.run_attempt) ? fingerprint([run.id, run.run_attempt, run.updated_at, run.conclusion]) : undefined;
          let runJobs = key && completedJobs.get(key);
          if (!runJobs) {
            runJobs = await api.all('GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs', { run_id: run.id, filter: 'latest' }, 'jobs');
            if (key && runJobs.every((job) => job.status === 'completed')) completedJobs.set(key, runJobs);
          }
          jobs.set(run.id, runJobs);
        }
      }
      const human = await readHumanEvidence(reviews, comments);
      const cursorChecks = checks.filter((check) => check.name === 'Cursor Bugbot' && check.app?.slug === 'cursor');
      const evidenceResults = {
        'codex-review-gate': policy.codexGate(reviews, reactions, sha, observation.started_at),
        'bugbot-review-gate': policy.bugbotGate(cursorChecks, reviews, sha),
        'human-review-gate': human.result,
        'required-ci-gate': policy.ciGate(groups, runs, jobs, workflowIds, pr),
      };
      await checkIdentity(await getPr());
      return { results: evidenceResults, humanFingerprint: human.fingerprint };
    }
    async function finalEvidence() {
      let candidate = await readEvidence();
      for (let attempt = 0; attempt < 3; attempt++) {
        if (candidate.results['human-review-gate'].state !== 'success') return candidate;
        const human = await readHumanEvidence();
        await checkIdentity(await getPr());
        const rechecked = { results: { ...candidate.results, 'human-review-gate': human.result }, humanFingerprint: human.fingerprint };
        if (candidate.humanFingerprint === rechecked.humanFingerprint) return { ...rechecked, verified: true };
        candidate = rechecked;
      }
      throw new Error('Human evidence kept changing during final verification; no human clearance published. Rerun Review policy.');
    }
    let published;
    const deadline = timing.deadline ??= now() - pausedMs + 30 * 60 * 1000;
    while (true) {
      // Human clearance must stay current even when AI checks are bypassed.
      const evidence = await finalEvidence();
      results = evidence.results;
      const waitingForAI = () => ['codex-review-gate', 'bugbot-review-gate'].some((name) => results[name].state === 'pending');
      const aiWaiting = waitingForAI();
      const expired = now() - pausedMs >= deadline;
      if (aiWaiting && expired) for (const name of ['codex-review-gate', 'bugbot-review-gate']) {
        if (results[name].state === 'pending') results[name] = { state: 'failure', reason: 'No completed review after 30 minutes; rerun Review policy.' };
      }
      const text = summary(pr, classification, results, evidence.verified ? evidence.humanFingerprint : undefined);
      if (text !== published) {
        const current = await getPr();
        if (current.head.sha !== sha || current.base.ref !== pr.base.ref || current.state !== 'open') return;
        await checkIdentity(current);
        for (const name of policy.GATES) if (!await publish(name, results[name], text)) return;
        published = text;
      }
      if (!aiWaiting || expired) { await core.summary.addRaw(`<pre>${text.replaceAll('&', '&amp;').replaceAll('<', '&lt;')}</pre>`).write(); return; }
      core.info(`Waiting for AI reviews on PR #${number}, ${sha.slice(0, 7)}.`);
      await sleep(Math.min(AI_POLL_INTERVAL_MS, Math.max(0, deadline - (now() - pausedMs))));
    }
  } catch (error) {
    if (error instanceof RateLimitError) throw error;
    const reason = `Review policy could not verify requirements: ${error.message}`;
    for (const name of policy.GATES) if (gateChecks.has(name)) {
      try { await publish(name, { state: 'failure' }, reason); }
      catch (publicationError) {
        if (publicationError instanceof RateLimitError) throw publicationError;
        core.info(`Could not publish ${name}: ${publicationError.message}`);
      }
    }
    core.setFailed(reason);
  }
}

module.exports = { Api, targets, readCodeowners, people, summary, evaluate };
