'use strict';
// The forge adapter test matrix (t3, VAL-113..117).
//
// Every test drives the real devflow.cjs CLI against a real git repository whose
// PATH resolves `git-forge` to the fake binary. The fake performs the REAL side
// effects of `pr merge --ff` (base CAS, worktree removal, branch deletion) while
// keeping forge state as JSON and recording every call, so each test can assert
// both repository state and which entity (adapter vs validator) touched the forge.
//
// Path A (legacy, no --forge) is exercised alongside path B (--forge) in the
// parity test: same plan, same worker script, both must reach identical feature
// statuses, and the forge path must additionally carry the prId/issueId runtime
// fields and the mirrored forge events.
const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');

const ROOT = path.join(__dirname, '..', '..');
const CLI = path.join(ROOT, 'scripts', 'devflow.cjs');
const FAKE = path.join(ROOT, 'fixtures', 'devflow', 'fake-git-forge.cjs');

const SPEC = '.specs/forge-matrix';
const FAKE_BIN_DIR = path.join(__dirname, '..', '..', 'fixtures', 'devflow', 'bin');
if (!fs.existsSync(FAKE_BIN_DIR)) fs.mkdirSync(FAKE_BIN_DIR, { recursive: true });
const FAKE_LINK = path.join(FAKE_BIN_DIR, 'git-forge');

let tmp;
let callerCounter = 0;

function forgePathEntry() {
  fs.rmSync(FAKE_LINK, { force: true });
  fs.symlinkSync(FAKE, FAKE_LINK);
  return FAKE_BIN_DIR;
}

/**
 * A repo with a two-feature plan, proposed under `session`. With `forge: true`
 * the mission is proposed with --forge and every command gets the fake forge
 * on PATH plus a private FORGE_FAKE_STATE dir and a DEVFLOW_FORGE_CALLER name.
 */
function makeArm(name, { forge = false } = {}) {
  const repo = path.join(tmp, name);
  fs.mkdirSync(repo);
  const g = (args, must = true) => {
    const r = spawnSync('git', args, { cwd: repo, encoding: 'utf8' });
    if (must) assert.equal(r.status, 0, `git ${args.join(' ')} failed: ${r.stderr}`);
    return r.stdout.trim();
  };
  g(['init', '-b', 'main']);
  g(['config', 'user.email', 'matrix@devflow.local']);
  g(['config', 'user.name', 'forge matrix']);
  fs.writeFileSync(path.join(repo, 'README.md'), '# matrix\n');
  fs.mkdirSync(path.join(repo, SPEC), { recursive: true });
  fs.writeFileSync(path.join(repo, SPEC, 'validation-contract.json'), JSON.stringify({
    contract_version: '1', authority: 'matrix contract',
    assertions: [
      { id: 'VAL-001', title: 'first', behavior: 'the first marker file exists', tool: 'cli', evidence: 'stdout', status: 'pending' },
      { id: 'VAL-002', title: 'second', behavior: 'the second marker file exists', tool: 'cli', evidence: 'stdout', status: 'pending' },
    ],
  }, null, 2));
  const base = { status: 'pending', workerSessionIds: [], currentWorkerSessionId: null, addressesItemId: [] };
  fs.writeFileSync(path.join(repo, SPEC, 'features.json'), JSON.stringify({
    mission: 'matrix', generatedBy: 'test', generatedAt: '2026-09-05',
    features: [
      Object.assign({}, base, {
        id: 't1', description: 'Create the first marker file',
        dependsOn: [], milestone: 'ms1', fulfills: ['VAL-001'], skillName: 'tdd',
        expectedBehavior: ['t1.txt exists'],
      }),
      Object.assign({}, base, {
        id: 't2', description: 'Create the second marker file',
        dependsOn: ['t1'], milestone: 'ms1', fulfills: ['VAL-002'], skillName: 'tdd',
        expectedBehavior: ['t2.txt exists'],
      }),
    ],
  }, null, 2));
  fs.writeFileSync(path.join(repo, '.gitignore'), `${SPEC}/**/missions/\n`);
  g(['add', '-A']);
  g(['commit', '-m', 'plan']);

  // missionDirFor: the passed --mission-dir is the ROOT; propose appends
  // <session>/missions. The gitignore must cover the nested runtime dir.
  const forgeArgs = forge ? ['--forge'] : [];
  const r = run(['propose', '--session', name, '--working-directory', repo,
    '--features', path.join(repo, SPEC, 'features.json'),
    '--contract', path.join(repo, SPEC, 'validation-contract.json'),
    '--mission-dir', path.join(repo, SPEC), ...forgeArgs], armEnv(name));
  assert.equal(r.status, 0, r.stderr);
  const missionDir = path.join(repo, SPEC, name, 'missions');
  const arm = { name, repo, missionDir, forge, g, env: armEnv(name) };
  return arm;
}

function armEnv(name) {
  const bin = forgePathEntry();
  const state = path.join(tmp, `forge-state-${name}`);
  return {
    ...process.env,
    PATH: `${bin}${path.delimiter}${process.env.PATH}`,
    FORGE_FAKE_STATE: state,
    DEVFLOW_FORGE_CALLER: name,
  };
}

function run(args, env) {
  return spawnSync(process.execPath, [CLI, ...args], { encoding: 'utf8', env: env || process.env });
}

function events(missionDir) {
  const p = path.join(missionDir, 'progress_log.jsonl');
  if (!fs.existsSync(p)) return [];
  return fs.readFileSync(p, 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l));
}

function runtime(missionDir) {
  return JSON.parse(fs.readFileSync(path.join(missionDir, 'runtime.json'), 'utf8'));
}

/** The mission's state.json (status/expectedBranch/repoRoot live here). */
function state(missionDir) {
  return JSON.parse(fs.readFileSync(path.join(missionDir, 'state.json'), 'utf8'));
}

function forgeState(arm) {
  return {
    issues: JSON.parse(fs.readFileSync(path.join(arm.env.FORGE_FAKE_STATE, 'issues.json'), 'utf8')),
    prs: JSON.parse(fs.readFileSync(path.join(arm.env.FORGE_FAKE_STATE, 'prs.json'), 'utf8')),
    calls: fs.existsSync(path.join(arm.env.FORGE_FAKE_STATE, 'calllog.jsonl'))
      ? fs.readFileSync(path.join(arm.env.FORGE_FAKE_STATE, 'calllog.jsonl'), 'utf8').split('\n').filter(Boolean).map(JSON.parse)
      : [],
  };
}

function armScenario(arm, obj) {
  fs.writeFileSync(path.join(arm.env.FORGE_FAKE_STATE, 'scenario.json'), JSON.stringify(obj, null, 2) + '\n');
}

/**
 * A stand-in worker: commits `t<N>.txt` on its feature branch and submits a
 * success receipt. Written once per tmp; works for both arms.
 */
function writeWorker() {
  const p = path.join(tmp, 'worker.cjs');
  fs.writeFileSync(p, `
'use strict';
const { spawnSync } = require('node:child_process');
const fs = require('node:fs'), path = require('node:path');
const featureId = process.env.FEATURE_ID;
const wd = process.env.WORKING_DIRECTORY;
const cli = process.env.DEVFLOW_CLI;
fs.writeFileSync(path.join(wd, featureId + '.txt'), 'work for ' + featureId + '\\n');
spawnSync('git', ['add', '-A'], { cwd: wd });
spawnSync('git', ['commit', '-m', 'feat: ' + featureId], { cwd: wd });
const commitId = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: wd, encoding: 'utf8' }).stdout.trim();
const receipt = {
  successState: 'success', commitId, repoPath: wd, validatorsPassed: true,
  handoff: {
    salientSummary: 'wrote ' + featureId + '.txt',
    whatWasImplemented: 'created ' + featureId + '.txt',
    whatWasLeftUndone: '',
    verification: { commandsRun: [{ command: 'true', exitCode: 0 }] },
    tests: { added: [] }, discoveredIssues: [],
  },
};
const res = spawnSync(process.execPath, [cli, 'end-feature', '--mission-dir', process.env.MISSION_DIR,
  '--feature', featureId, '--worker-session', process.env.WORKER_SESSION_ID],
  { input: JSON.stringify(receipt), env: process.env, encoding: 'utf8' });
process.stderr.write(res.stderr || '');
process.exit(res.status === 0 ? 0 : 1);
`);
  fs.chmodSync(p, 0o755);
  return p;
}

/**
 * Drive one feature through the whole gate: claim opens the worker session, the
 * worker commits and submits its receipt, next-feature settles it, a passing
 * review is recorded, and merge-feature runs the forge merge gate — the
 * documented model loop. The merge-gate forge work happens exactly where the
 * adapter owns it, and `onBeforeSettle` is the injection window between the
 * review verdict and the gate.
 */
function driveFeature(arm, featureId, worker, round = 0, onBeforeSettle = null) {
  const claimOut = run(['claim', '--mission-dir', arm.missionDir, '--feature', featureId, '--json'], arm.env);
  assert.equal(claimOut.status, 0, `claim failed: ${claimOut.stderr}`);
  const claim = JSON.parse(claimOut.stdout);
  const sid = claim.workerSessionId;
  // The worker inherits no forge caller name: a forge write from a worker would
  // log caller 'external', so the call log attributes writes precisely.
  const workerEnv = { ...claim.env, DEVFLOW_CLI: CLI };
  delete workerEnv.DEVFLOW_FORGE_CALLER;
  const wr = spawnSync(process.execPath, [worker], { env: workerEnv, encoding: 'utf8' });
  assert.equal(wr.status, 0, `worker failed: ${wr.stderr}`);
  // The review lands BEFORE the settle: settleRound performs the forge merge
  // gate in the same round it settles the receipt, so the injection window a
  // test needs is between record-review and next-feature.
  const reviewPath = path.join(arm.missionDir, 'reviews', `${featureId}-r${round + 1}.json`);
  fs.mkdirSync(path.dirname(reviewPath), { recursive: true });
  fs.writeFileSync(reviewPath, JSON.stringify({ findings: [], summary: 'clean' }, null, 2));
  const rec = run(['record-review', '--mission-dir', arm.missionDir, '--feature', featureId, '--report', reviewPath, '--round', String(round + 1)], arm.env);
  assert.equal(rec.status, 0, `record-review failed: ${rec.stderr}`);
  if (onBeforeSettle) onBeforeSettle();
  const settle = run(['next-feature', '--mission-dir', arm.missionDir], arm.env);
  assert.equal(settle.status, 0, settle.stderr);
  return { sid, claim, settle };
}

before(() => {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'devflow-forge-'));
});

after(() => {
  fs.rmSync(tmp, { recursive: true, force: true });
  if (fs.existsSync(FAKE_LINK)) fs.rmSync(FAKE_LINK, { force: true });
});

// ---------------------------------------------------------------------------
// VAL-113: propose --forge → merge-gate PR creation, prId/issueId, idempotence
// ---------------------------------------------------------------------------

test('VAL-113: propose --forge files issues for active tickets once; a re-propose reuse never re-files', () => {
  const arm = makeArm('val113', { forge: true });
  const s1 = forgeState(arm);
  // Two active tickets at propose -> two issues, session-labelled, Ticket first line.
  assert.equal(s1.issues.issues.length, 2, 'two tickets -> two issues at propose');
  for (const f of ['t1', 't2']) {
    const issue = s1.issues.issues.find((i) => i.body.startsWith(`Ticket: ${f}\n`) || i.body === `Ticket: ${f}`);
    assert.ok(issue, `issue for ${f} carries body first line 'Ticket: ${f}'`);
    assert.ok(issue.labels.includes(`session:val113`), `issue for ${f} carries the session label`);
  }
  // Idempotence: the idempotence surface is the ADAPTER's filing step. The
  // reconcile-forge probe re-runs filing for active tickets; a matched
  // (session label + Ticket first line) open Issue is reused, never re-filed.
  const probe = run(['reconcile-forge', '--mission-dir', arm.missionDir], arm.env);
  assert.equal(probe.status, 0, `reconcile-forge failed: ${probe.stderr}`);
  const s2 = forgeState(arm);
  assert.equal(s2.issues.issues.length, 2, 'forge-side issue count does not grow across retries');
});

test('VAL-114: ci red at the merge gate pauses gate_failed, never blanket bridge failure', () => {
  const arm = makeArm('val114', { forge: true });
  const worker = writeWorker();
  driveFeature(arm, 't1', worker, 0, () => armScenario(arm, { ciFail: true }));
  const ev = events(arm.missionDir);
  assert.ok(ev.some((e) => e.event === 'forge_ci_failed'), 'the ci run was attempted at the gate');
  const paused = ev.filter((e) => e.event === 'feature_paused');
  const ev2 = events(arm.missionDir).filter((e) => e.pauseReason);
  assert.ok(ev2.some((e) => e.pauseReason === 'gate_failed'), "the round's pause reason is gate_failed");
  assert.ok(forgeState(arm).prs.prs[0].merged === null, 'PR is not merged');
});

test('VAL-114: stale-base recreates the PR (forge_pr_superseded) and never pauses', () => {
  const arm = makeArm('val114b', { forge: true });
  const worker = writeWorker();
  driveFeature(arm, 't1', worker, 0, () => armScenario(arm, { staleBase: true }));
  const ev = events(arm.missionDir);
  const sup = ev.filter((e) => e.event === 'forge_pr_superseded');
  assert.equal(sup.length, 1, 'exactly one supersede event');
  assert.ok(sup[0].oldPrId, 'supersede carries the old prId');
  assert.ok(sup[0].newPrId && sup[0].newPrId !== sup[0].oldPrId, 'supersede carries a new prId');
  const paused = ev.filter((e) => e.pauseReason);
  assert.ok(!paused.some((e) => e.pauseReason === 'forge_bridge_failed'), 'stale-base never pauses');
  const after = forgeState(arm);
  assert.equal(after.prs.prs.length, 2, 'a recreated PR exists alongside the superseded one');
  assert.match(after.prs.prs.find((p) => p.id === sup[0].oldPrId).abandoned, /superseded: base moved/, 'the stale PR is abandoned with the supersede reason');
  assert.equal(ev.filter((e) => e.event === 'forge_pr_created').length, 2, 'both PR creations are mirrored');
  const rt = runtime(arm.missionDir);
  assert.equal(rt.features.t1.status, 'completed', 'the recreate path completes without a pause');
  assert.match(after.prs.prs.find((p) => p.id === sup[0].oldPrId).abandoned, /superseded: base moved/, 'the stale PR is abandoned with the supersede reason');
});

test('VAL-114: pr create bridge failure pauses RECOVERABLE forge_bridge_failed and self-clears on re-run', () => {
  const arm = makeArm('val114c', { forge: true });
  const worker = writeWorker();
  driveFeature(arm, 't1', worker, 0, () => armScenario(arm, { prCreateFail: true }));
  const ev = events(arm.missionDir);
  assert.ok(ev.some((e) => e.pauseReason === 'forge_bridge_failed'), 'the gate paused with forge_bridge_failed');
  assert.ok(ev.filter((e) => e.event === 'merge_blocked').some((e) => e.code === 'forge_bridge_failed'), 'merge_blocked carries the code');
  assert.equal(forgeState(arm).prs.prs.length, 0, 'no PR was created');
  assert.ok(fs.existsSync(path.join(arm.missionDir, 'state.json')), 'state exists');
  const rt = runtime(arm.missionDir);
  assert.equal(rt.features.t1.status, 'in_review', 'the feature stays in_review (no work to redo)');
  // Self-clearing: without the injection the re-run succeeds.
  const merged = run(['merge-feature', '--mission-dir', arm.missionDir, '--feature', 't1'], arm.env);
  assert.equal(merged.status, 0, `re-run self-clears: ${merged.stderr}`);
  assert.ok(forgeState(arm).prs.prs[0].merged, 'PR merged on the re-run');
});

test('VAL-114: prose-parse failure pauses forge_bridge_failed, never a duplicate filing', () => {
  const arm = makeArm('val114d', { forge: true });
  const worker = writeWorker();
  const before = forgeState(arm);
  const issueCount = before.issues.issues.length;
  driveFeature(arm, 't1', worker, 0, () => armScenario(arm, { prCreateGarbage: true }));
  const ev = events(arm.missionDir);
  assert.ok(ev.some((e) => e.pauseReason === 'forge_bridge_failed'), 'parse failure pauses forge_bridge_failed');
  const after = forgeState(arm);
  assert.equal(after.issues.issues.length, issueCount, 'no duplicate filing on parse failure');
  assert.ok(after.prs.prs.length >= 1, 'the forge may have created a PR; the adapter refuses to guess its id');
  assert.equal(ev.filter((e) => e.event === 'forge_pr_created').length, 0, 'no forge_pr_created mirror without a parsed id');
});

test('VAL-114: persistent finalize CAS conflict pauses forge_bridge_failed', () => {
  const arm = makeArm('val114e', { forge: true });
  const worker = writeWorker();
  driveFeature(arm, 't1', worker, 0, () => armScenario(arm, { casConflict: true }));
  const ev = events(arm.missionDir);
  assert.ok(ev.some((e) => e.pauseReason === 'forge_bridge_failed'), 'CAS conflict pauses forge_bridge_failed');
  assert.ok(forgeState(arm).prs.prs[0].merged === null, 'PR is not merged');
});

// ---------------------------------------------------------------------------
// VAL-115: proof tickets close via one adapter comment + close
// ---------------------------------------------------------------------------

test('VAL-115: proof ticket closes via a single issue comment + close; the call log attributes every write to the adapter', () => {
  const arm = makeArm('val115', { forge: true });
  // Inject a report-only validation feature so a proof ticket exists.
  const amend = {
    id: 'v-p1', description: 'validate the matrix contract', dependsOn: ['t1'],
    milestone: 'ms1', validatorKind: 'assertions', fulfills: [], proves: [],
    skillName: 'user-validate-flow', expectedBehavior: ['report exists'], reportOnly: true,
    cause: { kind: 'review', itemId: 'F-901' },
  };
  const amendPath = path.join(tmp, 'amend-v-p1.json');
  fs.writeFileSync(amendPath, JSON.stringify(amend, null, 2));
  const r = run(['amend', '--mission-dir', arm.missionDir, '--add-feature', amendPath], arm.env);
  assert.equal(r.status, 0, `amend failed: ${r.stderr}`);
  const s1 = forgeState(arm);
  const vIssue = s1.issues.issues.find((i) => i.body.startsWith('Ticket: v-p1'));
  assert.ok(vIssue, 'the amended validator ticket was filed');
  assert.equal(vIssue.open, true);
  // The adapter closes it with exactly one comment (evidence pointer + status) + close.
  const closed = run(['close-forge-ticket', '--mission-dir', arm.missionDir, '--feature', 'v-p1',
    '--evidence', 'reports/v-p1.json', '--status', 'success'], arm.env);
  assert.equal(closed.status, 0, `close-forge-ticket failed: ${closed.stderr}`);
  const s2 = forgeState(arm);
  const closedIssue = s2.issues.issues.find((i) => i.body.startsWith('Ticket: v-p1'));
  assert.equal(closedIssue.open, false, 'the proof issue is closed');
  assert.equal(closedIssue.comments.length, 1, 'exactly one closing comment');
  assert.match(closedIssue.comments[0], /reports\/v-p1\.json/, 'the comment carries the evidence pointer');
  assert.match(closedIssue.comments[0], /success/, 'the comment carries the overall status');
  // Write attribution: every forge WRITE call is the adapter's (DEVFLOW_FORGE_CALLER),
  // and no validator/worker process wrote. In this matrix the only writer is the
  // adapter; a worker holding no forge caller name would log 'external'.
  // Write attribution: every forge WRITE call must carry the adapter caller name
  // (DEVFLOW_FORGE_CALLER is set only by the adapter's forgeRun). A worker or
  // validator writing directly would log 'external' and fail here.
  const writes = s2.calls.filter((c) => ['new', 'comment', 'close', 'create', 'merge', 'review', 'run', 'abandon'].includes(c.argv[1]));
  assert.ok(writes.length > 0, 'the matrix produced forge writes');
  for (const w of writes) assert.equal(w.caller, 'adapter', `forge write ${JSON.stringify(w.argv)} came from the adapter, not a validator`);
});

// ---------------------------------------------------------------------------
// VAL-116: crash reconcile/resume probe
// ---------------------------------------------------------------------------

/** Drive t1 up to a successful forge merge, then rewind the runtime to simulate a crash before the runtime/progress update. */
function crashAfterMerge(arm, worker) {
  driveFeature(arm, 't1', worker);
  const st = forgeState(arm);
  const pr = st.prs.prs[0];
  assert.ok(pr && pr.merged, 'the PR is merged');
  // Crash states are produced by editing the RUNTIME (never hand-edited in real
  // operation — the probe rebuilds it) and rewinding the log to the merge point.
  return pr;
}

test('VAL-116: merged-not-recorded → probe marks completed, mirrors forge_merged, never re-invokes pr merge', () => {
  const arm = makeArm('val116a', { forge: true });
  const worker = writeWorker();
  const pr = crashAfterMerge(arm, worker);
  // Rewind: the feature is back to in_review (the crash ate the completion write).
  // Rewind via the runtime file the scheduler would have written: put t1 back in_review.
  const runtimePath = path.join(arm.missionDir, 'runtime.json');
  const rt = runtime(arm.missionDir);
  rt.features.t1 = { status: 'in_review', workerSessionIds: rt.features.t1.workerSessionIds, currentWorkerSessionId: null, branch: rt.features.t1.branch, worktree: rt.features.t1.worktree, prId: rt.features.t1.prId, issueId: rt.features.t1.issueId };
  fs.writeFileSync(runtimePath, JSON.stringify(rt, null, 2) + '\n');
  // Rewind the log: drop every event after the forge merge (feature_merged/completed never happened).
  const logPath = path.join(arm.missionDir, 'progress_log.jsonl');
  const ev = fs.readFileSync(logPath, 'utf8').split('\n').filter(Boolean).map(JSON.parse);
  const keep = [];
  for (const e of ev) {
    if (e.event === 'forge_merged') break;
    keep.push(e);
  }
  fs.writeFileSync(logPath, keep.map((e) => JSON.stringify(e)).join('\n') + '\n');

  const callsBefore = forgeState(arm).calls.filter((c) => c.argv[1] === 'merge').length;
  const probe = run(['reconcile-forge', '--mission-dir', arm.missionDir], arm.env);
  assert.equal(probe.status, 0, `probe failed: ${probe.stderr}`);
  const rt2 = runtime(arm.missionDir);
  assert.equal(rt2.features.t1.status, 'completed', 'merged-not-recorded converges to completed');
  const ev2 = events(arm.missionDir);
  assert.ok(ev2.some((e) => e.event === 'forge_merged' && e.prId === pr.id), 'probe mirrors forge_merged with the prId');
  assert.ok(ev2.some((e) => e.event === 'feature_completed' && e.featureId === 't1'), 'feature_completed is recorded');
  const callsAfter = forgeState(arm).calls.filter((c) => c.argv[1] === 'merge');
  assert.equal(callsAfter.length, callsBefore, 'the probe never re-invokes pr merge');
});

test('VAL-116: merged + open issue → probe posts the closing comment and closes; still never re-merges', () => {
  const arm = makeArm('val116b', { forge: true });
  const worker = writeWorker();
  const pr = crashAfterMerge(arm, worker);
  // Simulate the crash ALSO eating the issue close (pr.close linkage): the issue
  // that the merge would have closed (the --closes ticket t1) is forced open.
  const fsIssues = path.join(arm.env.FORGE_FAKE_STATE, 'issues.json');
  const issuesDoc = JSON.parse(fs.readFileSync(fsIssues, 'utf8'));
  const t1Issue = issuesDoc.issues.find((i) => i.body.startsWith('Ticket: t1'));
  t1Issue.open = true;
  fs.writeFileSync(fsIssues, JSON.stringify(issuesDoc, null, 2) + '\n');

  const callsBefore = forgeState(arm).calls.filter((c) => c.argv[1] === 'merge').length;
  const probe = run(['reconcile-forge', '--mission-dir', arm.missionDir], arm.env);
  assert.equal(probe.status, 0, `probe failed: ${probe.stderr}`);
  const s2 = forgeState(arm);
  assert.equal(s2.issues.issues.find((i) => i.body.startsWith('Ticket: t1')).open, false, 'the issue is closed by the probe');
  const t1After = s2.issues.issues.find((i) => i.body.startsWith('Ticket: t1'));
  assert.ok(t1After.comments.length >= 1, 'a closing comment was posted');
  assert.equal(s2.calls.filter((c) => c.argv[1] === 'merge').length, callsBefore, 'pr merge was never re-invoked');
  void pr;
});

test('VAL-116: merged + remaining worktree → probe recycles it, still never re-merges', () => {
  const arm = makeArm('val116c', { forge: true });
  const worker = writeWorker();
  const pr = crashAfterMerge(arm, worker);
  // Re-create the worktree the crash left behind (registered, on the PR's
  // source branch name — the crash window's leftover, branch not yet deleted).
  const rt = runtime(arm.missionDir);
  const st = forgeState(arm);
  const srcBranch = st.prs.prs[0].source;
  const srcHead = st.prs.prs[0].source_head;
  const wt = path.join(tmp, 'leftover-wt');
  arm.g(['branch', srcBranch, srcHead]);
  arm.g(['worktree', 'add', wt, srcBranch]);
  const runtimePath = path.join(arm.missionDir, 'runtime.json');
  rt.features.t1.worktree = wt;
  rt.features.t1.branch = rt.features.t1.branch || srcBranch;
  fs.writeFileSync(runtimePath, JSON.stringify(rt, null, 2) + '\n');

  const callsBefore = forgeState(arm).calls.filter((c) => c.argv[1] === 'merge').length;
  const probe = run(['reconcile-forge', '--mission-dir', arm.missionDir], arm.env);
  assert.equal(probe.status, 0, `probe failed: ${probe.stderr}`);
  assert.ok(!fs.existsSync(wt), 'the leftover worktree was recycled');
  const rt2 = runtime(arm.missionDir);
  assert.equal(rt2.features.t1.worktree, null, 'the worktree record is cleared after a successful recycle');
  assert.equal(forgeState(arm).calls.filter((c) => c.argv[1] === 'merge').length, callsBefore, 'pr merge was never re-invoked');
  void pr;
});

// ---------------------------------------------------------------------------
// VAL-117: dual-path parity + mirror events reconstruct the recreate window
// ---------------------------------------------------------------------------

test('VAL-117: dual-path parity — legacy and --forge missions converge on the same statuses; forge mirrors the three events', () => {
  const legacy = makeArm('parity-legacy');
  const forgeArm = makeArm('parity-forge', { forge: true });
  const worker = writeWorker();
  for (const fid of ['t1', 't2']) {
    driveFeature(legacy, fid, worker);
    driveFeature(forgeArm, fid, worker);
  }
  const statuses = (missionDir) => {
    const st = state(missionDir);
    const doc = JSON.parse(fs.readFileSync(st.featuresSourcePath, 'utf8'));
    const rt = runtime(missionDir);
    return doc.features.map((f) => [f.id, (rt.features[f.id] || {}).status || f.status]);
  };
  assert.deepEqual(statuses(forgeArm.missionDir), statuses(legacy.missionDir),
    'both paths land on identical feature statuses');
  const legacyEvents = events(legacy.missionDir).map((e) => e.event);
  assert.ok(!legacyEvents.some((e) => e.startsWith('forge_')), 'the legacy path never emits forge events');
  assert.ok(!runtime(legacy.missionDir).features.t1.prId, 'legacy features never gain a prId');
  const forgeEvents = events(forgeArm.missionDir);
  for (const kind of ['forge_pr_created', 'forge_merged']) {
    assert.ok(forgeEvents.some((e) => e.event === kind), `the forge path mirrors ${kind}`);
  }
  assert.ok(!forgeEvents.some((e) => e.event === 'forge_pr_superseded'), 'a clean double run never recreates a PR');
  assert.ok(runtime(forgeArm.missionDir).features.t1.prId, 'forge features carry a prId');
  assert.ok(runtime(forgeArm.missionDir).features.t1.issueId, 'forge features carry an issueId');
});

test('VAL-117: mirrored events reconstruct the recreate window (created → superseded → merged chain)', () => {
  const arm = makeArm('parity-window', { forge: true });
  const worker = writeWorker();
  driveFeature(arm, 't1', worker, 0, () => armScenario(arm, { staleBase: true }));
  const ev = events(arm.missionDir).filter((e) => e.event.startsWith('forge_'));
  const created = ev.filter((e) => e.event === 'forge_pr_created');
  const superseded = ev.filter((e) => e.event === 'forge_pr_superseded');
  const mergedEv = ev.filter((e) => e.event === 'forge_merged');
  assert.equal(created.length, 2, 'two creations (original + recreate)');
  assert.equal(superseded.length, 1);
  assert.equal(mergedEv.length, 1);
  // The window: created(prOld) → superseded(prOld→prNew) → merged(prNew)
  assert.equal(superseded[0].oldPrId, created[0].prId, 'the supersede closes the first window');
  assert.equal(created[1].prId, superseded[0].newPrId, 'the recreate opens the second window');
  assert.equal(mergedEv[0].prId, superseded[0].newPrId, 'the merge lands on the recreated PR');
});

// ---------------------------------------------------------------------------
// Recycle ownership (clause 8): salvage before recycle, outcome persisted
// ---------------------------------------------------------------------------

test('recycle ownership: partial-leftover pauses worktree_partial_removal and keeps feature.worktree as the recovery record', () => {
  const arm = makeArm('recycle-partial', { forge: true });
  const worker = writeWorker();
  driveFeature(arm, 't1', worker, 0, () => armScenario(arm, { partialRecycle: true }));
  const rt = runtime(arm.missionDir);
  assert.equal(rt.features.t1.status, 'completed', 'the PR is terminal — the merge stands');
  const ev = events(arm.missionDir);
  const outcome = ev.find((e) => e.event === 'forge_recycle_outcome');
  assert.ok(outcome && /partial-leftover/.test(outcome.outcome || ''), 'the structured outcome is persisted');
  assert.ok(outcome.worktree, 'feature.worktree is KEPT as the recovery record');
  assert.ok(ev.some((e) => e.event === 'worktree_recycle_partial'), 'the partial recycle is on the record');
  // Recovery: the next round recycles the leftover (recoverMission's completed +
  // worktree sweep), so the pointer converges — the RECORD is the durable part.
  run(['next-feature', '--mission-dir', arm.missionDir], arm.env);
  const rt2 = runtime(arm.missionDir);
  assert.equal(rt2.features.t1.status, 'completed', 'the merge stands through recovery');
});

test('recycle ownership: merged + recycle removes the worktree, deletes the branch, persists removed before clearing', () => {
  const arm = makeArm('recycle-ok', { forge: true });
  const worker = writeWorker();
  driveFeature(arm, 't1', worker);
  const ev = events(arm.missionDir);
  const outcome = ev.find((e) => e.event === 'forge_recycle_outcome');
  assert.ok(outcome && /removed/.test(outcome.outcome), 'the structured outcome was persisted BEFORE clearing');
  const rt2 = runtime(arm.missionDir);
  assert.equal(rt2.features.t1.worktree, null);
  assert.equal(rt2.features.t1.branch, null);
  assert.ok(ev.some((e) => e.event === 'forge_merged' && e.prId), 'forge_merged mirrored');
  const st = forgeState(arm);
  assert.ok(st.prs.prs[0].merged, 'the PR is merged');
});
