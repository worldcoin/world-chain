'use strict';

const RISKS = { low: 0, important: 1, critical: 2 };
const CODEX = new Set(['chatgpt-codex-connector[bot]', 'chatgpt-codex-connector']);
const CURSOR = new Set(['cursor[bot]']);
const GATES = ['codex-review-gate', 'bugbot-review-gate', 'human-review-gate', 'required-ci-gate'];
const lower = (value) => String(value || '').toLowerCase();
const human = (user) => user?.type === 'User' && !lower(user.login).endsWith('[bot]');

function glob(pattern) {
  let result = '';
  for (let i = 0; i < pattern.length; i++) {
    const ch = pattern[i];
    if (ch === '*' && pattern[i + 1] === '*') {
      i++;
      if (pattern[i + 1] === '/') { i++; result += '(?:.*/)?'; }
      else result += '.*';
    } else if (ch === '*') result += '[^/]*';
    else if (ch === '?') result += '[^/]';
    else result += ch.replace(/[\\^$+?.()|{}\[\]]/g, '\\$&');
  }
  return result;
}

function matches(path, pattern) {
  return new RegExp(`^${glob(pattern)}$`).test(path);
}

function changedPaths(files) {
  return [...new Set(files.flatMap((file) => [file.filename, file.previous_filename].filter(Boolean)))];
}

function classify(paths, config) {
  if (!paths.length) throw new Error('No changed paths; refusing an empty classification.');
  const details = paths.map((path) => {
    let risk = config.defaultRisk;
    for (const rule of config.riskRules) {
      if (rule.patterns.some((pattern) => matches(path, pattern))) risk = rule.risk;
    }
    if (!(risk in RISKS)) throw new Error(`Unknown risk: ${risk}`);
    return { path, risk };
  });
  return { risk: details.reduce((a, b) => RISKS[a] >= RISKS[b.risk] ? a : b.risk, 'low'), details };
}

function parseCodeowners(text) {
  return text.split(/\r?\n/).flatMap((line, index) => {
    line = line.trim();
    if (!line || line.startsWith('#')) return [];
    const [pattern, ...rest] = line.split(/\s+/);
    const comment = rest.findIndex((part) => part.startsWith('#'));
    const owners = (comment < 0 ? rest : rest.slice(0, comment)).map(lower);
    if (/[\\\[\]!]/.test(pattern) || owners.some((owner) => !/^@[a-z0-9-]+(?:\/[a-z0-9_.-]+)?$/.test(owner))) {
      throw new Error(`Unsupported CODEOWNERS syntax on line ${index + 1}; ownership cannot be verified.`);
    }
    let source = pattern.replace(/^\//, '');
    const directory = source.endsWith('/');
    source = source.replace(/\/$/, '');
    const anchored = pattern.startsWith('/') || source.includes('/');
    const regex = new RegExp(`${anchored ? '^' : '(?:^|/)'}${glob(source)}${directory ? '/' : '(?:/|$)'}`);
    return [{ pattern, owners, regex }];
  });
}

function ownership(paths, rules) {
  const global = [...rules].reverse().find((rule) => rule.pattern === '*')?.owners || [];
  return paths.map((path) => {
    const owners = [...rules].reverse().find((rule) => rule.regex.test(path))?.owners || [];
    return { path, owners, requiredOwners: owners.length ? owners : global };
  });
}

function latestReviews(reviews) {
  const latest = new Map();
  for (const review of [...reviews].sort((a, b) => Date.parse(a.submitted_at) - Date.parse(b.submitted_at) || a.id - b.id)) {
    if (human(review.user) && ['APPROVED', 'CHANGES_REQUESTED', 'DISMISSED'].includes(review.state)) {
      latest.set(lower(review.user.login), review);
    }
  }
  return latest;
}

function authorApproved(comments, author) {
  const commands = comments.filter((comment) => human(comment.user) && lower(comment.user.login) === lower(author)
    && /^\/(?:un)?approve$/.test((comment.body || '').trim()));
  commands.sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at) || a.id - b.id);
  return commands.at(-1)?.body.trim() === '/approve';
}

function humanGate({ pr, risk, paths, reviews, comments, writable, ownerMembers }) {
  const isWritable = (login) => writable.has(lower(login));
  const isOwner = (login, owners) => owners.some((owner) => ownerMembers.get(owner)?.has(lower(login)));
  const author = lower(pr.user.login);
  const fastPath = human(pr.user) && isWritable(author) && paths.every((path) => path.owners.length && isOwner(author, path.owners));
  const latest = latestReviews(reviews);
  const blockers = [...latest].filter(([login, review]) => isWritable(login) && review.state === 'CHANGES_REQUESTED');
  const approved = [...latest].filter(([login, review]) => login !== author && isWritable(login) && review.state === 'APPROVED')
    .map(([login]) => login).filter((login) => paths.some((path) => isOwner(login, path.requiredOwners)));
  const missing = paths.filter((path) => !(fastPath || (human(pr.user) && isWritable(author) && isOwner(author, path.owners)))
    && !approved.some((login) => isOwner(login, path.requiredOwners)));
  const authorApproval = human(pr.user) && isWritable(author) && authorApproved(comments, author);
  const confirmations = new Set([...approved, ...(authorApproval ? [author] : [])]);
  const required = risk === 'critical' ? 2 : risk === 'important' || !fastPath ? 1 : 0;
  const reasons = [
    ...blockers.map(([login]) => `${login} requests changes`),
    ...missing.map((path) => `Owner approval missing: ${path.path}`),
    ...(confirmations.size < required ? [`${confirmations.size}/${required} distinct human confirmations`] : []),
  ];
  return { state: reasons.length ? 'failure' : 'success', fastPath, required, confirmations: [...confirmations], reasons };
}

function codexGate(reviews, reactions, sha, cutoff) {
  const review = reviews.some((item) => CODEX.has(item.user?.login) && item.commit_id === sha && item.submitted_at
    && ['COMMENTED', 'APPROVED', 'CHANGES_REQUESTED'].includes(item.state));
  const reaction = reactions.some((item) => CODEX.has(item.user?.login) && item.content === '+1'
    && Date.parse(item.created_at) >= Date.parse(cutoff));
  return { state: review || reaction ? 'success' : 'pending', reason: review ? 'Review on current SHA' : reaction ? 'Fresh Codex thumbs-up (time heuristic)' : 'Waiting for Codex on current SHA' };
}

function bugbotGate(checks, reviews, sha) {
  const check = checks.filter((item) => item.name === 'Cursor Bugbot' && item.app?.slug === 'cursor' && item.head_sha === sha)
    .sort((a, b) => b.id - a.id)[0];
  if (!check || check.status !== 'completed') return { state: 'pending', reason: 'Waiting for Cursor Bugbot on current SHA' };
  if (check.conclusion === 'success') return { state: 'success', reason: 'Bugbot completed successfully' };
  const attested = reviews.some((review) => CURSOR.has(review.user?.login) && review.commit_id === sha
    && review.state === 'COMMENTED' && Date.parse(review.submitted_at) >= Date.parse(check.started_at)
    && /<!-- BUGBOT_REVIEW -->/.test(review.body || '')
    && /Cursor Bugbot has reviewed your changes[\s\S]*found \d+ potential issue/.test(review.body || ''));
  if (check.conclusion === 'neutral' && attested) return { state: 'success', reason: 'Bugbot completed with findings; resolve every review thread' };
  return { state: 'failure', reason: `Bugbot ${check.conclusion}; no acceptable completed review` };
}

function requiredCi(paths, config) {
  return config.ci.filter((group) => paths.some((path) => group.patterns.some((pattern) => matches(path, pattern))));
}

function ciGate(groups, runs, jobsByRun, workflowIds, pr) {
  const reasons = [];
  let pending = false;
  for (const group of groups) {
    const run = runs.filter((item) => item.workflow_id === workflowIds[group.workflow] && item.head_sha === pr.head.sha
      && item.event === 'pull_request' && (item.pull_requests?.some((pull) => pull.number === pr.number)
        || (!item.pull_requests?.length && item.head_branch === pr.head.ref && item.head_repository?.id === pr.head.repo?.id)))
      .sort((a, b) => b.id - a.id)[0];
    if (!run || run.status !== 'completed') {
      pending = true; reasons.push(`${group.id}: ${run ? 'running' : 'workflow missing'}`); continue;
    }
    if (run.conclusion !== 'success') { reasons.push(`${group.id}: ${run.conclusion}`); continue; }
    const jobs = jobsByRun.get(run.id) || [];
    const missing = group.jobs.filter((name) => !jobs.some((job) => job.name === name && job.status === 'completed' && job.conclusion === 'success'));
    if (missing.length) reasons.push(`${group.id}: required jobs did not succeed: ${missing.join(', ')}`);
  }
  const failed = reasons.some((reason) => !reason.endsWith(': running') && !reason.endsWith(': workflow missing'));
  return { state: failed ? 'failure' : pending ? 'pending' : 'success', reasons };
}

module.exports = { GATES, human, matches, changedPaths, classify, parseCodeowners, ownership, latestReviews, authorApproved, humanGate, codexGate, bugbotGate, requiredCi, ciGate };
