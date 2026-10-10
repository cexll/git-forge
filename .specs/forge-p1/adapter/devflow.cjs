#!/usr/bin/env node
'use strict';
/**
 * devflow.cjs -- deterministic CLI for the devflow local multi-agent development runtime.
 *
 * Subcommands in this batch (ms1-contract):
 *   validate       4 classes of DAG checks + strict schema (VAL-001, t1)
 *   propose        mission initial-state lock + planDigest trust anchor (VAL-009, t0)
 *   receipt-check  hard receipt validation (VAL-002, t2)
 *   log            append an event to progress_log.jsonl (VAL-002, t2)
 *
 * Later batches: reconcile (t7), gate (t6).
 * Contract-layer features (t0/t1/t2) are executed directly through the CLI by a human/host (the bootstrap path).
 *
 * Exit codes: 0 = accepted/passed; 1 = validation rejection (reason on stderr); 2 = input error (missing file, JSON syntax error, usage error; state untouched).
 *
 * Argument parsing: every option is `--name value` or `--name=value`; option values never enter positionals.
 */
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const os = require('node:os');
const { spawnSync } = require('node:child_process');

// ---------------------------------------------------------------------------
// Constants and shared helpers
// ---------------------------------------------------------------------------

// `cancelled` is terminal and preserves history: a dropped feature is never deleted, because the
// plan is also the execution record. It satisfies nobody's dependsOn -- eligibility asks for
// `completed` -- and it does not hold the mission open at closing.
const FEATURE_STATUS = ['pending', 'in_progress', 'in_review', 'completed', 'failed', 'paused', 'cancelled'];
/**
 * Pause reasons a later round can clear by itself: the check that failed is re-run before the next
 * spawn, so fixing the cause is the whole recovery procedure and no human has to unblock anything.
 * Every other reason waits for a person. Named once because two places decide on this list and a
 * third copy of one fact is how they drift.
 */
const RECOVERABLE_PAUSE = ['branch_mismatch', 'dirty_worktree', 'gate_failed', 'forge_bridge_failed'];
const SUCCESS_STATES = ['success', 'partial', 'failure'];
const FAILURE_CLASSES = ['defect', 'blocked', 'boundary-violation'];
const HANDOFF_REQUIRED = [
  'salientSummary',
  'whatWasImplemented',
  'whatWasLeftUndone',
  'verification',
  'tests',
  'discoveredIssues',
];
// Kept in sync with the feature properties in features.schema.json (additionalProperties: false)
const FEATURE_KEYS = [
  'id',
  'description',
  'dependsOn',
  'milestone',
  'touches',
  'fulfills',
  'proves',
  'skillName',
  'expectedBehavior',
  'status',
  'workerSessionIds',
  'currentWorkerSessionId',
  'addressesItemId',
  'reportOnly',
  'validatorKind',
  'verification',
  'skillFile',
  'origin',
  'cause',
  'branch',
  'worktree',
  // Forge adapter runtime (t3): a --forge mission carries the PR/Issue projection
  // ids on the feature. They live in runtime.json, never in the committed plan.
  'prId',
  'issueId',
];

/**
 * Where a feature came from. `plan` is compiled from a ticket; `amend` was appended mid-run and
 * must name the `cause` that produced it. An appended feature with no cause is an unexplained
 * plan change, which is the thing append-only growth exists to prevent.
 */
const FEATURE_ORIGINS = ['plan', 'amend'];
const AMEND_CAUSE_KINDS = ['review', 'gate', 'assertion', 'receipt'];

/** How many features a `misc-*` debt milestone may hold before its validation round stops being feasible. */
const MISC_MILESTONE_CAP = 6;

class InputError extends Error {}
class RejectError extends Error {
  constructor(message, code) {
    super(message);
    this.code = code || 'rejected';
  }
}

function readJsonFile(p) {
  let raw;
  try {
    raw = fs.readFileSync(p, 'utf8');
  } catch (e) {
    throw new InputError(`cannot read ${p}: ${e.message}`);
  }
  try {
    return JSON.parse(raw);
  } catch (e) {
    throw new InputError(`invalid JSON in ${p}: ${e.message}`);
  }
}

function git(args, cwd) {
  const r = spawnSync('git', args, { cwd, encoding: 'utf8' });
  return { ok: r.status === 0, status: r.status, stdout: (r.stdout || '').trim(), stderr: (r.stderr || '').trim() };
}

function realpathOrNull(p) {
  try {
    return fs.realpathSync(p);
  } catch {
    return null;
  }
}

/** realpath the nearest existing ancestor, then re-join the non-existent segments (for comparing paths that do not exist yet). */
function realpathApprox(p) {
  let cur = path.resolve(p);
  const tail = [];
  while (!fs.existsSync(cur)) {
    const parent = path.dirname(cur);
    if (parent === cur) break;
    tail.unshift(path.basename(cur));
    cur = parent;
  }
  const real = fs.realpathSync(cur);
  return tail.length ? path.join(real, ...tail) : real;
}

function logEvent(missionDir, event, payload) {
  const line = JSON.stringify(Object.assign({ ts: new Date().toISOString(), event }, payload || {}));
  fs.appendFileSync(path.join(missionDir, 'progress_log.jsonl'), line + '\n');
}

/** Parsed progress_log events; malformed lines are skipped rather than failing the read. */
function readEvents(missionDir) {
  const p = path.join(missionDir, 'progress_log.jsonl');
  if (!fs.existsSync(p)) return [];
  const out = [];
  for (const line of fs.readFileSync(p, 'utf8').split('\n')) {
    if (!line) continue;
    try {
      out.push(JSON.parse(line));
    } catch {
      /* skip malformed line */
    }
  }
  return out;
}

// ---------------------------------------------------------------------------
// Layout. A session owns one directory in the project:
//
//   <specsRoot>/<session>/            spec.md, tickets/, features.json, validation-contract.json
//   <specsRoot>/<session>/missions/   state.json, progress_log.jsonl, handoff/, logs/
//
// The plan and the run of that plan stay together; only the runtime half is ignored by git.
// ---------------------------------------------------------------------------

/** Default root: the project's own .specs. DROID_MISSIONS_DIR is the deprecated spelling. */
function specsRootFor(repoRoot) {
  return process.env.DEVFLOW_SPECS_ROOT || process.env.DROID_MISSIONS_DIR || path.join(repoRoot, '.specs');
}

/**
 * The repository `from` sits in, or `from` itself when that is not a repository.
 *
 * The loop commands take no --working-directory: the repository is recorded in state.json, which
 * lives in the directory being resolved. Resolving the default root from the caller's cwd instead
 * would work at the top level and fail one directory down, which is where people actually are.
 */
function repoRootFrom(from) {
  const r = git(['rev-parse', '--show-toplevel'], from);
  return r.ok && r.stdout ? r.stdout : from;
}

function missionDirFor(root, session) {
  return path.resolve(path.join(root, session, 'missions'));
}

/**
 * A mission directory inside the repository must be ignored by git.
 *
 * Outside the repository nothing needs checking -- it cannot dirty a worktree. Inside, the ignore
 * is the only thing standing between the runtime and the clean-worktree requirement that propose
 * and every spawn enforce.
 */
function requireIgnoredIfInsideRepo(missionDir, repoRoot) {
  // repoRoot is the git toplevel (realpath); missionDir may not exist yet -> approximate via the
  // nearest existing ancestor's realpath.
  const rel = path.relative(path.resolve(repoRoot), realpathApprox(missionDir));
  const inside = rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel));
  if (!inside) return;
  const probe = path.join(missionDir, 'state.json'); // check-ignore wants a path, not a maybe-dir
  const r = git(['check-ignore', '-q', probe], repoRoot);
  if (r.status !== 0) {
    const relDir = path.relative(path.resolve(repoRoot), path.resolve(missionDir));
    throw new RejectError(
      `mission directory ${missionDir} is inside the repository but not ignored by git; ` +
        `its runtime files would dirty the worktree and pause the mission on the first spawn. ` +
        `Add this line to ${path.join(path.dirname(path.dirname(relDir)), '.gitignore')} and commit it:\n` +
        `  */missions/`,
      'mission_dir_not_ignored'
    );
  }
}

/**
 * A spec artifact inside the repository must be tracked by git.
 *
 * Outside the repository there is nothing to check -- the caller keeps it wherever they like.
 * Inside, "the worktree is clean" and "the file is in version control" are different facts, and
 * only the second one gives the mission an anchor to compare against.
 */
function requireTrackedIfInsideRepo(filePath, repoRoot, label) {
  const abs = path.resolve(filePath);
  const rel = path.relative(path.resolve(repoRoot), realpathApprox(abs));
  if (rel.startsWith('..') || path.isAbsolute(rel)) return;
  const r = git(['ls-files', '--error-unmatch', abs], repoRoot);
  if (r.status !== 0) {
    throw new RejectError(
      `${label} ${abs} is inside the repository but not tracked by git; ` +
        `propose locks its path and digest and run re-reads it every round, so it must be committed first. ` +
        `Check whether .gitignore excludes the spec directory, then git add and commit it.`,
      `${label}_not_tracked`
    );
  }
}

function statePath(missionDir) {
  return path.join(missionDir, 'state.json');
}

function readState(missionDir) {
  try {
    return JSON.parse(fs.readFileSync(statePath(missionDir), 'utf8'));
  } catch (e) {
    throw new InputError(`cannot read state.json in ${missionDir}: ${e.message}`);
  }
}

/**
 * Unified argument parsing: `--name value` or `--name=value`.
 * An option value (the next token, not starting with `--`) never enters positionals.
 */
function parseArgs(args) {
  const positionals = [];
  const options = {};
  // A repeated option collects into an array rather than the last value silently winning:
  // --gate-command is repeatable, and a dropped one is a gate that quietly stopped running.
  const set = (name, value) => {
    if (!Object.prototype.hasOwnProperty.call(options, name)) options[name] = value;
    else if (Array.isArray(options[name])) options[name].push(value);
    else options[name] = [options[name], value];
  };
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a.startsWith('--')) {
      const eq = a.indexOf('=');
      if (eq >= 0) {
        set(a.slice(2, eq), a.slice(eq + 1));
      } else {
        const name = a.slice(2);
        if (i + 1 < args.length && !args[i + 1].startsWith('--')) {
          set(name, args[++i]);
        } else {
          set(name, true);
        }
      }
    } else {
      positionals.push(a);
    }
  }
  return { positionals, options };
}

// ---------------------------------------------------------------------------
// Strict schema checks (semantically identical to references/features.schema.json, dependency-free implementation)
// ---------------------------------------------------------------------------

function schemaErrors(features) {
  const errs = [];
  if (!Array.isArray(features)) {
    return [{ at: 'features', kind: 'not-array' }];
  }
  if (features.length === 0) {
    errs.push({ at: 'features', kind: 'empty-features' }); // schema minItems: 1
  }
  features.forEach((f, i) => {
    const at = f && typeof f.id === 'string' ? f.id : `features[${i}]`;
    if (typeof f !== 'object' || f === null || Array.isArray(f)) {
      errs.push({ at, kind: 'not-object' });
      return;
    }
    for (const k of Object.keys(f)) {
      if (!FEATURE_KEYS.includes(k)) errs.push({ at, kind: 'extra-key', key: k });
    }
    for (const k of ['id', 'description', 'milestone', 'skillName']) {
      if (typeof f[k] !== 'string' || f[k] === '') errs.push({ at, kind: `missing-${k}` });
    }
    if (f.reportOnly !== undefined && typeof f.reportOnly !== 'boolean') {
      errs.push({ at, kind: 'bad-reportOnly' });
    }
    // `validatorKind` routes a report-only feature between reviewer and assertion-validator. The
    // published schema constrains it to the two supported kinds; the runtime validator must agree,
    // or an arbitrary value is dispatched as the wrong role without anything flagging it.
    if (f.validatorKind !== undefined && !VALIDATOR_KIND_SCHEMA_ALLOW.includes(f.validatorKind)) {
      errs.push({ at, kind: 'bad-validatorKind', value: f.validatorKind });
    }
    // `touches` is what admits two features to run concurrently: the paths this one WRITES.
    // Absent or empty means "unknown", which the scheduler treats as intersecting everything --
    // a permitted but deliberate choice, never a silent omission.
    if (f.touches !== undefined) {
      if (!Array.isArray(f.touches)) errs.push({ at, kind: 'missing-touches' });
      else if (!f.touches.every((x) => typeof x === 'string' && x !== '')) errs.push({ at, kind: 'bad-array-element-touches' });
    }
    if (f.origin !== undefined && !FEATURE_ORIGINS.includes(f.origin)) {
      errs.push({ at, kind: 'bad-origin' });
    }
    // An appended feature names exactly one cause. Without it the plan can grow for reasons
    // nobody can reconstruct, and "was this in the original plan?" stops being answerable.
    if (f.origin === 'amend') {
      if (!f.cause || typeof f.cause !== 'object' || Array.isArray(f.cause)) {
        errs.push({ at, kind: 'missing-cause' });
      } else if (!AMEND_CAUSE_KINDS.includes(f.cause.kind)) {
        errs.push({ at, kind: 'bad-cause-kind' });
      }
    } else if (f.cause !== undefined) {
      errs.push({ at, kind: 'cause-without-amend' });
    }
    for (const k of ['branch', 'worktree']) {
      if (f[k] !== undefined && f[k] !== null && typeof f[k] !== 'string') errs.push({ at, kind: `bad-${k}` });
    }
    errs.push(...verificationErrors(f, at));
    errs.push(...skillFileErrors(f, at));
    for (const k of ['dependsOn', 'fulfills', 'expectedBehavior']) {
      if (!Array.isArray(f[k])) {
        errs.push({ at, kind: `missing-${k}` });
      } else if (!f[k].every((x) => typeof x === 'string')) {
        errs.push({ at, kind: `bad-array-element-${k}` });
      }
    }
    for (const k of ['workerSessionIds', 'addressesItemId']) {
      if (f[k] === undefined) continue;
      if (!Array.isArray(f[k])) errs.push({ at, kind: `missing-${k}` });
      else if (!f[k].every((x) => typeof x === 'string')) errs.push({ at, kind: `bad-array-element-${k}` });
    }
    if (Array.isArray(f.addressesItemId) && !f.addressesItemId.every((x) => typeof x === 'string' && /^F-\d+$/.test(x))) {
      errs.push({ at, kind: 'bad-item-id', key: 'addressesItemId' });
    }
    // Area-prefixed ids (VAL-AUTH-001, VAL-CROSS-002) are what real contracts use -- missions
    // prescribes them and migration packs emit them -- so the shape allows optional uppercase
    // area segments while still requiring a VAL- prefix and a numeric tail.
    if (Array.isArray(f.fulfills) && !f.fulfills.every((x) => typeof x === 'string' && ASSERTION_ID_RE.test(x))) {
      errs.push({ at, kind: 'bad-fulfills-pattern' });
    }
    // `proves` is optional (a plan may have no validation features at all), but once present it
    // obeys the same shape as `fulfills`. The two are different claims about the same assertion:
    // `fulfills` says "my work makes this true", `proves` says "my work demonstrates it is true".
    if (f.proves !== undefined) {
      if (!Array.isArray(f.proves)) {
        // Not `missing-proves`: the field is present and wrong-typed, and `missing-` is what the
        // required-field checks above emit. Sending someone to look for an absent field they wrote
        // is the kind of error message that costs more time than no message.
        errs.push({ at, kind: 'bad-proves-type' });
      } else if (!f.proves.every((x) => typeof x === 'string')) {
        errs.push({ at, kind: 'bad-array-element-proves' });
      } else if (!f.proves.every((x) => ASSERTION_ID_RE.test(x))) {
        errs.push({ at, kind: 'bad-proves-pattern' });
      }
    }
    if (typeof f.id === 'string' && !/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(f.id)) {
      errs.push({ at, kind: 'bad-id' });
    }
    if (!Array.isArray(f.expectedBehavior) || f.expectedBehavior.length === 0) {
      errs.push({ at, kind: 'empty-expectedBehavior' });
    }
    // Runtime fields are optional in a plan file: they live in the mission's runtime, and a plan
    // that carries them is either pre-split or freshly compiled with initialised values. They are
    // still checked when present -- an illegal status must not pass merely by being optional.
    if (f.status !== undefined) {
      if (typeof f.status !== 'string' || f.status === '') errs.push({ at, kind: 'missing-status' });
      else if (!FEATURE_STATUS.includes(f.status)) errs.push({ at, kind: 'bad-status', value: f.status });
    }
    if (f.currentWorkerSessionId !== undefined &&
        f.currentWorkerSessionId !== null && typeof f.currentWorkerSessionId !== 'string') {
      errs.push({ at, kind: 'bad-currentWorkerSessionId' });
    }
  });
  return errs;
}

// ---------------------------------------------------------------------------
// DAG checks: unknown id / self-loop / cycle (Kahn) / dangling fulfills
// ---------------------------------------------------------------------------

function dagErrors(features, contractAssertions) {
  const errs = [];
  const ids = new Set(features.map((f) => f.id));
  const seen = new Set();
  for (const f of features) {
    if (seen.has(f.id)) errs.push({ at: f.id, kind: 'duplicate-id' });
    seen.add(f.id);
    for (const d of f.dependsOn || []) {
      if (d === f.id) errs.push({ at: f.id, kind: 'self-loop', dep: d });
      else if (!ids.has(d)) errs.push({ at: f.id, kind: 'unknown-id', dep: d });
    }
    for (const v of f.fulfills || []) {
      if (contractAssertions && !contractAssertions.has(v)) {
        errs.push({ at: f.id, kind: 'dangling-fulfills', val: v });
      }
    }
    const built = new Set(Array.isArray(f.fulfills) ? f.fulfills : []);
    for (const v of Array.isArray(f.proves) ? f.proves : []) {
      if (contractAssertions && !contractAssertions.has(v)) {
        errs.push({ at: f.id, kind: 'dangling-proves', val: v });
      }
      // "A validator that repairs the thing it is testing has destroyed its own evidence"
      // (validation-protocol.md §1). A feature naming one assertion in both fulfills and proves
      // is that sentence encoded: it certifies its own work. Proving what a *sibling* built is
      // fine and stays allowed -- the rule is about self-certification, not about committing.
      if (built.has(v)) {
        errs.push({ at: f.id, kind: 'self-proving-feature', val: v });
      }
    }
  }
  // Kahn topological sort; leftover nodes = a cycle
  const indeg = new Map();
  const adj = new Map();
  for (const f of features) {
    indeg.set(f.id, 0);
    adj.set(f.id, []);
  }
  for (const f of features) {
    for (const d of f.dependsOn || []) {
      if (ids.has(d) && d !== f.id) {
        adj.get(d).push(f.id);
        indeg.set(f.id, indeg.get(f.id) + 1);
      }
    }
  }
  const q = features.filter((f) => indeg.get(f.id) === 0).map((f) => f.id);
  const order = [];
  while (q.length) {
    const n = q.shift();
    order.push(n);
    for (const m of adj.get(n)) {
      indeg.set(m, indeg.get(m) - 1);
      if (indeg.get(m) === 0) q.push(m);
    }
  }
  const cyc = features.filter((f) => !order.includes(f.id)).map((f) => f.id).sort();
  if (cyc.length) errs.push({ kind: 'cycle', ids: cyc });
  return errs;
}

// ---------------------------------------------------------------------------
// planDigest: SHA-256 over the normalized planning fields only (FR-011)
// Runtime fields (status/workerSessionIds/currentWorkerSessionId) are excluded from the hash.
// ---------------------------------------------------------------------------

const PLANNING_FIELDS = [
  'description',
  'id',
  'dependsOn',
  'milestone',
  'touches',
  'fulfills',
  'proves',
  'skillName',
  'expectedBehavior',
  'addressesItemId',
  'reportOnly',
  'validatorKind',
  'verification',
  'skillFile',
  'origin',
  'cause',
];
const META_FIELDS = ['mission', 'generatedBy', 'generatedAt'];

/**
 * @param injectedIds ids the progress log attests were injected at runtime (t1). They are
 * runtime state, not plan, so they are excluded -- but the attestation is what makes the
 * exclusion safe: a feature merely NAMED like a validator is still covered by the digest.
 */
function planDigest(features, meta, injectedIds) {
  const injected = injectedIds instanceof Set ? injectedIds : new Set(injectedIds || []);
  const proj = {};
  for (const k of META_FIELDS) {
    if (meta[k] !== undefined) proj[k] = meta[k];
  }
  proj.features = features.filter((f) => !injected.has(f.id)).map((f) => {
    const o = {};
    for (const k of PLANNING_FIELDS) {
      if (f[k] !== undefined) o[k] = f[k];
    }
    return o;
  });
  return crypto.createHash('sha256').update(JSON.stringify(proj)).digest('hex');
}

// ---------------------------------------------------------------------------
// skill binding (t5): bind a feature to the artifact it will be executed with.
//
// `skillName` routes to a HOST skill that lives in the agent harness, not in the repo being
// built, so file resolution cannot be unconditional -- it would reject every plan to-spec
// produces. A feature binds a real artifact with `skillFile`, and `--skills-root` opts a whole
// mission into strict resolution where every skillName must resolve under that root.
// ---------------------------------------------------------------------------

const DIGEST_RE = /^sha256:[0-9a-f]{64}$/;
const ASSERTION_ID_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;

/** `--repair-budget <n>`: how many runs a milestone may spend trying to converge (t6). */
function parseRepairBudget(raw) {
  if (raw === undefined) return 2;
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 0) throw new InputError(`--repair-budget must be a non-negative integer, got: ${raw}`);
  return n;
}

/** `--gate-command name=shell command` (repeatable): the milestone hard gate (t3). */
function parseGateCommands(raw) {
  const list = raw === undefined ? [] : Array.isArray(raw) ? raw : [raw];
  return list.map((entry) => {
    const i = String(entry).indexOf('=');
    if (i <= 0) throw new InputError(`--gate-command must be <id>=<command>, got: ${entry}`);
    const id = entry.slice(0, i).trim();
    const command = entry.slice(i + 1).trim();
    if (!id || !command) throw new InputError(`--gate-command must be <id>=<command>, got: ${entry}`);
    return { id, command, expectedExit: 'zero' };
  });
}

/** `--review-checklist <file.json>`: [{id, text}] the scrutiny report must address. */
function parseReviewChecklist(file) {
  if (!file) return [];
  const doc = readJsonFile(file);
  if (!Array.isArray(doc)) throw new InputError(`review checklist must be a JSON array of {id, text}: ${file}`);
  return doc.map((item, i) => {
    if (typeof item !== 'object' || item === null || typeof item.id !== 'string' || item.id === '') {
      throw new InputError(`review checklist item ${i} needs a non-empty id: ${file}`);
    }
    return { id: item.id, text: typeof item.text === 'string' ? item.text : '' };
  });
}

function skillFileErrors(f, at) {
  const sf = f.skillFile;
  if (sf === undefined) return [];
  if (typeof sf !== 'object' || sf === null || Array.isArray(sf)) return [{ at, kind: 'bad-skillFile' }];
  const errs = [];
  if (typeof sf.artifactRef !== 'string' || sf.artifactRef === '') errs.push({ at, kind: 'missing-skillFile-artifactRef' });
  if (sf.digest !== undefined && (typeof sf.digest !== 'string' || !DIGEST_RE.test(sf.digest))) {
    errs.push({ at, kind: 'bad-skillFile-digest' });
  }
  return errs;
}

function fileDigest(absPath) {
  return 'sha256:' + crypto.createHash('sha256').update(fs.readFileSync(absPath)).digest('hex');
}

/** A skill file must at least declare who it is, the same bar missions' rF1 check applied. */
function skillFrontmatterOk(absPath) {
  const txt = fs.readFileSync(absPath, 'utf8');
  const m = txt.match(/^---\n([\s\S]*?)\n---/);
  if (!m) return false;
  return /^name:\s*\S/m.test(m[1]) && /^description:\s*\S/m.test(m[1]);
}

/**
 * Resolve and verify each feature's skill artifact against repoRoot.
 * Returns { bindings } or throws RejectError with a reason distinguishing "you named an
 * artifact that is not there" from "the artifact changed under an approved plan".
 */
function resolveSkillBindings(features, repoRoot, skillsRoot, artifactRoot) {
  // A plan's artifacts do not have to live in the repository it changes: a migration's sealed
  // pack is outside the target repo by design. The root is declared, so containment and digest
  // verification apply to it rather than being dropped.
  const root = artifactRoot || repoRoot;
  const bindings = {};
  for (const f of features) {
    let ref = f.skillFile && f.skillFile.artifactRef;
    let declaredDigest = f.skillFile && f.skillFile.digest;
    if (!ref && skillsRoot) {
      // repoRoot is a realpath; canonicalize the root too or a symlinked tmpdir (/var vs
      // /private/var on macOS) makes an in-repo path look like an escape.
      const rootAbs = realpathOrNull(skillsRoot) || path.resolve(skillsRoot);
      ref = path.join(path.relative(root, rootAbs), f.skillName, 'SKILL.md');
    }
    if (!ref) continue;

    const abs = path.resolve(root, ref);
    const rel = path.relative(root, realpathApprox(abs));
    if (rel.startsWith('..') || path.isAbsolute(rel)) {
      throw new RejectError(`skill artifact escapes the repository @ ${f.id}: ${ref}`, 'skill_file_outside_repo');
    }
    if (!fs.existsSync(abs) || !fs.statSync(abs).isFile()) {
      const reason = f.skillFile ? 'skill_file_missing' : 'skill_unresolved';
      throw new RejectError(`skill artifact not found @ ${f.id}: ${ref} (skillName=${f.skillName})`, reason);
    }
    // Frontmatter is required only where devflow resolves a skills library by name (the rF1
    // gate): there the file must identify itself. An explicitly bound artifact is verified for
    // identity, not shape -- what it must be is its owner's business, and a sealed migration
    // skill is a plain document.
    if (!f.skillFile && skillsRoot && !skillFrontmatterOk(abs)) {
      throw new RejectError(`skill artifact lacks name/description frontmatter @ ${f.id}: ${ref}`, 'skill_file_invalid');
    }
    const actual = fileDigest(abs);
    if (declaredDigest && declaredDigest !== actual) {
      throw new RejectError(
        `skill artifact digest does not match the plan @ ${f.id}: ${ref} (declared=${declaredDigest}, actual=${actual})`,
        'skill_digest_mismatch'
      );
    }
    bindings[f.id] = { artifactRef: rel, digest: actual };
  }
  return bindings;
}

/** Re-verify recorded bindings before scheduling: an approved plan must not execute against edited artifacts. */
function verifySkillBindings(state, repoRoot) {
  const root = state.artifactRoot || repoRoot;
  for (const [featureId, b] of Object.entries(state.skillBindings || {})) {
    const abs = path.resolve(root, b.artifactRef);
    if (!fs.existsSync(abs)) {
      throw new RejectError(`skill artifact missing since propose @ ${featureId}: ${b.artifactRef}`, 'skill_file_missing');
    }
    if (fileDigest(abs) !== b.digest) {
      throw new RejectError(
        `skill artifact changed since propose @ ${featureId}: ${b.artifactRef}; re-propose under a new session to adopt it`,
        'skill_digest_changed'
      );
    }
  }
}

// ---------------------------------------------------------------------------
// declarative verification (t2): the scheduler re-runs what the plan declared.
// `validatorsPassed` is the worker's claim about its own work; these steps are the scheduler's
// own observation, so a contradicting exit code beats the claim.
// ---------------------------------------------------------------------------

const EXPECTED_EXITS = ['zero', 'nonzero'];

function verificationErrors(f, at) {
  const v = f.verification;
  if (v === undefined) return [];
  if (typeof v !== 'object' || v === null || Array.isArray(v)) return [{ at, kind: 'bad-verification' }];
  if (v.mode !== 'steps') return [{ at, kind: 'bad-verification-mode' }];
  if (v.when !== undefined && v.when !== 'pre-spawn' && v.when !== 'post-receipt') {
    return [{ at, kind: 'bad-verification-when' }];
  }
  if (!Array.isArray(v.steps) || v.steps.length === 0) return [{ at, kind: 'empty-verification-steps' }];
  const errs = [];
  v.steps.forEach((st, i) => {
    const where = `${at}.verification.steps[${i}]`;
    if (typeof st !== 'object' || st === null || Array.isArray(st)) {
      errs.push({ at: where, kind: 'bad-verification-step' });
      return;
    }
    if (typeof st.id !== 'string' || st.id === '') errs.push({ at: where, kind: 'missing-step-id' });
    if (typeof st.command !== 'string' || st.command === '') errs.push({ at: where, kind: 'missing-step-command' });
    if (!EXPECTED_EXITS.includes(st.expectedExit)) errs.push({ at: where, kind: 'bad-expectedExit', val: String(st.expectedExit) });
  });
  return errs;
}

/**
 * Run a feature's declared verification steps in the working directory; stops at the first
 * unsatisfied step. `expectedExit: nonzero` exists for red-state checks -- a step that must
 * currently fail -- so the enum does not collapse to a boolean "must pass".
 */
function runVerificationSteps(feature, workingDirectory, timeoutMs, phase) {
  const v = feature.verification;
  if (!v || v.mode !== 'steps') return { ok: true, results: [] };
  if ((v.when || 'post-receipt') !== (phase || 'post-receipt')) return { ok: true, results: [] };
  const results = [];
  for (const step of v.steps) {
    const r = spawnSync(step.command, {
      shell: true,
      cwd: workingDirectory,
      encoding: 'utf8',
      timeout: timeoutMs,
      stdio: ['ignore', 'pipe', 'pipe'],
      killSignal: 'SIGKILL',
    });
    const timedOut = !!(r.error && r.error.code === 'ETIMEDOUT');
    const exitCode = typeof r.status === 'number' ? r.status : null;
    const satisfied = timedOut ? false : step.expectedExit === 'zero' ? exitCode === 0 : exitCode !== 0 && exitCode !== null;
    results.push({ id: step.id, command: step.command, exitCode, timedOut, expectedExit: step.expectedExit, satisfied });
    if (!satisfied) return { ok: false, results };
  }
  return { ok: true, results };
}

// ---------------------------------------------------------------------------
// milestone model (t0): derived from features + progress_log, never stored.
// A second persisted status beside features[].status would be a competing source of truth.
// ---------------------------------------------------------------------------

// Validator features are injected by the validation gate and are not part of the
// implementation work a milestone consists of, so they never gate its completion.
const VALIDATOR_ID_PREFIXES = ['scrutiny-', 'assertions-'];

function isValidatorFeature(f) {
  return VALIDATOR_ID_PREFIXES.some((p) => typeof f.id === 'string' && f.id.startsWith(p));
}

/**
 * Per-milestone view: which features it holds, whether its implementation work is complete,
 * and whether it has been sealed. Completion needs at least one completed feature -- a
 * milestone whose features were all cancelled was never built and must not read as done.
 * Milestones never gate each other; eligibility remains per-feature dependsOn.
 */
function milestoneStates(features, events) {
  const sealed = new Set();
  const completedLogged = new Set();
  for (const e of events || []) {
    if (e.event === 'milestone_sealed' && e.milestone) sealed.add(e.milestone);
    if (e.event === 'milestone_completed' && e.milestone) completedLogged.add(e.milestone);
  }
  const order = [];
  const byMilestone = new Map();
  for (const f of features) {
    const m = f.milestone;
    if (!byMilestone.has(m)) {
      byMilestone.set(m, []);
      order.push(m);
    }
    byMilestone.get(m).push(f);
  }
  return order.map((m) => {
    const all = byMilestone.get(m);
    const impl = all.filter((f) => !isValidatorFeature(f));
    const done = impl.filter((f) => f.status === 'completed');
    const settled = impl.filter((f) => f.status === 'completed' || f.status === 'cancelled');
    return {
      milestone: m,
      features: all.map((f) => f.id),
      implementationFeatures: impl.map((f) => f.id),
      validatorFeatures: all.filter(isValidatorFeature).map((f) => f.id),
      completed: done.length,
      cancelled: impl.filter((f) => f.status === 'cancelled').length,
      open: impl.filter((f) => f.status !== 'completed' && f.status !== 'cancelled').map((f) => f.id),
      complete: impl.length > 0 && settled.length === impl.length && done.length > 0,
      completionLogged: completedLogged.has(m),
      sealed: sealed.has(m),
    };
  });
}

const VALIDATOR_KINDS = [
  // `skill` must name one that exists: `scrutiny-validator` never did, so every scrutiny feature
  // routed a worker at nothing. `description` is the feature's own words in the brief, so it says
  // what this role does rather than what the scheduler already did -- "run the programmatic
  // validators" sat three lines above "do not re-run the programmatic gate", in one prompt.
  { kind: 'scrutiny', prefix: 'scrutiny-', skill: 'code-review',
    description: 'review this milestone\'s completed work as a whole, with its programmatic gate already green',
    done: ['every finding names a file, a line, and what goes wrong there'] },
  { kind: 'assertions', prefix: 'assertions-', skill: 'user-validate-flow',
    description: 'prove this milestone\'s assertions through their real user surface',
    done: ['every assigned assertion carries a verdict backed by evidence'] },
];
const VALIDATOR_KINDS_NAMES = VALIDATOR_KINDS.map((k) => k.kind);
// `acceptance` is a whole-contract gate the COMPILER writes from a `Type: ACCEPTANCE` ticket. It is
// deliberately NOT in VALIDATOR_KINDS: injectMilestoneValidators iterates that catalog, and a feature
// that is always part of the plan must never be injected (nor logged as a skipped validator) at every
// milestone. It is still a legal validatorKind, so the schema gate allows it explicitly.
const VALIDATOR_KIND_SCHEMA_ALLOW = [...VALIDATOR_KINDS_NAMES, 'acceptance'];

/** Ids the log attests were injected; the authority for excluding them from planDigest. */
/** Per-assertion content hashes, so "the contract only gained assertions" is checkable. */
function contractAssertionHashes(contractPath, parsed) {
  const out = {};
  try {
    const doc = parsed || readJsonFile(contractPath);
    for (const a of (doc && doc.assertions) || []) {
      if (a && a.id) out[a.id] = crypto.createHash('sha256').update(JSON.stringify(a)).digest('hex');
    }
  } catch { /* propose already proved it readable */ }
  return out;
}

/** Feature ids the log attests were appended by `amend`. */
function amendedFeatureIds(events) {
  const ids = new Set();
  for (const e of events || []) {
    if (e.event === 'amended') for (const id of e.features || []) ids.add(id);
  }
  return ids;
}

/** The propose-time baseline: what the plan and the contract looked like when they were approved. */
function proposedBaseline(events) {
  for (const e of events || []) {
    if (e.event === 'mission_proposed') {
      return {
        featureIds: Array.isArray(e.plannedFeatureIds) ? new Set(e.plannedFeatureIds) : null,
        assertionHashes: e.assertionHashes || null,
      };
    }
  }
  return { featureIds: null, assertionHashes: null };
}

function injectedFeatureIds(events) {
  const ids = new Set();
  for (const e of events || []) {
    if (e.event === 'milestone_validation_triggered') for (const id of e.featureIds || []) ids.add(id);
  }
  return ids;
}

function validatorFeature(kind, milestone, gateCommands) {
  const gate = kind.kind === 'scrutiny' && gateCommands && gateCommands.length
    ? { verification: { mode: 'steps', when: 'pre-spawn', steps: gateCommands } }
    : {};
  return {
    ...gate,
    id: `${kind.prefix}${milestone}`,
    description: `${kind.description} (${milestone})`,
    dependsOn: [],
    milestone,
    // What role this is, said outright. `featureKind` used to infer it from `proves` -- and the only
    // thing that ever creates these features leaves `proves` empty on purpose, because a validator
    // that claimed proof would let `--validators` alone satisfy the proof-coverage gate and the
    // plan's own obligation would stop mattering. So the assertion validator was read as a plain
    // review: it got the reviewer's brief, with no assertion list, no report path and no evidence
    // directory, and the entire validator procedure was unreachable. Two consumers were asking two
    // different questions of one field; the role now has its own.
    validatorKind: kind.kind,
    fulfills: [], // validators claim no assertions, so the 1:1 coverage gate is untouched
    proves: [], // and none here either: the obligation is DERIVED from the milestone, never claimed
    skillName: kind.skill,
    expectedBehavior: kind.done,
    status: 'pending',
    workerSessionIds: [],
    currentWorkerSessionId: null,
    addressesItemId: [],
    reportOnly: true, // a validator proves assertions and commits nothing
  };
}

/**
 * Inject a completed milestone validators, once. Injected features go to the top so they run
 * before whatever is queued behind them, matching missions' ordering semantics.
 */
function injectMilestoneValidators(missionDir, featuresDoc, state) {
  const events = readEvents(missionDir);
  const already = new Set();
  for (const e of events) {
    if (e.event === 'milestone_validation_triggered' && e.milestone) already.add(e.milestone);
  }
  const enabled = state.validatorKinds || [];
  if (!enabled.length && !(state.validatorsRequested === true)) return false;
  let changed = false;
  for (const st of milestoneStates(featuresDoc.features, events)) {
    if (!st.complete || already.has(st.milestone)) continue;
    const created = [];
    for (const kind of VALIDATOR_KINDS) {
      if (!enabled.includes(kind.kind)) {
        // A skipped validator is logged, never silently omitted.
        logEvent(missionDir, 'validation_skipped', { milestone: st.milestone, kind: kind.kind });
        continue;
      }
      created.push(validatorFeature(kind, st.milestone, state.gateCommands));
    }
    logEvent(missionDir, 'milestone_validation_triggered', { milestone: st.milestone, featureIds: created.map((f) => f.id) });
    if (created.length) {
      featuresDoc.features.unshift(...created);
      changed = true;
      // Injected validators are tickets too: file their Issue projections now
      // (clause 2: injected validators at injection; idempotent).
      if (state.forge === true) {
        forgeFileActiveTickets(missionDir, state, featuresDoc);
      }
    }
  }
  return changed;
}

/** Log milestone_completed once for each milestone whose implementation work just finished. */
function recordMilestoneCompletions(missionDir, features) {
  const states = milestoneStates(features, readEvents(missionDir));
  for (const st of states) {
    if (st.complete && !st.completionLogged) {
      logEvent(missionDir, 'milestone_completed', { milestone: st.milestone, features: st.implementationFeatures });
    }
  }
}

function cmdMilestones(args) {
  const { options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  if (!missionDir) throw new InputError('usage: devflow.cjs milestones --mission-dir <dir>');
  if (!fs.existsSync(statePath(missionDir))) {
    throw new InputError(`no mission at ${missionDir} (no state.json)`);
  }
  const doc = readMissionFeatures(missionDir, readState(missionDir));
  process.stdout.write(JSON.stringify(milestoneStates(doc.features || [], readEvents(missionDir)), null, 2) + '\n');
}

// ---------------------------------------------------------------------------
// validate subcommand (VAL-001 / t1)
// ---------------------------------------------------------------------------

function validateAll(featuresDoc, assertions) {
  if (typeof featuresDoc !== 'object' || featuresDoc === null || Array.isArray(featuresDoc)) {
    throw new InputError('features document must be a JSON object with a "features" array');
  }
  if (!Array.isArray(featuresDoc.features)) {
    throw new InputError('features document missing "features" array');
  }
  const errs = [
    ...schemaErrors(featuresDoc.features),
    ...dagErrors(featuresDoc.features, assertions),
  ];
  // Document-level schema: mission is required and non-empty (schema required)
  if (typeof featuresDoc.mission !== 'string' || featuresDoc.mission === '') {
    errs.unshift({ at: '<doc>', kind: 'missing-mission' });
  }
  return errs;
}

/** 1:1 coverage: every contract assertion claimed by exactly one feature. */
function coverageErrors(features, assertions) {
  const claims = new Map();
  for (const f of features || []) {
    for (const v of f.fulfills || []) claims.set(v, (claims.get(v) || 0) + 1);
  }
  return [...assertions]
    .filter((a) => claims.get(a) !== 1)
    .map((a) => ({ at: a, kind: claims.has(a) ? 'assertion-claimed-more-than-once' : 'assertion-unclaimed' }));
}

/**
 * Proof coverage: every contract assertion is named by at least one feature's `proves`.
 *
 * N:1, not 1:1 -- two validators looking at the same assertion through different surfaces is a
 * stronger plan, not an invalid one. Build coverage (`coverageErrors`) stays 1:1 because two
 * features claiming to build the same thing is an ownership defect.
 */
function proofCoverageErrors(features, assertions) {
  const proven = new Set();
  for (const f of features || []) {
    for (const v of Array.isArray(f.proves) ? f.proves : []) proven.add(v);
  }
  return [...assertions].filter((a) => !proven.has(a)).map((a) => ({ at: a, kind: 'assertion-unproven' }));
}

/**
 * Whether the proof-coverage gate applies to this plan.
 *
 * A plan that declares no `proves` at all is not gated -- it has no proof path, the same
 * precedent the end-of-mission ledger gate follows for missions that never requested validators.
 * A plan that proves *some* assertions is gated on all of them: partial proof is the state that
 * reads as validated without being it. `--require-proof-coverage` forces the gate on a plan whose
 * validation features were forgotten entirely.
 */
function proofCoverageApplies(features, forced) {
  if (forced) return true;
  return (features || []).some((f) => Array.isArray(f.proves) && f.proves.length > 0);
}

/**
 * The acceptance gate (Contract 3): the final milestone must close the whole contract.
 *
 * A contract that ships no `Type: ACCEPTANCE` feature, or one that does not prove every assertion,
 * reads as validated when it is not -- the mission "fails open" on the last step. This gate turns
 * that into a refusal. It is enforced at `compile`/`validate` only when `--require-acceptance` is
 * passed (to-spec always passes it; an old plan that never declared the gate keeps compiling as it
 * always did), which is what keeps the acceptance contract additive rather than a breaking change.
 *
 * The acceptance feature must ALSO be followed by a promotion ticket -- an implementation feature
 * in the same milestone, blocked by the acceptance feature, writing `.agents/qa/flows/` -- because
 * "we proved it works" without "the passing assertions become a durable flow contract" is a verdict
 * that stops at the checkpoint. Both halves are required together; either alone is a plan that
 * finishes the run and loses the result.
 */
function acceptanceGateErrors(features, assertionIds, milestones) {
  const errs = [];
  const accept = (features || []).filter((f) => f.validatorKind === 'acceptance');
  if (!accept.length) {
    errs.push({ kind: 'acceptance-missing' });
    return errs;
  }
  // One gate, not several: two acceptance features both claim to be the round's last word, and
  // checking only the first would let an incomplete second one ride along unexamined.
  if (accept.length > 1) {
    errs.push({ kind: 'acceptance-duplicate', ids: accept.map((f) => f.id) });
  }
  for (const acc of accept) {
    const proven = new Set(acc.proves || []);
    const missing = [...assertionIds].filter((id) => !proven.has(id));
    if (missing.length) {
      errs.push({ kind: 'acceptance-incomplete', at: acc.id, ids: missing });
    }
    // Terminality is checkable only where the milestone ORDER is known: compile has the spec's
    // Suggested milestones, validate sees a flat features.json. The acceptance gate is the plan's
    // own last word; sitting in an earlier milestone reads as closure while later work can still
    // re-break the contract.
    if (milestones && milestones.length && acc.milestone !== milestones[milestones.length - 1]) {
      errs.push({ kind: 'acceptance-not-terminal', at: acc.id, val: acc.milestone });
    }
    // The promotion ticket is the other half: an implementation feature downstream of the
    // acceptance gate that writes the passing assertions into `.agents/qa/flows/`. It lives in the
    // same milestone so the milestone owns both the gate and its promotion.
    const promo = (features || []).find((f) => f.id !== acc.id &&
      f.reportOnly !== true &&
      (f.dependsOn || []).includes(acc.id) &&
      (f.touches || []).some((t) => t.includes('.agents/qa/')));
    if (!promo) {
      errs.push({ kind: 'acceptance-promotion-missing', at: acc.id });
    } else if (promo.milestone !== acc.milestone) {
      errs.push({ kind: 'acceptance-promotion-milestone', at: acc.id, dep: promo.id });
    }
  }
  return errs;
}

function contractAssertions(contractArg, doc) {
  const contract = doc || readJsonFile(contractArg);
  const arr = contract && contract.assertions;
  if (!Array.isArray(arr)) throw new InputError(`contract ${contractArg}: missing assertions[]`);
  return new Set(arr.map((a) => a && a.id).filter(Boolean));
}

/**
 * The assertion DOCS (not just their ids), keyed by id. validation-check needs the `evidence`
 * text and the `chain` array to enforce the controlled-evidence-type rule (Contract 2), and neither
 * survives in a Set of ids.
 */
function contractAssertionDocs(contractArg, doc) {
  const contract = doc || readJsonFile(contractArg);
  const arr = contract && contract.assertions;
  if (!Array.isArray(arr)) throw new InputError(`contract ${contractArg}: missing assertions[]`);
  const out = new Map();
  for (const a of arr) if (a && a.id) out.set(a.id, a);
  return out;
}

function formatErrors(errs) {
  return errs
    .map((e) => `  ${e.kind}${e.at ? ' @ ' + e.at : ''}${e.dep ? ' -> ' + e.dep : ''}${e.val ? ' val=' + e.val : ''}${e.key ? ' key=' + e.key : ''}${e.ids ? ' ids=' + e.ids.join(',') : ''}`)
    .join('\n');
}

// ---------------------------------------------------------------------------
// compile: tickets + spec -> features.json + validation-contract.json (W8)
//
// The rules lived in to-spec/references/compile-to-features.md and were executed by the model,
// which meant every decision had to be hand-propagated across spec.md, the tickets, and two JSON
// files. A real session spent hundreds of edits doing that and still shipped mismatches. Here the
// tickets are the only source of features, the spec's Validation Plan the only source of
// assertions, and both JSON files are derived. Hand-editing them stops being a thing anyone does.
// ---------------------------------------------------------------------------

class CompileError extends Error {
  constructor(problems) {
    super(`compile failed:\n${problems.map((p) => `  ${p}`).join('\n')}`);
    this.problems = problems;
  }
}

/** Split "rest of line" into the keyword clauses the Validation Plan uses. */
function splitClauses(rest) {
  // `Chain` is the fifth clause: a cross-layer chain of steps a chain assertion walks through.
  // It is a real clause, not a trailing remark, so it must be split out here or a `Chain: a → b`
  // tail gets folded into `Evidence` and the renderer never sees the steps.
  const keys = ['Behavior', 'Surface', 'Tool', 'Evidence', 'Chain'];
  const out = { head: rest.trim(), Behavior: '', Surface: '', Tool: '', Evidence: '', Chain: '' };
  const positions = [];
  for (const k of keys) {
    const i = rest.indexOf(`, ${k}:`);
    if (i >= 0) positions.push({ k, i });
  }
  positions.sort((a, b) => a.i - b.i);
  // Which clauses were WRITTEN, distinct from their values: an empty `Chain:` must be a grammar
  // problem, and a truthiness check on the value cannot tell "empty" from "absent".
  out.present = new Set(positions.map((p) => p.k));
  if (!positions.length) return out;
  out.head = rest.slice(0, positions[0].i).trim();
  for (let n = 0; n < positions.length; n++) {
    const start = positions[n].i + `, ${positions[n].k}:`.length;
    const end = n + 1 < positions.length ? positions[n + 1].i : rest.length;
    out[positions[n].k] = rest.slice(start, end).trim();
  }
  return out;
}

/**
 * The body of a markdown section, up to the next heading at the same level or the end of the file.
 *
 * Scanned rather than matched: JavaScript has no `\Z`, so the obvious regex silently returns
 * nothing for whichever section happens to be last -- and "the last section is invisible" is a
 * failure that looks exactly like "the author forgot to write it".
 */
function markdownSection(text, heading) {
  const lines = text.split('\n');
  const start = lines.findIndex((l) => l.trim() === heading);
  if (start < 0) return '';
  const level = heading.match(/^#+/)[0];
  const out = [];
  for (let i = start + 1; i < lines.length; i++) {
    if (new RegExp(`^${level}(?!#)\\s`).test(lines[i])) break;
    out.push(lines[i]);
  }
  return out.join('\n');
}

/** spec.md -> the assertion enumeration and the milestone list. */
function parseSpec(specPath) {
  const text = fs.readFileSync(specPath, 'utf8');
  const problems = [];
  const section = (name) => markdownSection(text, `## ${name}`);
  const planText = section('Validation Plan');
  if (!planText.trim()) problems.push('spec: no "## Validation Plan" section, so there are no assertions to compile');
  const assertions = [];
  const seen = new Set();
  for (const line of planText.split('\n')) {
    // The id shape the RUNTIME accepts, not a narrower one. `VAL-\d+` silently skipped every
    // area-prefixed id -- `VAL-AUTH-001`, the form ASSERTION_ID_RE's own comment calls "what real
    // contracts use" -- so a spec written in the documented style lost assertions and compile
    // reported success with a smaller number. A producer whose grammar is narrower than its
    // consumer's must say so; it must not quietly emit less than it was given.
    const m = line.match(/^-\s*(VAL-[A-Za-z0-9._-]+)\s*:\s*(.+)$/);
    if (!m) {
      if (/^-\s*VAL/i.test(line)) {
        problems.push(`spec: Validation Plan line does not parse as "- <ID>: <behavior>, Surface: …, Evidence: …": ${line.trim().slice(0, 120)}`);
      }
      continue;
    }
    const [, id, rest] = m;
    if (seen.has(id)) { problems.push(`spec: ${id} appears twice in the Validation Plan`); continue; }
    seen.add(id);
    const c = splitClauses(rest);
    // A chain assertion walks one contract fact across layers (`GUI form -> API -> table -> list`).
    // `Chain` is split out of the tail by splitClauses; here the chain's own grammar is enforced:
    // a chain must be declared as `Surface: chain`, must carry the cross-layer evidence token, and
    // must name at least two steps -- one step is a single observation, not a chain. Steps are
    // joined by `->` or the rightwards arrow (U+2192); both spellings split, and dropping empties
    // keeps a trailing arrow from silently producing a one-step chain. Clause PRESENCE is what
    // gates the grammar: an empty `Chain:` is an authoring error, not a missing clause.
    const hasChain = c.present.has('Chain');
    const chain = hasChain ? c.Chain.split(/\s*(?:\u2192|->)\s*/).map((s) => s.trim()).filter(Boolean) : [];
    if (hasChain) {
      // `Surface: chain` declares a real distinct surface; every other value (or, with the
      // no-Surface error above, no surface at all) would hand a cross-layer fact to a single-layer
      // renderer and lose the layer-to-layer comparison that makes the chain worth writing.
      if (c.Surface && c.Surface !== 'chain') {
        problems.push(`spec: ${id} has "Chain:" but "Surface:" must be "chain", got "${c.Surface}"`);
      }
      if (c.Evidence && !controlledEvidenceTokens(c.Evidence).has('cross-layer')) {
        problems.push(`spec: ${id} is a chain but "Evidence:" must include cross-layer`);
      }
      if (!c.Chain) {
        problems.push(`spec: ${id} has an empty "Chain:" clause; list at least 2 steps joined by "->" or the rightwards arrow`);
      } else if (chain.length < 2) {
        problems.push(`spec: ${id} chain must have at least 2 steps joined by "->" or the rightwards arrow, got ${chain.length}`);
      }
    } else if (c.Surface === 'chain') {
      problems.push(`spec: ${id} has "Surface: chain" but no "Chain:" clause with at least 2 steps`);
    }
    // Two accepted shapes. With an explicit `Behavior:` the head is the title; without one the
    // head IS the behavior, which is how every spec written before this compiler existed reads.
    const title = c.head;
    const behavior = c.Behavior || c.head;
    if (!title) problems.push(`spec: ${id} has no title or behavior`);
    if (!c.Surface) problems.push(`spec: ${id} has no "Surface:" clause; an assertion with no surface cannot be exercised`);
    if (!c.Evidence) problems.push(`spec: ${id} has no "Evidence:" clause; an assertion with no evidence cannot be closed`);
    // `surface` is the category the assertion is exercised through; `tool` is what actually
    // exercises it. A spec that names only one gives both the same value rather than inventing.
    // `chain` travels as its own field so the brief can render the numbered steps and so
    // `contractAssertionHashes` hashes them -- an assertion object that silently dropped the steps
    // would let the verdict and the brief disagree with the contract.
    const assertion = {
      id, title, behavior,
      surface: c.Surface,
      tool: c.Tool || c.Surface,
      evidence: c.Evidence,
      status: 'pending',
    };
    if (c.Chain) assertion.chain = chain;
    assertions.push(assertion);
  }
  if (!assertions.length && planText.trim()) problems.push('spec: the Validation Plan section declares no VAL-xxx lines');

  const handoff = section('Mission Handoff');
  const msLine = handoff.match(/^-\s*Suggested milestones:\s*(.+)$/m);
  const milestones = msLine ? msLine[1].split(',').map((s) => s.trim()).filter(Boolean) : [];
  if (!milestones.length) {
    problems.push('spec: no "## Mission Handoff -> - Suggested milestones:" line; milestone is the unit devflow converges on and cannot be guessed');
  }
  return { assertions, milestones, problems };
}

/** One ticket file -> the fields the mapping table names. */
function parseTicket(file) {
  const text = fs.readFileSync(file, 'utf8');
  const base = path.basename(file);
  const problems = [];
  const one = (re, what, required = true) => {
    const m = text.match(re);
    if (!m && required) problems.push(`${base}: missing ${what}`);
    return m ? m[1].trim() : '';
  };
  const heading = text.match(/^## Issue:\s*(\S+)\s+—\s+(.+)$/m);
  if (!heading) {
    problems.push(`${base}: no "## Issue: <id> — <title>" heading`);
    return { problems };
  }
  const id = heading[1];
  // `<id>-<slug>.md` is the convention; `<id>.md` is the same thing with no slug. What is refused
  // is a filename that names a different id than the heading does.
  if (base !== `${id}.md` && !base.startsWith(`${id}-`)) {
    problems.push(`${base}: filename must be "${id}.md" or start with "${id}-" so the ticket and its feature id cannot drift`);
  }
  const type = one(/^Type:\s*(.+)$/m, '"Type:" (AFK | HITL | VALIDATION | ACCEPTANCE)');
  if (type && !['AFK', 'HITL', 'VALIDATION', 'ACCEPTANCE'].includes(type)) {
    problems.push(`${base}: Type must be AFK, HITL, VALIDATION or ACCEPTANCE, got "${type}"`);
  }
  const description = one(/^Description:\s*(.+)$/m, '"Description:" (the worker\'s brief, compiled verbatim)');
  const milestone = one(/^Milestone:\s*(.+)$/m, '"Milestone:"');
  const blocked = one(/^Blocked by:\s*(.+)$/m, '"Blocked by:" (ids, or None)', type !== 'ACCEPTANCE');
  // An ACCEPTANCE ticket may omit it: the compiler derives its dependsOn from the whole plan
  // (unioned with whatever is authored), so requiring the line would demand a table that is
  // thrown away.
  // Absent is legal and means "unknown", which the scheduler treats as conflicting with
  // everything. It is a deliberate choice to run alone, not an oversight -- so it is not required.
  const touchesLine = one(/^Touches:\s*(.+)$/m, '"Touches:"', false);
  const touches = touchesLine && touchesLine !== 'None'
    ? touchesLine.split(',').map((x) => x.trim()).filter(Boolean)
    : [];
  const dependsOn = blocked && blocked !== 'None'
    ? blocked.split(',').map((s) => s.trim()).filter(Boolean)
    : [];

  const acceptance = [];
  for (const line of markdownSection(text, '### Acceptance criteria').split('\n')) {
    const m = line.match(/^-\s*\[[ xX]\]\s*(.+)$/);
    if (m) acceptance.push(m[1].trim());
  }
  if (!acceptance.length) problems.push(`${base}: no "### Acceptance criteria" checkboxes; expectedBehavior would be empty`);

  const valText = markdownSection(text, '### Validation');
  const ids = (kw) => {
    const m = valText.match(new RegExp(`^${kw}:\\s*(.+)$`, 'm'));
    return m ? m[1].split(',').map((s) => s.trim()).filter(Boolean) : null;
  };
  const fulfills = ids('Fulfills');
  const proves = ids('Proves');
  // ACCEPTANCE is VALIDATION's whole-contract sibling: both are report-only and both claim proof
  // via `Proves:`, never build via `Fulfills:`.
  const proofTicket = type === 'VALIDATION' || type === 'ACCEPTANCE';
  if (proofTicket && fulfills) problems.push(`${base}: a ${type} ticket proves assertions; it must use "Proves:", not "Fulfills:"`);
  if (!proofTicket && proves) problems.push(`${base}: an implementation ticket builds assertions; it must use "Fulfills:", not "Proves:"`);
  if (proofTicket && !(proves || []).length) problems.push(`${base}: a ${type} ticket must name what it proves`);

  // The scheduler-run check, explicit rather than sniffed out of prose: "Command / scenario:"
  // stays a human scenario and never compiles, so no heuristic decides which lines are runnable.
  let verification;
  const vm = valText.match(/^-\s*Verification:\s*(.+?)\s*\(exit (zero|nonzero)\)\s*$/m);
  if (vm) {
    // The ticket writes `cmd` with markdown backticks; strip one surrounding pair so the
    // scheduler's shell runs `cmd`, not a backtick command-substitution (which exits 127).
    const command = vm[1].trim().replace(/^`([^`]*)`$/, '$1');
    verification = { mode: 'steps', steps: [{ id: 'check', command, expectedExit: vm[2] }] };
  } else if (/^-\s*Verification:/m.test(valText)) {
    problems.push(`${base}: a "- Verification:" line must end with "(exit zero)" or "(exit nonzero)" so the expected exit is decided by the plan, not by the runner`);
  }

  return {
    problems, id, type, description, milestone, dependsOn, touches, acceptance,
    fulfills: fulfills || [], proves: proves || [], verification,
  };
}

/** skillName routing (compile rules §3): fix-type work is diagnosed, validators validate, the rest is TDD. */
function routeSkill(ticket) {
  // ACCEPTANCE is the whole-contract runner, not a build: the same surface-exercising skill a
  // VALIDATION feature runs does the run, so both route to user-validate-flow.
  if (ticket.type === 'VALIDATION' || ticket.type === 'ACCEPTANCE') return 'user-validate-flow';
  if (/\b(fix|repair|defect|regression|bug)\b/i.test(`${ticket.description}`)) return 'diagnosing-bugs';
  return 'tdd';
}

/**
 * The compile date, kept off the clock.
 *
 * A compiler whose selling point is determinism must not produce different bytes on Tuesday.
 * Recompiling an unchanged plan keeps the date the previous compile stamped, so the artifact is
 * byte-stable across sessions and a diff means the plan actually changed. The date advances only
 * when the compiled content does. `--generated-at` overrides both.
 */
function stableGeneratedAt(existingPath, compiled) {
  if (fs.existsSync(existingPath)) {
    try {
      const prev = JSON.parse(fs.readFileSync(existingPath, 'utf8'));
      const same = prev.mission === compiled.mission &&
        JSON.stringify(prev.features) === JSON.stringify(compiled.features);
      if (same && typeof prev.generatedAt === 'string' && prev.generatedAt) return prev.generatedAt;
    } catch { /* unreadable previous output: fall through to today */ }
  }
  return new Date().toISOString().slice(0, 10);
}

/**
 * What this slicing will cost at run time, said at compile time.
 *
 * Concurrency admission compares `touches`, so two features claiming one file can never run
 * together. That is discovered today by running: a real 28-hour mission produced 43
 * `touches_overlap` rejections, 2 merged features, and a plan where one page file was claimed by
 * four separate tickets. The information was in the tickets the whole time.
 *
 * Reported, not refused. A wire-up ticket that deliberately owns the files every lane must touch is
 * the correct answer to this problem -- one mature project in the same workspace had arrived at
 * exactly that by hand -- so the compiler's job is to show the shape, not to overrule it.
 */
function concurrencyReport(features) {
  const staticPart = (glob) => {
    let out = '';
    for (const ch of String(glob)) {
      if ('*?['.includes(ch)) break;
      out += ch;
    }
    return out.replace(/\/+$/, '');
  };
  const byPath = new Map();
  for (const f of features) {
    if (f.reportOnly === true) continue;
    for (const t of f.touches || []) {
      const k = staticPart(t);
      if (!k) continue;
      byPath.set(k, [...(byPath.get(k) || []), f.id]);
    }
  }
  const shared = [...byPath.entries()]
    .filter(([, ids]) => new Set(ids).size > 1)
    .map(([p, ids]) => ({ path: p, features: [...new Set(ids)] }))
    .sort((a, b) => b.features.length - a.features.length);

  const build = features.filter((f) => f.reportOnly !== true);
  let blockedPairs = 0;
  for (let i = 0; i < build.length; i += 1) {
    for (let j = i + 1; j < build.length; j += 1) {
      const a = (build[i].touches || []).map(staticPart).filter(Boolean);
      const b = (build[j].touches || []).map(staticPart).filter(Boolean);
      if (!a.length || !b.length) continue;
      if (a.some((x) => b.some((y) => x.startsWith(y) || y.startsWith(x)))) blockedPairs += 1;
    }
  }
  const undeclared = build.filter((f) => !(f.touches || []).length).map((f) => f.id);
  return { shared, blockedPairs, undeclared, buildFeatures: build.length };
}

function cmdCompile(args) {
  const { options } = parseArgs(args);
  const specPath = options['spec'];
  const ticketsDir = options['tickets'];
  const outDir = options['out-dir'] || (specPath ? path.dirname(specPath) : null);
  if (!specPath || !ticketsDir) {
    throw new InputError('usage: devflow.cjs compile --spec <spec.md> --tickets <dir> [--out-dir <dir>] [--mission <name>] [--milestone <name>] [--generated-at <date>] [--require-acceptance]');
  }
  if (!fs.existsSync(specPath)) throw new InputError(`no spec at ${specPath}`);
  if (!fs.existsSync(ticketsDir)) throw new InputError(`no tickets directory at ${ticketsDir}`);

  const spec = parseSpec(specPath);
  const problems = spec.problems.slice();
  const files = fs.readdirSync(ticketsDir).filter((f) => f.endsWith('.md')).sort();
  if (!files.length) problems.push(`${ticketsDir}: no ticket files`);
  let tickets = files.map((f) => parseTicket(path.join(ticketsDir, f)));

  // --milestone compiles ONE milestone's slice, and it exists because 1:1 coverage is checked
  // across the whole plan: a single ticket claiming the wrong assertion fails the compile for every
  // other ticket too. In a real 14-hour session that amplifier produced 155 compiles, 60 proposes
  // across seven session names, and zero merged features -- the loop never started because the spec
  // was still being authored, and every authoring slip cost a full cycle on everything.
  //
  // The coverage rule is right; enforcing it over the whole plan before anything runs is not. A
  // mission can start on the milestone that is ready and grow by `amend`, which is already the
  // supported growth path. Assertions belonging to milestones outside the slice are DEFERRED, not
  // errors -- but they are counted and named, because silently shipping a smaller contract is how a
  // requirement gets lost.
  const sliceOf = options['milestone'] ? String(options['milestone']) : null;
  let deferredAssertions = [];
  if (sliceOf) {
    const inSlice = tickets.filter((t) => t.milestone === sliceOf);
    if (!inSlice.length) {
      throw new InputError(`no ticket declares "Milestone: ${sliceOf}"; the milestones present are ` +
        `${[...new Set(tickets.map((t) => t.milestone).filter(Boolean))].join(', ') || '(none)'}`);
    }
    // A slice that depends on a ticket outside it cannot run: dependsOn would name nothing.
    const ids = new Set(inSlice.map((t) => t.id));
    for (const t of inSlice) {
      for (const d of t.dependsOn || []) {
        if (!ids.has(d)) {
          problems.push(`${t.id}: "Blocked by: ${d}" names a ticket outside milestone "${sliceOf}"; ` +
            `compile that milestone first, or drop the dependency`);
        }
      }
    }
    const claimed = new Set(inSlice.flatMap((t) => [...t.fulfills, ...t.proves]));
    deferredAssertions = spec.assertions.filter((a) => !claimed.has(a.id)).map((a) => a.id);
    spec.assertions = spec.assertions.filter((a) => claimed.has(a.id));
    tickets = inSlice;
  }
  for (const t of tickets) problems.push(...t.problems);

  const known = new Set(spec.assertions.map((a) => a.id));
  const byId = new Map();
  for (const t of tickets) {
    if (!t.id) continue;
    if (byId.has(t.id)) problems.push(`duplicate ticket id "${t.id}"`);
    byId.set(t.id, t);
    if (t.milestone && spec.milestones.length && !spec.milestones.includes(t.milestone)) {
      problems.push(`${t.id}: milestone "${t.milestone}" is not in the spec's Suggested milestones (${spec.milestones.join(', ')})`);
    }
    // The spec's Validation Plan is the only authority for what assertions exist (D9). A ticket
    // that references one it does not enumerate is a mismatch between two sources of one fact --
    // refused here rather than silently repaired into whichever half was written last.
    for (const v of [...t.fulfills, ...t.proves]) {
      if (!known.has(v)) problems.push(`${t.id}: references ${v}, which the spec's Validation Plan does not declare`);
    }
  }
  for (const t of tickets) {
    for (const d of t.dependsOn || []) {
      if (!byId.has(d)) problems.push(`${t.id}: "Blocked by: ${d}" names no ticket`);
    }
  }
  // ACCEPTANCE dependsOn derivation, done BEFORE the problems gate so an authored edge that
  // contradicts the derivation is named here -- not discovered afterwards as a bare DAG cycle.
  // The downstream set is the TRANSITIVE closure of what depends on the acceptance ticket (the
  // promotion ticket, and anything flowing from it): folding any of those into the acceptance
  // feature's own dependencies closes a cycle, because they already reach it through the plan.
  // Fellow acceptance tickets are excluded too -- a gate never needs to wait on another gate;
  // two of them is a plan error the acceptance gate reports as acceptance-duplicate.
  for (const t of tickets) {
    if (t.type !== 'ACCEPTANCE' || !t.id) continue;
    const downstream = new Set();
    let grew = true;
    while (grew) {
      grew = false;
      for (const x of tickets) {
        if (!x.id || x.id === t.id || downstream.has(x.id)) continue;
        if ((x.dependsOn || []).some((d) => d === t.id || downstream.has(d))) {
          downstream.add(x.id);
          grew = true;
        }
      }
    }
    const authoredDownstream = (t.dependsOn || []).filter((d) => downstream.has(d));
    if (authoredDownstream.length) {
      problems.push(`${t.id}: "Blocked by:" names downstream feature(s) ${authoredDownstream.join(', ')} that depend on this acceptance ticket; the compiler derives its dependencies -- drop the edge`);
    }
    t.derivedDependsOn = [...new Set([
      ...t.dependsOn.filter((d) => !downstream.has(d)),
      ...tickets.filter((x) => x.id && x.id !== t.id && x.type !== 'ACCEPTANCE' && !downstream.has(x.id)).map((x) => x.id),
    ])];
  }
  // Build coverage, checked against the spec rather than after the fact: an assertion nobody
  // builds means the plan is incomplete, and an assertion two tickets build is an ownership defect.
  const claims = new Map();
  for (const t of tickets) for (const v of t.fulfills) claims.set(v, [...(claims.get(v) || []), t.id]);
  for (const a of spec.assertions) {
    const c = claims.get(a.id) || [];
    if (!c.length) problems.push(`${a.id} is declared in the spec but no ticket's "Fulfills:" claims it`);
    if (c.length > 1) problems.push(`${a.id} is claimed by more than one ticket (${c.join(', ')})`);
  }
  if (problems.length) throw new CompileError(problems);

  const contract = {
    contract_version: '1',
    authority: `machine contract for ${options['mission'] || path.basename(path.dirname(path.resolve(specPath)))}; spec.md Validation Plan is the human-readable view, this JSON is the machine grammar consumed by devflow.cjs validate`,
    assertions: spec.assertions,
  };
  const features = tickets.map((t) => {
    const f = {
      id: t.id,
      description: t.description,
      dependsOn: t.dependsOn,
      milestone: t.milestone,
      touches: t.touches,
      fulfills: t.fulfills,
    };
    // VALIDATION and its whole-contract sibling ACCEPTANCE are the two report-only kinds. Both
    // claim proof via `proves`, never build; the difference is what they prove and how they're
    // dispatched -- a milestone's assertions for a validator, the entire contract to a gate that
    // runs its own runtime, its own role, and its own environment.
    if (t.type === 'VALIDATION') {
      f.proves = t.proves;
      f.reportOnly = true;
    } else if (t.type === 'ACCEPTANCE') {
      f.proves = t.proves;
      f.reportOnly = true;
      f.validatorKind = 'acceptance';
      // dependsOn was derived above the problems gate: the whole plan, minus the transitive
      // downstream of this ticket (the promotion and anything flowing from it) and minus fellow
      // acceptance tickets. The author does not write the full Blocked-by table.
      f.dependsOn = t.derivedDependsOn || t.dependsOn;
    }
    if (t.verification) f.verification = t.verification;
    f.skillName = routeSkill(t);
    f.expectedBehavior = t.acceptance;
    f.origin = 'plan';
    // No runtime fields: the plan says what to build, the mission's runtime says how far it got.
    // Emitting initialised runtime here is what made an amended plan -- which strips them -- stop
    // validating as the same kind of file it started as.
    f.addressesItemId = [];
    return f;
  });
  const mission = options['mission'] || path.basename(path.dirname(path.resolve(specPath)));
  const doc = {
    mission,
    generatedBy: 'devflow.cjs compile',
    generatedAt: options['generated-at'] || stableGeneratedAt(path.join(outDir, 'features.json'), { mission, features }),
    // This file carries no `status`, and that has to be said IN the file. It is the plan: committed,
    // covered by planDigest, and rewriting any of its fields is refused as `features_tampered`.
    // Status is runtime -- it changes on every receipt, so keeping it here would mean either
    // breaking the digest constantly or teaching the digest to ignore mutable fields, and a digest
    // that ignores fields is one a worker can write through.
    //
    // The note exists because a reader opened this file, found no status, and had no way to learn
    // where it went -- twice. A file that omits something a reader expects owes them the pointer.
    // Not covered by planDigest (META_FIELDS is mission/generatedBy/generatedAt), so it is a note,
    // not a fact anything depends on.
    statusNote: 'This is the plan and holds no status. Live status is in <spec-dir>/missions/runtime.json '
      + '(gitignored, written only by devflow). Read it with: devflow.cjs brief --mission-dir <spec-dir>/missions '
      + '— that answers "is it done", read-only. Never edit this file by hand: append with '
      + "'devflow.cjs amend --add-feature', and drop work with 'devflow.cjs cancel'.",
    features,
  };

  // The compiler does not get to be its own checker: the schema/DAG gates run here, before anything
  // is written. The coverage gates are NOT this command's to enforce -- validate (run with
  // --require-proof-coverage on the compiled output) is the documented gate, and compile is also used
  // standalone to probe narrow properties of partially-proven plans. The acceptance gate behaves the
  // same way: it is enforced here only when --require-acceptance is passed (to-spec always passes
  // it), so a plan that never declared a final acceptance milestone keeps compiling exactly as it
  // always did -- back-compatibility is the whole reason the gate is flag-gated, not unconditional.
  const errs = validateAll(doc, new Set(spec.assertions.map((a) => a.id)));
  if (options['require-acceptance'] !== undefined) {
    errs.push(...acceptanceGateErrors(doc.features, spec.assertions.map((a) => a.id), spec.milestones));
  }
  if (errs.length) throw new CompileError([`the compiled plan does not validate:\n${formatErrors(errs)}`]);

  fs.mkdirSync(outDir, { recursive: true });
  const contractPath = path.join(outDir, 'validation-contract.json');
  const featuresPath = path.join(outDir, 'features.json');
  fs.writeFileSync(contractPath, JSON.stringify(contract, null, 2) + '\n');
  fs.writeFileSync(featuresPath, JSON.stringify(doc, null, 2) + '\n');
  process.stdout.write(
    `compiled ${features.length} feature(s) and ${spec.assertions.length} assertion(s)`
    + (sliceOf ? ` for milestone ${sliceOf}` : '')
    + (deferredAssertions.length
      ? `\n  ${deferredAssertions.length} assertion(s) belong to other milestones and are NOT in this contract: `
        + `${deferredAssertions.join(', ')}\n  append them with their features via 'amend --add-assertion --add-feature' as those milestones are authored`
      : '')
    + (() => {
      const c = concurrencyReport(features);
      const L = [];
      if (c.undeclared.length) {
        L.push(`\n  ${c.undeclared.length} feature(s) declare no "Touches:" and therefore conflict with everything: ${c.undeclared.join(', ')}`);
      }
      if (c.shared.length) {
        L.push(`\n  ${c.shared.length} path(s) are claimed by more than one ticket, so those tickets can never run at the same time:`);
        for (const s2 of c.shared.slice(0, 8)) L.push(`\n    ${s2.path} — ${s2.features.join(', ')}`);
        if (c.shared.length > 8) L.push(`\n    …and ${c.shared.length - 8} more`);
        L.push(`\n  ${c.blockedPairs} of ${(c.buildFeatures * (c.buildFeatures - 1)) / 2} feature pairs are blocked from running together.`);
        L.push(`\n  If a file is one every lane must touch, give it its own wire-up ticket instead of letting each ticket claim it.`);
      }
      return L.join('');
    })()
    + '\n' +
      `  ${contractPath}\n  ${featuresPath}\n`
  );
}

function cmdValidate(args) {
  const { positionals, options } = parseArgs(args);
  if (positionals.length !== 1 || !options['contract']) {
    throw new InputError('usage: devflow.cjs validate <features.json> --contract <validation-contract.json> [--require-proof-coverage] [--require-acceptance] [--max-concurrency <1-3>]');
  }
  const featuresDoc = readJsonFile(positionals[0]);
  const assertions = contractAssertions(options['contract']);
  const errs = validateAll(featuresDoc, assertions);
  // The coverage gate belongs here, not only in loadMission: an unclaimed assertion used to pass
  // validate and propose, then fail at the first run with the plan already locked and no amend path.
  if (!errs.length) errs.push(...coverageErrors(featuresDoc.features, assertions));
  if (!errs.length && proofCoverageApplies(featuresDoc.features, options['require-proof-coverage'] !== undefined)) {
    errs.push(...proofCoverageErrors(featuresDoc.features, assertions));
  }
  // The acceptance gate is flag-gated for the same reason the proof-coverage gate is: an old plan
  // that never declared a final acceptance milestone is not broken by a new gate it never opted
  // into. --require-acceptance (which to-spec's compile flow always passes) turns a whole-contract
  // run without an acceptance/provisioning pairing into a refusal instead of a silent pass.
  if (!errs.length && options['require-acceptance'] !== undefined) {
    errs.push(...acceptanceGateErrors(featuresDoc.features, assertions));
  }
  if (errs.length) {
    throw new RejectError(`validate failed (${errs.length} error${errs.length > 1 ? 's' : ''}):\n${formatErrors(errs)}`, 'validation_failed');
  }
  const proved = proofCoverageApplies(featuresDoc.features, false) ? ', proven >=1' : '';
  let out = `validate OK: ${featuresDoc.features.length} feature(s), ${assertions.size} assertion(s) covered 1:1${proved}`;
  // Schedule pathologies (W-E): the same derivation `plan` reports, but at validate -- where
  // to-spec passes through -- so a plan that cannot run the way it claims is visible before the
  // work starts, not after a day of one rejection at a time. Warnings, not refusals: validate still
  // passes, but each one is a "confirm this was a choice" the operator has to acknowledge.
  const mc = options['max-concurrency'] !== undefined ? Number(options['max-concurrency']) : 2;
  if (!Number.isInteger(mc) || mc < 1 || mc > 3) {
    throw new InputError(`--max-concurrency must be an integer between 1 and 3, got '${options['max-concurrency']}'`);
  }
  const schedule = deriveSchedule(featuresDoc.features, { maxConcurrency: mc });
  const warnings = schedulePathologies(featuresDoc.features, schedule, { maxConcurrency: mc, assertionDocs: contractAssertionDocs(options['contract']) });
  if (warnings.length) {
    out += '\n' + warnings.map((w) => `  schedule warning [${w.kind}] ${w.why}`).join('\n');
  }
  process.stdout.write(out + '\n');
}

// ---------------------------------------------------------------------------
// plan subcommand (W-E): what the plan implies about how it will run.
//
// Queries the SAME admission rule `wave`/`claim` use and projects it against a plan with no running
// state. Never executes, has no authority, and is recomputable at any moment -- it is the answer to
// "what would the waves be, and why", which used to be a runtime surprise.
// ---------------------------------------------------------------------------

/** Simulate the waves `wave` would admit, using the same predicate, without any running state. */
function deriveSchedule(features, { maxConcurrency = 2 } = {}) {
  const cap = maxConcurrency;
  const state = { maxConcurrency: cap };
  const byId = new Map(features.map((f) => [f.id, f]));
  const pending = features.map((f) => Object.assign({}, f, { status: 'pending' }));
  const done = new Set();
  const waves = [];
  let guard = 0;
  while (done.size < features.length) {
    if (++guard > features.length + 1) break; // a dependency cycle would never resolve
    const eligible = pending.filter(
      (f) => !done.has(f.id) && (f.dependsOn || []).every((d) => done.has(d))
    );
    const wave = [];
    for (const f of eligible) {
      // The members admitted so far are the in-flight set for this candidate -- the exact predicate
      // `claim`/`wave` apply (cap, then disjoint writes, report-only vs report-only excepted).
      // Reusing concurrencyBlock keeps one admission rule instead of a second approximation.
      const mini = { features: pending.map((x) => Object.assign({}, x, {
        status: done.has(x.id) ? 'completed' : wave.includes(x) ? 'in_progress' : x.status,
      })) };
      const blocked = concurrencyBlock(state, mini, f);
      if (blocked) continue;
      wave.push(f);
    }
    if (!wave.length) break;
    for (const f of wave) done.add(f.id);
    waves.push(wave.map((f) => f.id));
  }
  // Critical path: the longest dependency chain. `plan` is not gated by `dagErrors` like `validate`
  // is, so a cyclic plan must not blow the stack -- an in-flight set turns a cycle into an empty
  // contribution rather than infinite recursion.
  // A null-prototype object: feature ids are schema-valid arbitrary strings, and `constructor`,
  // `toString`, `hasOwnProperty`, etc. would otherwise hit Object.prototype and corrupt the result.
  const memo = Object.create(null);
  const chain = (id, visiting = new Set()) => {
    // A dependency that names no feature never resolves, so it contributes no real node -- it is a
    // dangling ref, not a chain step. Counting it inflated the critical path and could fabricate
    // a "fake-parallel" diagnosis for a one-feature plan.
    if (!byId.has(id)) return 0;
    if (memo[id] !== undefined) return memo[id];
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const deps = ((byId.get(id) || {}).dependsOn || []).map((d) => chain(d, visiting));
    visiting.delete(id);
    memo[id] = 1 + (deps.length ? Math.max(...deps) : 0);
    return memo[id];
  };
  const criticalPath = features.length ? Math.max(...features.map((f) => chain(f.id))) : 0;
  // Conflict graph over the features that actually write: report-only validators touch nothing.
  const build = features.filter((f) => f.reportOnly !== true);
  const conflicts = [];
  for (let i = 0; i < build.length; i += 1) {
    for (let j = i + 1; j < build.length; j += 1) {
      if (touchesIntersect(build[i].touches, build[j].touches)) conflicts.push({ a: build[i].id, b: build[j].id });
    }
  }
  return { waves, criticalPath, conflicts };
}

/** The scheduling pathologies that used to surface one rejection at a time, at runtime. */
function schedulePathologies(features, schedule, { maxConcurrency = 2, assertionDocs = null } = {}) {
  const warnings = [];
  const build = features.filter((f) => f.reportOnly !== true);
  // (1) a path declared by more than two features.
  const pathCount = new Map();
  for (const f of build) {
    for (const t of f.touches || []) {
      const p = staticPrefix(t);
      if (!p) continue;
      if (!pathCount.has(p)) pathCount.set(p, { path: p, ids: new Set() });
      pathCount.get(p).ids.add(f.id);
    }
  }
  for (const { path: p, ids } of pathCount.values()) {
    if (ids.size > 2) warnings.push({
      kind: 'path-overdeclared', path: p, features: [...ids],
      why: `${ids.size} features claim '${p}': more than two writers to one path serialize into repeated conflict triage`,
    });
  }
  // (2) a build feature with no touches writes "unknown", conflicts with everything and runs serial.
  for (const f of build) {
    if (!f.touches || f.touches.length === 0) warnings.push({
      kind: 'empty-touches', feature: f.id,
      why: `'${f.id}' declares no touches: it conflicts with everything and must run serial -- legal, but confirm it is a choice`,
    });
  }
  // (3.5) an assertion about rendered output claimed by a feature touching no template path.
  // Rendered text cannot be made true without changing the rendering; a real mission shipped a
  // ticket claiming "the fragment contains the priority" while owning no template, and the miss
  // surfaced only at validation time. Heuristic, hence a warning: surface ui/chain, or behavior
  // text naming fragment/render/template/page/browser, needs a template-shaped Touches entry.
  if (assertionDocs) {
    const RENDER_TEXT = /\b(fragment|templates?|renders?|rendered|rendering|page|browser)\b/i;
    const TEMPLATE_PATH = /(^|\/)(templates?|views?|pages|components)(\/|$)|\.(html?|jinja2?|j2|hbs|ejs|mustache|vue|svelte|jsx|tsx|astro)$/i;
    for (const f of features) {
      if (f.reportOnly === true) continue;
      for (const id of f.fulfills || []) {
        const a = assertionDocs.get(id);
        if (!a) continue;
        const impliesRendering = a.tool === 'ui' || a.tool === 'chain'
          || RENDER_TEXT.test(`${a.title || ''} ${a.behavior || ''}`);
        if (!impliesRendering) continue;
        if (!(f.touches || []).some((t) => TEMPLATE_PATH.test(t))) warnings.push({
          kind: 'assertion-seam', feature: f.id, assertion: id,
          why: `'${f.id}' claims ${id} (rendered output) but touches no template path — either the ticket cannot make it true inside its ownership, or Touches forgot the template`,
        });
      }
    }
  }
  // (3) declared concurrency with nothing to parallelise. A parallelism claim needs at least two
  // features to be a lie: a one-feature plan (or an empty one) has, by construction, no chain.
  if (features.length >= 2 && maxConcurrency >= 2 && schedule.criticalPath >= features.length) warnings.push({
    kind: 'fake-parallel', criticalPath: schedule.criticalPath, features: features.length,
    why: `the plan declares concurrency ${maxConcurrency} yet every feature is on the critical path (${schedule.criticalPath}/${features.length}): the claimed parallelism is a chain`,
  });
  // (4) a milestone whose features cannot run together BY DESIGN. The greedy schedule serializes
  // cross-milestone contention too, so it is not the right measure: a milestone whose features
  // could run in parallel but happened to be starved by another milestone's conflict is not a
  // "serial milestone". This asks only whether any pair is admissible at all.
  const byMilestone = new Map();
  for (const f of build) {
    if (!f.milestone) continue;
    if (!byMilestone.has(f.milestone)) byMilestone.set(f.milestone, []);
    byMilestone.get(f.milestone).push(f);
  }
  const byId2 = new Map(features.map((f) => [f.id, f]));
  for (const [ms, members] of byMilestone) {
    if (members.length < 2) continue;
    let anyParallel = false;
    for (let i = 0; i < members.length && !anyParallel; i += 1) {
      for (let j = i + 1; j < members.length; j += 1) {
        const x = byId2.get(members[i].id);
        const y = byId2.get(members[j].id);
        const depends = (x.dependsOn || []).includes(y.id) || (y.dependsOn || []).includes(x.id);
        if (!depends && !touchesIntersect(x.touches, y.touches)) { anyParallel = true; break; }
      }
    }
    if (!anyParallel) warnings.push({
      kind: 'milestone-serial', milestone: ms, features: members.map((f) => f.id),
      why: `milestone '${ms}' has ${members.length} features (${members.map((f) => f.id).join(', ')}) but none can run together: entirely serial by design`,
    });
  }
  return warnings;
}

function cmdPlan(args) {
  const { positionals, options } = parseArgs(args);
  const featuresPath = options['features'] || positionals[0];
  if (!featuresPath) throw new InputError('usage: devflow.cjs plan [--features <features.json>] [--max-concurrency <1-3>] [--json]');
  const maxConcurrency = options['max-concurrency'] !== undefined ? Number(options['max-concurrency']) : 2;
  if (!Number.isInteger(maxConcurrency) || maxConcurrency < 1 || maxConcurrency > 3) {
    throw new InputError(`--max-concurrency must be an integer between 1 and 3, got '${options['max-concurrency']}'`);
  }
  const featuresDoc = readJsonFile(featuresPath);
  if (featuresDoc === null || typeof featuresDoc !== 'object' || Array.isArray(featuresDoc)) {
    throw new InputError(`${featuresPath}: expected a JSON object with a "features" array`);
  }
  if (!Array.isArray(featuresDoc.features)) throw new InputError(`${featuresPath}: missing "features" array`);
  // plan is deliberately lenient (no DAG gate), but a hand-edited file must not crash on a null
  // member or a member without the id deriveSchedule needs -- name it instead.
  featuresDoc.features.forEach((f, i) => {
    if (f === null || typeof f !== 'object' || Array.isArray(f) || typeof f.id !== 'string') {
      throw new InputError(`${featuresPath}: features[${i}] must be an object with a string "id"`);
    }
  });
  const schedule = deriveSchedule(featuresDoc.features, { maxConcurrency });
  const warnings = schedulePathologies(featuresDoc.features, schedule, { maxConcurrency });
  if (options['json'] === true || options['json'] === 'true') {
    process.stdout.write(JSON.stringify({
      maxConcurrency, waves: schedule.waves, criticalPath: schedule.criticalPath,
      conflicts: schedule.conflicts, warnings,
    }, null, 2) + '\n');
    return;
  }
  const L = [];
  L.push(`plan: ${featuresDoc.features.length} feature(s), concurrency ${maxConcurrency}`);
  schedule.waves.forEach((ids, i) => L.push(`  wave ${i + 1}: ${ids.join(', ')}`));
  L.push(`critical path: ${schedule.criticalPath} feature(s)`);
  if (schedule.conflicts.length) {
    L.push('conflicts (overlapping writes):');
    for (const c of schedule.conflicts) L.push(`  ${c.a} <-> ${c.b}`);
  } else {
    L.push('conflicts: none');
  }
  if (warnings.length) {
    L.push('schedule warnings (must be confirmed, not refusals):');
    for (const w of warnings) L.push(`  [${w.kind}] ${w.why}`);
  } else {
    L.push('schedule warnings: none');
  }
  process.stdout.write(L.join('\n') + '\n');
}

// ---------------------------------------------------------------------------
// propose subcommand (FR-011 / VAL-009 / t0)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Forge adapter (t3): the mission's single forge writer, behind `propose --forge`.
//
// Contract (docs/decisions/implemented/architecture/forge-pipeline-adapter-contract.md):
// one owner (this adapter, orchestrator-side only -- worker briefs forbid `git forge`),
// PR creation at merge-gate time, idempotent issue projection (session:<s> label +
// body first line `Ticket: <fid>`), proof tickets closed by one comment + close,
// a named failure taxonomy, a crash reconcile probe keyed by prId that NEVER
// re-invokes `pr merge`, recycle outcome consumption, and progress-log mirroring
// (forge_pr_created / forge_pr_superseded / forge_merged).
//
// Until issue #7 (machine interface) the adapter parses the forge's prose output
// (`issue #<n> created:` / `PR #<n> created:` / `merged PR #<n> into ...`); a parse
// failure is a RECOVERABLE `forge_bridge_failed` pause, never a guess, never a
// duplicate filing.
// ---------------------------------------------------------------------------

/** The prose shapes the adapter parses until #7 lands. */
const FORGE_RE = {
  issueCreated: /^issue #(\d+) created:/m,
  prCreated: /^PR #(\d+) created:/m,
  merged: /merged PR #(\d+) into (\S+) \(([0-9a-f]{7,64})\)/,
  recycleOutcome: /^recycle outcome: ([^\r\n]*)$/m,
  // `pr merge` refusals, matched on the forge's own words (never inferred):
  movedBase: /has moved since PR #\d+ was created/,
  notOnTop: /not directly on top of the snapshot base|merge_base != base_head/,
  ciGate: /is not mergeable/,
  // ci RED: the forge RAN the plan and recorded a failed Check (its own words).
  // Anything else from `ci run` is failure-to-execute — a bridge failure.
  ciRed: /ci run (recorded: failed|for PR #\d+ failed)|CI run for PR #\d+ failed/,
  // Recycle refusal (clause 6 → worktree_partial_removal), both phases: the
  // pre-merge validation (merge NOT executed) and the phase-2 TOCTOU re-check
  // (merge terminal, worktree stays). Consume the exact standalone outcome line.
  recycleRefused: /^recycle outcome: (refused-dirty|refused-not-contained)$/m,
};

/** Run one `git forge` command from the repo root. The caller env names the caller; only the adapter sets it. */
function forgeRun(state, args) {
  const r = spawnSync('git', ['forge', ...args], {
    cwd: state.repoRoot,
    encoding: 'utf8',
    env: Object.assign({}, process.env, { DEVFLOW_FORGE_CALLER: 'adapter' }),
  });
  return { ok: r.status === 0, status: r.status, stdout: (r.stdout || '').trim(), stderr: (r.stderr || '').trim() };
}

/** A RECOVERABLE pause: self-clears on re-run; the error text names the remedy. */
function forgeBridgeError(detail) {
  return new RejectError(
    `${detail} — remedy: re-run the same round; every adapter forge call is idempotent and nothing is duplicated (forge_bridge_failed)`,
    'forge_bridge_failed'
  );
}

/**
 * `propose --forge` binary detection: git resolves `forge` through PATH, so the
 * probe asks git itself. A missing binary is a refused propose, never a mission
 * that discovers it is forge-less at the first merge gate.
 */
function forgeBinaryAvailable(repoRoot) {
  const r = spawnSync('git', ['forge', '--version'], { cwd: repoRoot, encoding: 'utf8' });
  if (r.error) return false;
  const out = `${r.stdout || ''}\n${r.stderr || ''}`;
  if (/is not a git command/i.test(out)) return false;
  return true;
}

/** The ticket's stable body: first line is the machine-matched id, the rest is context. */
function forgeTicketBody(feature) {
  return `Ticket: ${feature.id}\n${feature.description || ''}`;
}

/** Label vocabulary is exactly type:/milestone:/session: — ticket ids never live in labels. */
function forgeIssueLabels(state, feature) {
  const kind = featureKind(feature);
  const type = kind === 'validation' || kind === 'review' ? 'validation' : kind === 'acceptance' ? 'acceptance' : 'afk';
  return [`type:${type}`, `milestone:${feature.milestone || 'misc'}`, `session:${state.sessionId}`];
}

/**
 * File the Issue projection for one ticket — idempotently.
 *
 * Before creating anything, scan the forge's open Issues: one carrying the
 * `session:<s>` label whose body starts `Ticket: <fid>` IS this ticket's
 * projection; reuse it, never re-file. Called at propose (active tickets),
 * amend (additions), and validator injection; a crash between `issue new` and
 * the runtime write replays safely because the scan matches before creating.
 */
function forgeEnsureTicketIssue(missionDir, state, feature) {
  if (feature.issueId) return feature.issueId;
  const list = forgeRun(state, ['issue', 'list']);
  if (!list.ok) throw forgeBridgeError(`git forge issue list failed: ${list.stderr || list.stdout}`);
  for (const m of list.stdout.matchAll(/^#(\d+) /gm)) {
    const id = Number(m[1]);
    const show = forgeRun(state, ['issue', 'show', String(id)]);
    if (!show.ok) continue;
    const labels = ((show.stdout.match(/^labels: (.*)$/m) || [])[1] || '').split(',').map((s) => s.trim());
    if (!labels.includes(`session:${state.sessionId}`)) continue;
    const bodyFirst = ((show.stdout.match(/^description: (.*)$/m) || [])[1] || '').trim();
    if (bodyFirst !== `Ticket: ${feature.id}`) continue;
    // Clause 3: look for an OPEN Issue. `issue list` returns closed ones too
    // (a lost-issueId refile after a proof close/abandon), and a PR --closes
    // aimed at a closed issue would close nothing.
    if (!/^#\d+ .* — open$/m.test(show.stdout)) continue;
    return id; // found the projection: reuse, never re-file
  }
  const labels = forgeIssueLabels(state, feature);
  const created = forgeRun(state, [
    'issue', 'new', `${feature.id}: ${feature.description || feature.id}`, forgeTicketBody(feature),
    ...labels.flatMap((l) => ['--label', l]),
  ]);
  if (!created.ok) throw forgeBridgeError(`git forge issue new failed: ${created.stderr || created.stdout}`);
  const pm = created.stdout.match(FORGE_RE.issueCreated);
  if (!pm) {
    throw forgeBridgeError(`cannot parse 'issue #<n> created:' from forge output: ${created.stdout.slice(0, 200)}`);
  }
  const issueId = Number(pm[1]);
  logEvent(missionDir, 'forge_issue_filed', { featureId: feature.id, issueId });
  return issueId;
}

/** PR labels mirror the ticket vocabulary (clause 4): on Issues and PRs alike. */
function forgePrLabels(state, feature) {
  const labels = forgeIssueLabels(state, feature);
  return labels.filter((l) => !l.startsWith('type:'));
}

/** Create the PR snapshot for one feature at merge-gate time. Returns the prId. */
function forgeCreatePR(missionDir, state, feature) {
  const args = [
    'pr', 'create',
    '--source', feature.branch,
    '--base', state.expectedBranch,
    '--body', `Ticket: ${feature.id}\nmission ${state.sessionId}, feature ${feature.id}`,
    ...forgePrLabels(state, feature).flatMap((l) => ['--label', l]),
  ];
  if (feature.issueId) args.push('--closes', String(feature.issueId));
  args.push(`${feature.id}: ${feature.description || feature.id}`);
  let created = forgeRun(state, args);
  if (!created.ok && /unknown option '--closes'/.test(created.stderr || '')) {
    // An older forge build without --closes: drop the linkage flag and retry.
    // The probe's clause-7 step (2) closes a still-open linked issue either way.
    const i = args.indexOf('--closes');
    created = forgeRun(state, [...args.slice(0, i), ...args.slice(i + 2)]);
  }
  if (!created.ok) throw forgeBridgeError(`git forge pr create failed: ${created.stderr || created.stdout}`);
  const pm = created.stdout.match(FORGE_RE.prCreated);
  if (!pm) {
    throw forgeBridgeError(`cannot parse 'PR #<n> created:' from forge output: ${created.stdout.slice(0, 200)}`);
  }
  const prId = Number(pm[1]);
  logEvent(missionDir, 'forge_pr_created', { featureId: feature.id, prId, issueId: feature.issueId || null });
  return prId;
}

/**
 * The crash reconcile/resume probe (clause 7): idempotent, ordered, keyed by the
 * persisted prId. Forge refs are the authority on what happened; the progress log
 * records what the mission mirrored; runtime.json is rebuilt from the probe —
 * never hand-edited. NEVER re-invokes `pr merge`: the PR is terminal.
 */
function forgeReconcileProbe(missionDir, state, featuresDoc, feature) {
  if (!feature.prId) return null;
  const shown = forgeRun(state, ['pr', 'show', String(feature.prId)]);
  if (!shown.ok) return null; // unknown PR: the forward path recreates; nothing to reconcile
  const merged = shown.stdout.match(/^merged: ([0-9a-f]{7,64})$/m);
  if (!merged) return null; // not merged: the forward merge gate drives from here
  const commitId = merged[1];
  const events = readEvents(missionDir);
  // (1) merged but the runtime never recorded it: mark completed, mirror once.
  if (feature.status !== 'completed') {
    if (!events.some((e) => e.event === 'forge_merged' && e.featureId === feature.id && e.prId === feature.prId)) {
      logEvent(missionDir, 'forge_merged', { featureId: feature.id, prId: feature.prId, commitId, probe: true });
    }
    feature.status = 'completed';
    feature.currentWorkerSessionId = null;
    logEvent(missionDir, 'feature_merged', { featureId: feature.id, branch: feature.branch, commitId, prId: feature.prId, probe: true });
    logEvent(missionDir, 'feature_completed', { featureId: feature.id, commitId, prId: feature.prId, probe: true });
    recordMilestoneCompletions(missionDir, featuresDoc.features);
    injectMilestoneValidators(missionDir, featuresDoc, state);
    sealCompletedMilestones(missionDir, featuresDoc.features, state);
  }
  // (2) merged and the linked issue is somehow still open: closing comment + close.
  if (feature.issueId) {
    const is = forgeRun(state, ['issue', 'show', String(feature.issueId)]);
    if (is.ok && /^#\d+ .* — open$/m.test(is.stdout)) {
      const marker = `forge-issue-close: ${feature.id}`;
      if (!is.stdout.includes(marker)) {
        const comment = forgeRun(state, ['issue', 'comment', String(feature.issueId), `${marker}\nmerged via PR #${feature.prId} (${commitId})`]);
        if (!comment.ok) throw forgeBridgeError(`git forge issue comment failed: ${comment.stderr || comment.stdout}`);
      }
      const closed = forgeRun(state, ['issue', 'close', String(feature.issueId)]);
      if (!closed.ok) throw forgeBridgeError(`git forge issue close failed: ${closed.stderr || closed.stdout}`);
    }
  }
  // (3) merged but the worktree is still present: recycle it (clause 8 outcome
  // handling; forge's own removal already happened inside the merge call, this
  // converges what a crash between the two left behind).
  if (feature.worktree) {
    const rec = recycleFeatureWorktree(missionDir, state, feature, { allowMissingDirectoryRecovery: true });
    if (rec.outcome === 'refused' || rec.outcome === 'partial') {
      state.state = 'paused';
      state.pauseReason = 'worktree_partial_removal';
      writeState(missionDir, state);
    }
  }
  writeFeaturesDoc(missionDir, featuresDoc);
  return { ok: true, commitId, prId: feature.prId };
}

/**
 * The forge-side merge gate (clause 1): PR creation happens HERE — after mission
 * review passed and gate verification — never at worker receipt. Replaces the
 * legacy `git merge --ff-only` for `--forge` missions. Returns the same shape as
 * mergeFeature.
 */
function forgeMergeFeature(missionDir, state, featuresDoc, feature) {
  const branch = feature.branch;
  // Crash resume first: a merged PR must never be re-merged.
  const resumed = forgeReconcileProbe(missionDir, state, featuresDoc, feature);
  if (resumed) return { ok: true, commitId: resumed.commitId };
  let prId = feature.prId || null;
  let supersededFrom = null;
  for (;;) {
    // Ensure the PR exists (created at THIS gate, not at receipt). A persisted
    // prId whose PR vanished forge-side is recreated and superseded on the record.
    if (prId) {
      const show = forgeRun(state, ['pr', 'show', String(prId)]);
      if (show.ok && /abandoned: /.test(show.stdout)) {
        logEvent(missionDir, 'forge_pr_superseded', { featureId: feature.id, oldPrId: prId, newPrId: null, reason: 'previous PR was abandoned' });
        prId = null;
      } else if (!show.ok) {
        // CR-08 (code review): only a CONFIRMED missing PR (the forge's own
        // wording) is recreated — a transient `pr show` failure (bridge error,
        // forge crash, spawn failure) would drop prId and duplicate the PR on
        // the next gate. Transient → recoverable bridge pause (idempotent
        // re-run, no duplicate).
        const err = `${show.stderr}\n${show.stdout}`;
        if (/PR #\d+ does not exist/.test(err)) {
          prId = null;
        } else {
          const be = forgeBridgeError(`git forge pr show ${prId} failed: ${err.slice(0, 300)}`);
          state.state = 'paused';
          state.pauseReason = 'forge_bridge_failed';
          writeState(missionDir, state);
          logEvent(missionDir, 'merge_blocked', { featureId: feature.id, code: 'forge_bridge_failed' });
          logEvent(missionDir, 'forge_bridge_failed', { featureId: feature.id, pauseReason: 'forge_bridge_failed', error: String(be.message).slice(0, 300) });
          writeFeaturesDoc(missionDir, featuresDoc);
          return { ok: false, code: 'forge_bridge_failed', message: be.message };
        }
      } else {
        // CR-06 (code review): the forge's PR snapshot is IMMUTABLE — ci run
        // and pr merge execute against the snapshot refs, never against
        // refs/heads/<branch>. A gate_failed fix round commits new work to
        // feature.branch; reusing the persisted prId would run CI against the
        // OLD snapshot forever (red → fix → red …). Detect a moved source
        // (the `diff: <base>...<source>` line the PR was created from vs the
        // live branch tip) and supersede the PR so the next round recreates
        // it from the fixed branch.
        const snapMatch = show.stdout.match(/^diff: [0-9a-f]{7,64}\.\.\.([0-9a-f]{7,64})$/m);
        if (snapMatch) {
          const live = git(['rev-parse', '--verify', `refs/heads/${feature.branch}`], state.repoRoot);
          if (live.ok && live.stdout !== snapMatch[1]) {
            logEvent(missionDir, 'forge_pr_superseded', { featureId: feature.id, oldPrId: prId, newPrId: null, reason: 'source moved since snapshot (fix round); recreating' });
            forgeRun(state, ['pr', 'abandon', String(prId), '--reason', 'superseded: source moved after a fix round; recreating the snapshot']);
            prId = null;
          }
        }
      }
    }
    if (!prId) {
      try {
        prId = forgeCreatePR(missionDir, state, feature);
        feature.prId = prId;
        if (supersededFrom) {
          // The mirror names old AND new ids so the recreate window stays
          // reconstructable from the log alone (clause 5).
          logEvent(missionDir, 'forge_pr_superseded', { featureId: feature.id, oldPrId: supersededFrom, newPrId: prId, reason: 'stale base' });
          supersededFrom = null;
        }
        // Persist the key BEFORE the merge: a crash after the merge leaves the
        // reconcile probe something to key on (clause 7).
        writeFeaturesDoc(missionDir, featuresDoc);
      } catch (e) {
        // The create attempt failed at the bridge (prCreateFail / parse garbage):
        // the feature's work is done and stays in_review — no work to redo — and
        // the mission pauses RECOVERABLY; re-running the same round self-clears
        // (clause 6). A paused feature would requeue the worker for nothing.
        state.state = 'paused';
        state.pauseReason = 'forge_bridge_failed';
        writeState(missionDir, state);
        logEvent(missionDir, 'merge_blocked', { featureId: feature.id, code: 'forge_bridge_failed' });
        logEvent(missionDir, 'forge_bridge_failed', { featureId: feature.id, pauseReason: 'forge_bridge_failed', error: String(e.message || e).slice(0, 300) });
        writeFeaturesDoc(missionDir, featuresDoc);
        if (e instanceof RejectError) return { ok: false, code: e.code, message: e.message };
        throw e;
      }
    }
    // CI is on-demand (L1): the gate needs a fresh green check for this snapshot.
    const ci = forgeRun(state, ['ci', 'run', String(prId)]);
    if (!ci.ok) {
      const ciOut = `${ci.stdout}\n${ci.stderr}`;
      // ci RED — the forge RAN the plan and recorded a failed Check (its own
      // wording, never inferred): a fix/rebase round follows, budget as today.
      if (FORGE_RE.ciRed.test(ciOut)) {
        feature.status = 'paused';
        state.state = 'paused';
        state.pauseReason = 'gate_failed';
        writeState(missionDir, state);
        logEvent(missionDir, 'forge_ci_failed', { featureId: feature.id, prId, detail: (ci.stderr || ci.stdout).slice(0, 300) });
        logEvent(missionDir, 'feature_paused', { featureId: feature.id, pauseReason: 'gate_failed' });
        writeFeaturesDoc(missionDir, featuresDoc);
        return { ok: false, code: 'gate_failed', message: `PR #${prId} CI is red for '${feature.id}': a fix round follows (gate_failed)` };
      }
      // Failure to EXECUTE (spawn failure, unknown PR, forge crash): no Check
      // was recorded, nothing was measured, no fix round is owed. Budget-neutral
      // bridge failure, same pause shape as every other forge path.
      const err = forgeBridgeError(`git forge ci run failed: ${ciOut.slice(0, 300)}`);
      state.state = 'paused';
      state.pauseReason = 'forge_bridge_failed';
      writeState(missionDir, state);
      logEvent(missionDir, 'merge_blocked', { featureId: feature.id, code: 'forge_bridge_failed' });
      logEvent(missionDir, 'forge_bridge_failed', { featureId: feature.id, pauseReason: 'forge_bridge_failed', error: String(err.message).slice(0, 300) });
      writeFeaturesDoc(missionDir, featuresDoc);
      return { ok: false, code: 'forge_bridge_failed', message: err.message };
    }
    // The ceremony approve mirrors the mission review that already passed to reach
    // this gate (reviewGate verdicts were checked above the forge path).
    // The FORGE's own `pr review` takes no --body (drilled against the real
    // binary: 'unknown option --body'); the mission-review linkage lives in
    // the adapter's event mirror, never in a review body this forge rejects.
    const review = forgeRun(state, ['pr', 'review', String(prId), '--approve']);
    if (!review.ok) {
      // A bridge failure like every other: persist the RECOVERABLE pause and
      // mirror the event — an unrecorded return left nothing on the record
      // naming the remedy (F-007).
      const err = forgeBridgeError(`git forge pr review failed: ${(review.stderr || review.stdout).slice(0, 300)}`);
      state.state = 'paused';
      state.pauseReason = 'forge_bridge_failed';
      writeState(missionDir, state);
      logEvent(missionDir, 'merge_blocked', { featureId: feature.id, code: 'forge_bridge_failed' });
      logEvent(missionDir, 'forge_bridge_failed', { featureId: feature.id, pauseReason: 'forge_bridge_failed', error: String(err.message).slice(0, 300) });
      writeFeaturesDoc(missionDir, featuresDoc);
      return { ok: false, code: 'forge_bridge_failed', message: err.message };
    }
    // The ceremony approve mirrors the mission review that already passed to reach
    const merged = forgeRun(state, ['pr', 'merge', String(prId), '--ff', '--recycle-worktree', feature.worktree]);
    const out = `${merged.stdout}\n${merged.stderr}`;
    // A partial-leftover recycle is reported AFTER a successful merge with a
    // non-zero exit (the forge's own epilogue): the merge line is in the output
    // even though merged.ok is false. Detect it before any refusal class.
    const mergedLine = out.match(FORGE_RE.merged);
    const partialOutcome = out.match(FORGE_RE.recycleOutcome);
    if (!merged.ok && mergedLine && partialOutcome && /partial-leftover/.test(partialOutcome[1])) {
      const commitId = mergedLine[3];
      logEvent(missionDir, 'forge_merged', { featureId: feature.id, prId, commitId });
      logEvent(missionDir, 'feature_merged', { featureId: feature.id, branch, commitId, prId });
      feature.status = 'completed';
      feature.currentWorkerSessionId = null;
      // Persist the outcome BEFORE anything is cleared; the worktree stays.
      logEvent(missionDir, 'forge_recycle_outcome', { featureId: feature.id, prId, outcome: partialOutcome[0], worktree: feature.worktree });
      logEvent(missionDir, 'worktree_recycle_partial', { featureId: feature.id, prId, outcome: partialOutcome[0], keptWorktree: feature.worktree });
      state.state = 'paused';
      state.pauseReason = 'worktree_partial_removal';
      writeState(missionDir, state);
      writeFeaturesDoc(missionDir, featuresDoc);
      // Same trio the clean completion path runs (F-005): the probe skips
      // completed features, so this is the only chance the milestone gets.
      recordMilestoneCompletions(missionDir, featuresDoc.features);
      injectMilestoneValidators(missionDir, featuresDoc, state);
      sealCompletedMilestones(missionDir, featuresDoc.features, state);
      writeFeaturesDoc(missionDir, featuresDoc);
      return { ok: false, code: 'worktree_partial_removal', message: `PR #${prId} merged; recycle outcome: ${partialOutcome[1]} — feature.worktree kept as the recovery record (worktree_partial_removal)`, partial: true };
    }
    // Phase-2 TOCTOU refused-dirty: the merge IS terminal (the merged line is in
    // the output) but the recycle refused, so the exit is non-zero and the
    // worktree stays. Same semantics as partial-leftover: the merge stands,
    // feature.worktree is kept as the recovery record, pause
    // worktree_partial_removal — never the catch-all re-run loop.
    if (!merged.ok && mergedLine && FORGE_RE.recycleRefused.test(out)) {
      const commitId = mergedLine[3];
      const refusal = out.match(FORGE_RE.recycleRefused)[0];
      logEvent(missionDir, 'forge_merged', { featureId: feature.id, prId, commitId });
      logEvent(missionDir, 'feature_merged', { featureId: feature.id, branch, commitId, prId });
      feature.status = 'completed';
      feature.currentWorkerSessionId = null;
      logEvent(missionDir, 'forge_recycle_outcome', { featureId: feature.id, prId, outcome: refusal, worktree: feature.worktree });
      logEvent(missionDir, 'worktree_recycle_partial', { featureId: feature.id, prId, outcome: refusal, keptWorktree: feature.worktree });
      state.state = 'paused';
      state.pauseReason = 'worktree_partial_removal';
      writeState(missionDir, state);
      writeFeaturesDoc(missionDir, featuresDoc);
      // A completion is a completion: the milestone trio runs here exactly as it
      // does on the clean path — the probe skips completed features forever, so
      // a skipped trio here stalls the milestone with no eligible work (F-005).
      recordMilestoneCompletions(missionDir, featuresDoc.features);
      injectMilestoneValidators(missionDir, featuresDoc, state);
      sealCompletedMilestones(missionDir, featuresDoc.features, state);
      writeFeaturesDoc(missionDir, featuresDoc);
      return { ok: false, code: 'worktree_partial_removal', message: `PR #${prId} merged; recycle refused: ${refusal} — feature.worktree kept as the recovery record (worktree_partial_removal)`, partial: true };
    }
    if (!merged.ok) {
      // Recycle refusal (clause 6, phase 1): the forge refused BEFORE any merge
      // side effect — the PR is unmerged, the worktree is untouched, and the
      // work itself is done (in_review). Pause worktree_partial_removal with
      // feature.worktree kept as the recovery record, never the catch-all
      // re-run loop: the remedy is worktree-side (commit/stash/clean), not
      // "re-run the round".
      if (FORGE_RE.recycleRefused.test(out)) {
        state.state = 'paused';
        state.pauseReason = 'worktree_partial_removal';
        writeState(missionDir, state);
        const refusal = out.match(FORGE_RE.recycleRefused)[0];
        logEvent(missionDir, 'forge_recycle_outcome', { featureId: feature.id, prId, outcome: refusal, worktree: feature.worktree });
        logEvent(missionDir, 'worktree_recycle_refused', { featureId: feature.id, prId, error: out.slice(0, 300) });
        logEvent(missionDir, 'merge_blocked', { featureId: feature.id, code: 'worktree_partial_removal' });
        writeFeaturesDoc(missionDir, featuresDoc);
        return { ok: false, code: 'worktree_partial_removal', message: `PR #${prId} NOT merged: ${(merged.stderr || merged.stdout).slice(0, 300)} — feature.worktree kept as the recovery record (worktree_partial_removal)` };
      }
      if (FORGE_RE.movedBase.test(out)) {
        logEvent(missionDir, 'forge_stale_base', { featureId: feature.id, prId });
        forgeRun(state, ['pr', 'abandon', String(prId), '--reason', 'superseded: base moved; recreating the snapshot']);
        supersededFrom = prId;
        prId = null;
        continue;
      }
      if (FORGE_RE.notOnTop.test(out)) {
        return { ok: false, code: 'not_fast_forward', message: `'${branch}' is not on top of ${state.expectedBranch}; rebase it and re-verify (never --no-ff)` };
      }
      if (FORGE_RE.ciGate.test(out)) {
        feature.status = 'paused';
        state.state = 'paused';
        state.pauseReason = 'gate_failed';
        writeState(missionDir, state);
        logEvent(missionDir, 'feature_paused', { featureId: feature.id, pauseReason: 'gate_failed' });
        writeFeaturesDoc(missionDir, featuresDoc);
        return { ok: false, code: 'gate_failed', message: `PR #${prId} refused by the forge gate: ${(merged.stderr || merged.stdout).slice(0, 200)}` };
      }
      // pr merge refusal that is none of the named classes: persistent finalize
      // CAS conflict after the forge's own retries, or an unknown bridge failure.
      const err = forgeBridgeError(`git forge pr merge failed: ${(merged.stderr || merged.stdout).slice(0, 300)}`);
      state.state = 'paused';
      state.pauseReason = 'forge_bridge_failed';
      writeState(missionDir, state);
      logEvent(missionDir, 'merge_blocked', { featureId: feature.id, code: 'forge_bridge_failed' });
      logEvent(missionDir, 'forge_bridge_failed', { featureId: feature.id, pauseReason: 'forge_bridge_failed', error: String(err.message).slice(0, 300) });
      writeFeaturesDoc(missionDir, featuresDoc);
      return { ok: false, code: 'forge_bridge_failed', message: err.message };
    }
    const mm = out.match(FORGE_RE.merged);
    if (!mm) {
      const err = forgeBridgeError(`cannot parse 'merged PR #<n> into <base> (<sha>)' from forge output: ${out.slice(0, 200)}`);
      state.state = 'paused';
      state.pauseReason = 'forge_bridge_failed';
      writeState(missionDir, state);
      logEvent(missionDir, 'merge_blocked', { featureId: feature.id, code: 'forge_bridge_failed' });
      logEvent(missionDir, 'forge_bridge_failed', { featureId: feature.id, pauseReason: 'forge_bridge_failed', error: String(err.message).slice(0, 300) });
      writeFeaturesDoc(missionDir, featuresDoc);
      return { ok: false, code: 'forge_bridge_failed', message: err.message };
    }
    const commitId = mm[3];
    const outcome = (out.match(FORGE_RE.recycleOutcome) || [])[0] || null;
    // Mirror the transition BEFORE anything else is derived from it (clause 5).
    logEvent(missionDir, 'forge_merged', { featureId: feature.id, prId, commitId, oldPrId: supersededFrom || undefined });
    logEvent(missionDir, 'feature_merged', { featureId: feature.id, branch, commitId, prId });
    feature.status = 'completed';
    feature.currentWorkerSessionId = null;
    // Persist the outcome BEFORE clearing feature.worktree/branch (clause 8).
    writeState(missionDir, state);
    writeFeaturesDoc(missionDir, featuresDoc);
    if (outcome && /partial-leftover/.test(outcome)) {
      // The merge is terminal and stands; the leftover worktree is the recovery
      // record (existing worktree_partial_removal semantics).
      logEvent(missionDir, 'forge_recycle_outcome', { featureId: feature.id, prId, outcome });
      logEvent(missionDir, 'worktree_recycle_partial', { featureId: feature.id, prId, outcome, keptWorktree: feature.worktree });
      state.state = 'paused';
      state.pauseReason = 'worktree_partial_removal';
      writeState(missionDir, state);
      // Same trio the clean completion path runs (F-005): the probe skips
      // completed features, so this is the only chance the milestone gets.
      recordMilestoneCompletions(missionDir, featuresDoc.features);
      injectMilestoneValidators(missionDir, featuresDoc, state);
      sealCompletedMilestones(missionDir, featuresDoc.features, state);
      writeFeaturesDoc(missionDir, featuresDoc);
      return { ok: true, commitId, outcome, partial: true };
    }
    logEvent(missionDir, 'forge_recycle_outcome', { featureId: feature.id, prId, outcome: outcome || 'not-requested' });
    // CR-05 (code review): milestonesInRepair keys on `feature_completed` to
    // clear a milestone's gate_failed state after a ci-red round — the probe
    // path emits it, the clean merge path did not, so a milestone that went
    // red and then merged clean stayed "in repair" forever (its repair budget
    // kept being charged for nothing).
    logEvent(missionDir, 'feature_completed', { featureId: feature.id, commitId, prId });
    // Now clear the pointers per the structured outcome.
    feature.worktree = null;
    if (outcome && /branch-kept/.test(outcome)) {
      logEvent(missionDir, 'branch_kept', { featureId: feature.id, branch, outcome });
    } else {
      feature.branch = null; // branch-deleted or already absent
    }
    writeFeaturesDoc(missionDir, featuresDoc);
    recordMilestoneCompletions(missionDir, featuresDoc.features);
    injectMilestoneValidators(missionDir, featuresDoc, state);
    sealCompletedMilestones(missionDir, featuresDoc.features, state);
    writeState(missionDir, state);
    writeFeaturesDoc(missionDir, featuresDoc);
    return { ok: true, commitId, outcome };
  }
}

/**
 * Close one ticket's Issue the way clause 2 names for the non-code paths, with
 * exactly-one-comment idempotence: the marker comment carries the evidence
 * pointer and overall status; a crash between comment and close replays safely.
 */
function forgeCloseTicketIssue(missionDir, state, feature, { marker, body }) {
  if (!feature.issueId) return;
  const is = forgeRun(state, ['issue', 'show', String(feature.issueId)]);
  if (!is.ok) throw forgeBridgeError(`git forge issue show ${feature.issueId} failed: ${is.stderr || is.stdout}`);
  if (!/^#\d+ .* — open$/m.test(is.stdout)) return; // already closed: nothing to do
  if (!is.stdout.includes(marker)) {
    const comment = forgeRun(state, ['issue', 'comment', String(feature.issueId), `${marker}\n${body}`]);
    if (!comment.ok) throw forgeBridgeError(`git forge issue comment failed: ${comment.stderr || comment.stdout}`);
  }
  const closed = forgeRun(state, ['issue', 'close', String(feature.issueId)]);
  if (!closed.ok) throw forgeBridgeError(`git forge issue close failed: ${closed.stderr || closed.stdout}`);
  logEvent(missionDir, 'forge_ticket_closed', { featureId: feature.id, issueId: feature.issueId, marker });
}

/** Proof (report-only) tickets: one comment (evidence pointer + status) + close.
 * The evidence is the validator's REPORT pointer — the artifact the receipt's
 * handoff points at — never the first verification command string, and never a
 * commitId (a report-only receipt must not carry one). The report path is the
 * same one reportDestinationBlock names: reports/<featureId>.json for
 * validation/acceptance, reviews/<featureId>.json for review. */
function forgeCloseProofTicket(missionDir, state, feature, receipt) {
  const kind = featureKind(feature);
  const reportPath = kind === 'review'
    ? path.join(missionDir, 'reviews', `${feature.id}.json`)
    : path.join(missionDir, 'reports', `${feature.id}.json`);
  const evidence = fs.existsSync(reportPath) ? reportPath : 'receipt';
  forgeCloseTicketIssue(missionDir, state, feature, {
    marker: `forge-proof-close: ${feature.id}`,
    body: `status: ${(receipt && receipt.successState) || 'success'}; evidence: ${evidence}`,
  });
}
/**
 * File every active (non-cancelled) ticket; used at propose and after amend.
 * `persist` defaults to the normal runtime write; propose passes its own because
 * state.json does not exist yet and the default write would misread the mission
 * as pre-split.
 */
function forgeFileActiveTickets(missionDir, state, featuresDoc, persist) {
  for (const f of featuresDoc.features) {
    if (f.status === 'cancelled') continue;
    if (!f.issueId) f.issueId = forgeEnsureTicketIssue(missionDir, state, f);
  }
  (persist || writeFeaturesDoc)(missionDir, featuresDoc);
}

function cmdPropose(args) {
  const { options } = parseArgs(args);
  const session = options['session'];
  const workingDirectory = options['working-directory'];
  const featuresPath = options['features'];
  const branch = options['branch'];
  const missionDirArg = options['mission-dir'];
  const contractArg = options['contract'];
  const forgeMode = options['forge'] === true;
  if (!session || !workingDirectory || !featuresPath || !contractArg) {
    throw new InputError(
      'usage: devflow.cjs propose --session <id> --working-directory <path> --features <path> --contract <validation-contract.json> [--branch <name>] [--mission-dir <dir>] [--skills-root <dir>] [--artifact-root <dir>] [--validators] [--skip-scrutiny] [--skip-assertions] [--require-proof-coverage] [--forge]'
    );
  }
  // Concurrency is declared once, at propose, and locked into the mission: a run that behaves
  // differently on a bigger machine is a run nobody can reproduce. 2 by default, 3 at most --
  const maxConcurrency = options['max-concurrency'] !== undefined ? Number(options['max-concurrency']) : 2;
  if (!Number.isInteger(maxConcurrency) || maxConcurrency < 1 || maxConcurrency > 3) {
    throw new InputError(`--max-concurrency must be an integer between 1 and 3, got '${options['max-concurrency']}'`);
  }
  // session is a missionDir path segment: no path separators, no traversal (containment)
  if (!/^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(session)) {
    throw new RejectError(`invalid session id '${session}': must match ^[a-zA-Z0-9][a-zA-Z0-9_-]*$ (no path separators)`, 'invalid_session_id');
  }
  if (!fs.existsSync(workingDirectory) || !fs.statSync(workingDirectory).isDirectory()) {
    throw new RejectError(`working-directory not a directory: ${workingDirectory}`, 'working_directory_missing');
  }
  // (1) git repository; take the canonical repoRoot
  const top = git(['rev-parse', '--show-toplevel'], workingDirectory);
  if (!top.ok) throw new RejectError(`working-directory is not a git repository: ${workingDirectory}`, 'not_a_git_repository');
  const repoRoot = top.stdout;
  // ② expectedBranch
  const headRef = git(['symbolic-ref', '--short', 'HEAD'], repoRoot);
  if (!headRef.ok) throw new RejectError('HEAD is detached; propose requires a symbolic branch', 'detached_head');
  const expectedBranch = branch || headRef.stdout;
  const verify = git(['rev-parse', '--verify', `refs/heads/${expectedBranch}`], repoRoot);
  if (!verify.ok) throw new RejectError(`branch does not exist: ${expectedBranch}`, 'branch_not_found');
  // (3) HEAD symbolic branch == expectedBranch
  if (headRef.stdout !== expectedBranch) {
    throw new RejectError(
      `HEAD is on '${headRef.stdout}' but expectedBranch is '${expectedBranch}'; propose does not auto-checkout`,
      'branch_mismatch'
    );
  }
  // (4) clean worktree (including unmerged conflicts -- the U states in porcelain)
  const st = git(['status', '--porcelain'], repoRoot);
  if (!st.ok) throw new RejectError('git status --porcelain failed', 'git_status_failed');
  if (st.stdout.length > 0) {
    throw new RejectError(
      `working tree is dirty (${st.stdout.split('\n').length} entr${st.stdout.split('\n').length > 1 ? 'ies' : 'y'}); propose refuses to run over uncommitted user edits`,
      'dirty_worktree'
    );
  }
  // (4b) The contract must be under version control. propose locks its path and digest and run
  // re-reads it every round, so an untracked contract has nothing to compare against, no history,
  // and `git clean` removes it. A clean worktree does not imply tracked: a wholly-ignored .specs/
  // is clean and unversioned at the same time, which the previous docs/specs/ location made
  // unlikely and this one makes easy.
  requireTrackedIfInsideRepo(contractArg, repoRoot, 'contract');
  // Same rule for the plan: propose stores its path as the ongoing plan authority and reopens it on
  // every mission load. An untracked `.specs/.../features.json` passes the clean-worktree gate and
  // is then removed by `git clean -fdx`, leaving the mission with `features_source_missing` and no
  // committed approved plan to restore.
  if (featuresPath !== contractArg) requireTrackedIfInsideRepo(featuresPath, repoRoot, 'features');
  // (5) features check: full validate (including dangling fulfills, with --contract as the assertion authority)
  const featuresDoc = readJsonFile(featuresPath);
  const assertions = contractAssertions(contractArg);
  const errs = validateAll(featuresDoc, assertions);
  if (!errs.length) errs.push(...coverageErrors(featuresDoc.features, assertions));
  // Same rule as validate: a plan that proves some of its contract must prove all of it. Catching
  // it here matters more than at validate -- past this point planDigest is locked and there is no
  // amend path, so a half-proven plan would only surface with the mission already immutable.
  if (!errs.length && proofCoverageApplies(featuresDoc.features, options['require-proof-coverage'] !== undefined)) {
    errs.push(...proofCoverageErrors(featuresDoc.features, assertions));
  }
  if (errs.length) {
    throw new RejectError(`features.json validation failed:\n${formatErrors(errs)}`, 'features_invalid');
  }
  // Initial-state normalization gate: propose only accepts an all-pending initial plan (stops a
  // preset completed from bypassing run's receipt checks). A plan carrying no runtime at all IS
  // initial -- that is what a compiled plan looks like now, and absent must not read as suspicious.
  for (const f of featuresDoc.features) {
    const status = f.status === undefined ? 'pending' : f.status;
    const current = f.currentWorkerSessionId === undefined ? null : f.currentWorkerSessionId;
    if (status !== 'pending' || current !== null || (f.workerSessionIds || []).length > 0) {
      throw new RejectError(
        `feature ${f.id} must be initial (status=pending, workerSessionIds=[], currentWorkerSessionId=null) at propose; got status=${status}`,
        'initial_state_must_be_pending'
      );
    }
  }
  // ⑥ missionDir = specsRoot/<sessionId>/missions, alongside the session's spec artifacts.
  // canonicalize: a relative path would break when resolved under the worker cwd (the code repository) (ENOENT)
  const missionDir = missionDirFor(missionDirArg || specsRootFor(repoRoot), session);
  // The runtime sits INSIDE the repository by design, next to the spec it belongs to. That is only
  // safe while git ignores it: propose and every spawn demand a clean worktree, so an unignored
  // mission directory dirties the tree the moment it is written. Refusing here beats the
  // alternative -- the mission proposes fine and then pauses on the first spawn with
  // dirty_worktree, naming a symptom two steps away from the cause.
  requireIgnoredIfInsideRepo(missionDir, repoRoot);
  if (fs.existsSync(statePath(missionDir))) {
    // The recovery this names has to be the cheapest one that works, because the expensive one is
    // what people actually do: a message whose only exit is "start over" gets taken literally, and
    // four missions over one plan is what that looks like a day later.
    throw new RejectError(
      `session already proposed: ${session} (${statePath(missionDir)}). ` +
        `To add work to it, append with 'amend --add-feature' — the running mission picks it up. ` +
        `Only a recompiled plan needs a new session, because planDigest is a snapshot of the approved one.`,
      'already_proposed'
    );
  }
  fs.mkdirSync(missionDir, { recursive: true });
  const digest = planDigest(featuresDoc.features, featuresDoc);
  // No plan snapshot: the plan is the committed file named below, and missionDir holds only what
  // must not be in git. A copy here is what put an amended plan somewhere git could not see.
  fs.mkdirSync(missionDir, { recursive: true });
  // Where the plan came from, and what it looked like. The contract has been watched this way
  // since it existed; the plan source was read once and never looked at again, so recompiling it
  // mid-mission diverged the committed plan from the running one with nothing to notice.
  const featuresSourcePath = realpathOrNull(featuresPath) || path.resolve(featuresPath);
  writeMissionRuntime(missionDir, featuresDoc, new Set(), { featuresSourcePath });
  const artifactRoot = options['artifact-root']
    ? realpathOrNull(options['artifact-root']) || path.resolve(options['artifact-root'])
    : null;
  const skillBindings = resolveSkillBindings(featuresDoc.features, repoRoot, options['skills-root'], artifactRoot);
  const state = {
    missionId: featuresDoc.mission, // validateAll guarantees mission is non-empty
    sessionId: session,
    workingDirectory: repoRoot,
    repoRoot,
    expectedBranch,
    planDigest: digest,
    featuresSourcePath,
    contractPath: realpathOrNull(contractArg) || path.resolve(contractArg), // canonical: run re-reads the same file from any cwd
    contractDigest: crypto.createHash('sha256').update(fs.readFileSync(contractArg)).digest('hex'),
    skillBindings,
    artifactRoot,
    // Injection is opt-in while the validators have no gate to run yet (t3/t4). Turning it on
    // by default would add two features to every existing mission that nothing can execute.
    // The default flips once the scrutiny gate and the assertion ledger land.
    validatorsRequested: options['validators'] !== undefined,
    repairBudget: parseRepairBudget(options['repair-budget']),
    gateCommands: parseGateCommands(options['gate-command']),
    reviewChecklist: parseReviewChecklist(options['review-checklist']),
    validatorKinds:
      options['validators'] === undefined
        ? []
        : ['scrutiny', 'assertions'].filter((k) => options[`skip-${k}`] === undefined),
    createdAt: new Date().toISOString(),
    state: 'initializing',
    activeWorkerSessionId: null,
    activeWorkers: [],
    worktreeRoot: options['worktree-root'] ? path.resolve(options['worktree-root']) : defaultWorktreeRoot(repoRoot, session),
    maxConcurrency,
    reviewGate: options['review-gate'] === true || options['review-gate'] === 'true',
    // Mission-scoped forge mode (t3): PRs are created at merge-gate time, issues are
    // filed for active tickets, and the merge runs through `git forge pr merge --ff`.
    // Missions proposed without --forge keep old-path merge semantics to completion.
    forge: forgeMode,
  };
  if (forgeMode && !forgeBinaryAvailable(repoRoot)) {
    throw new RejectError(
      '--forge: no git-forge binary available on PATH (install git-forge so `git forge` resolves, then re-run propose)',
      'forge_binary_missing'
    );
  }
  if (forgeMode) {
    // Filing runs BEFORE state.json exists: a bridge failure here aborts the propose
    // cleanly (nothing persisted) and a retry reuses the already-filed Issues.
    forgeFileActiveTickets(missionDir, state, featuresDoc, (md, doc) => {
      // Pre-state propose write: prId/issueId are runtime, so persist them the way
      // writeMissionRuntime does without misreading the mission as pre-split.
      writeMissionRuntime(md, doc, new Set(), { featuresSourcePath });
    });
  }
  fs.writeFileSync(statePath(missionDir), JSON.stringify(state, null, 2) + '\n');
  writeLedgerProjection(missionDir, assertionLedger([...assertions], []));
  logEvent(missionDir, 'mission_proposed', {
    sessionId: session,
    repoRoot,
    expectedBranch,
    planDigest: digest,
    featuresPath,
    // The baseline an append is measured against. A feature present later but absent here, with
    // no event attesting how it arrived, is a plan that grew without anyone recording why.
    plannedFeatureIds: featuresDoc.features.map((f) => f.id),
    assertionHashes: contractAssertionHashes(state.contractPath),
  });
  process.stdout.write(`${missionDir}\n`);
}

// ---------------------------------------------------------------------------
// receipt-check subcommand (FR-010 / VAL-002 / t2)
// ---------------------------------------------------------------------------

/**
 * Receipt classification (FR-010 exit codes): missing field / malformed -> input (exit 2, state untouched);
 * semantic rejection (illegal value, ownership, duplicate, stale) -> semantic (exit 1).
 */
function receiptErrors(receipt, opts) {
  // reportOnly comes from the PLAN (planDigest-hashed), never from the receipt: a worker that
  // could declare itself report-only would have a one-line escape from the git hard checks.
  const reportOnly = !!(opts && opts.reportOnly);
  // Same for the acceptance kind: the environment obligation below binds because the PLAN says
  // this feature is the whole-contract gate, not because the receipt claims it.
  const acceptance = !!(opts && opts.acceptance);
  const input = [];
  const semantic = [];
  if (typeof receipt !== 'object' || receipt === null) {
    return { input: [{ kind: 'not-object' }], semantic: [] };
  }
  if (receipt.successState === undefined) input.push({ kind: 'missing-successState' });
  else if (!SUCCESS_STATES.includes(receipt.successState)) semantic.push({ kind: 'bad-successState', value: receipt.successState });
  if (receipt.successState === 'success') {
    if (receipt.validatorsPassed === undefined) input.push({ kind: 'missing-validatorsPassed' });
    else if (receipt.validatorsPassed !== true) semantic.push({ kind: 'validators-not-passed' });
    // A report-only feature proves assertions and commits nothing; it may still commit test
    // fixtures, and when it does the commit is verified exactly as any other.
    const claimsCommit = typeof receipt.commitId === 'string' && receipt.commitId !== '';
    if (!reportOnly || claimsCommit) {
      if (!claimsCommit) input.push({ kind: 'missing-commitId' });
      if (typeof receipt.repoPath !== 'string' || receipt.repoPath === '') input.push({ kind: 'missing-repoPath' });
    }
    if (receipt.failureClass !== undefined) semantic.push({ kind: 'failureClass-on-success' });
  } else if (receipt.successState === 'partial' || receipt.successState === 'failure') {
    if (receipt.failureClass === undefined) input.push({ kind: 'missing-failureClass' });
    else if (!FAILURE_CLASSES.includes(receipt.failureClass)) semantic.push({ kind: 'bad-failureClass', value: receipt.failureClass });
  }
  const h = receipt.handoff;
  if (typeof h !== 'object' || h === null) {
    input.push({ kind: 'missing-handoff' });
  } else {
    // An acceptance feature owns the environment (acceptance.md §2): it stood the system up,
    // proved it healthy, and tore it down. A success receipt without that record leaves the
    // whole-contract proof resting on whatever happened to be running — the one assumption the
    // role exists to eliminate. healthCheck names the probe; tornDown is the teardown claim and
    // is semantic because a false one is a lie about the world, not a typo.
    if (acceptance && receipt.successState === 'success') {
      const env = h.environment;
      if (typeof env !== 'object' || env === null || Array.isArray(env)) {
        input.push({ kind: 'missing-handoff-environment' });
      } else {
        if (typeof env.healthCheck !== 'string' || !env.healthCheck) input.push({ kind: 'missing-environment-healthCheck' });
        if (env.tornDown !== true) semantic.push({ kind: 'environment-not-torn-down' });
      }
    }
    for (const k of HANDOFF_REQUIRED) {
      if (h[k] === undefined) input.push({ kind: `missing-handoff-${k}` });
    }
    // Optional on purpose, and checked when it is there. `skillFeedback` is where a worker records
    // that it departed from its procedure and why -- worker.md gives it a schema and SKILL.md calls
    // it "the only channel back". It was in neither the receipt schema nor any reader: four of five
    // real workers filled it in, including both that disclosed a genuine deviation, and every one of
    // those signals went into a JSON key nothing looked at. Requiring it would be worse: a mandatory
    // feedback field gets filled with "followedProcedure: true" to satisfy the check.
    // Operational facts a worker had to discover: how this repository builds and tests. They used
    // to be requested through `discoveredIssues`, which is a channel for problems -- so they arrived
    // labelled as issues, nothing routed them, and every worker rediscovered them. A field shaped
    // for the fact is what lets the next brief carry it, which is what every brief already promises.
    if (h.opsFindings !== undefined) {
      if (!Array.isArray(h.opsFindings)) {
        input.push({ kind: 'malformed-opsFindings', value: h.opsFindings });
      } else {
        for (const o of h.opsFindings) {
          if (!o || typeof o !== 'object' || !o.fact) {
            semantic.push({ kind: 'opsFinding-without-a-fact', value: o });
          }
        }
      }
    }
    if (h.skillFeedback !== undefined) {
      const sf = h.skillFeedback;
      if (typeof sf !== 'object' || sf === null || Array.isArray(sf)) {
        input.push({ kind: 'malformed-skillFeedback', value: sf });
      } else {
        if (sf.deviations !== undefined && !Array.isArray(sf.deviations)) {
          input.push({ kind: 'malformed-skillFeedback-deviations', value: sf.deviations });
        }
        for (const d of Array.isArray(sf.deviations) ? sf.deviations : []) {
          // A deviation nobody can act on is noise. The three fields are what make it actionable:
          // which step, what happened instead, and why.
          if (!d || typeof d !== 'object' || !d.step || !d.whatIDidInstead || !d.why) {
            semantic.push({ kind: 'skillFeedback-deviation-incomplete', value: d });
          }
        }
        if (sf.followedProcedure === false && !(Array.isArray(sf.deviations) && sf.deviations.length)) {
          semantic.push({ kind: 'skillFeedback-claims-deviation-without-naming-one' });
        }
      }
    }
  }
  return { input, semantic };
}

/**
 * The plan's report-only flag for a feature id; missing mission/feature -> false.
 * The worker can write missionDir, so the flag is honored only when the snapshot still
 * matches the planDigest locked at propose -- otherwise a worker could grant itself the
 * commit exemption by editing one boolean in features.json.
 */
function featureIsReportOnly(missionDir, featureId, state) {
  if (!featureId) return false;
  let doc;
  try {
    doc = readMissionFeatures(missionDir, state || readState(missionDir));
  } catch {
    return false;
  }
  const f = (doc.features || []).find((x) => x && x.id === featureId);
  if (!f || f.reportOnly !== true) return false;
  // Injected ids must be excluded here exactly as they are everywhere else the digest is checked.
  // Without them the digest never matches after an injection, `reportOnly` silently reads as
  // false, and the injected validator's commit-less receipt is rejected -- so it requeues on
  // every run and the mission can never finish.
  if (state && state.planDigest &&
      planDigest(doc.features, doc, injectedFeatureIds(readEvents(missionDir))) !== state.planDigest) {
    return false;
  }
  return true;
}
/**
 * The plan's validatorKind for a feature id (null when absent/unreadable). Same digest discipline
 * as featureIsReportOnly: the kind is a planning field under planDigest, and a tampered snapshot
 * must fail closed — an acceptance receipt's environment obligation is waived when the kind cannot
 * be trusted, never when it can.
 */
function featureValidatorKind(missionDir, featureId, state) {
  if (!featureId) return null;
  let doc;
  try {
    doc = readMissionFeatures(missionDir, state || readState(missionDir));
  } catch {
    return null;
  }
  const f = (doc.features || []).find((x) => x && x.id === featureId);
  if (!f || typeof f.validatorKind !== 'string' || !f.validatorKind) return null;
  if (state && state.planDigest &&
      planDigest(doc.features, doc, injectedFeatureIds(readEvents(missionDir))) !== state.planDigest) {
    return null;
  }
  return f.validatorKind;
}

function cmdReceiptCheck(args) {
  const { positionals, options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  if (positionals.length !== 1 || !missionDir) {
    throw new InputError('usage: devflow.cjs receipt-check <receipt.json> --mission-dir <dir> [--feature <id>]');
  }
  const receipt = readJsonFile(positionals[0]);
  const featureId = options['feature'] || process.env.FEATURE_ID || null;
  // Read once: the flag check needs the locked planDigest and the git checks below need the
  // same state. A non-success receipt never reaches the git path, so an unreadable state is
  // only fatal there.
  let state = null;
  try {
    state = readState(missionDir);
  } catch {
    state = null;
  }
  const reportOnly = featureIsReportOnly(missionDir, featureId, state);
  // (1) strict schema: missing field / malformed -> input error exit 2 (state untouched); semantic rejection -> exit 1
  const acceptance = featureValidatorKind(missionDir, featureId, state) === 'acceptance';
  const { input, semantic } = receiptErrors(receipt, { reportOnly, acceptance });
  if (input.length) {
    throw new InputError(
      `receipt missing/malformed fields:\n` +
        input.map((e) => `  ${e.kind}${e.value !== undefined ? ' value=' + JSON.stringify(e.value) : ''}`).join('\n')
    );
  }
  if (semantic.length) {
    throw new RejectError(
      `receipt semantic validation failed:\n` +
        semantic.map((e) => `  ${e.kind}${e.value !== undefined ? ' value=' + JSON.stringify(e.value) : ''}`).join('\n'),
      'receipt_semantic_failed'
    );
  }
  // partial/failure need no git checks (no commit was promised)
  if (receipt.successState !== 'success') {
    process.stdout.write('receipt-check OK (non-success, schema only)\n');
    return;
  }
  if (commitExempt(receipt, reportOnly)) {
    process.stdout.write('receipt-check OK (report-only feature, no commit claimed)\n');
    return;
  }
  // (2) repository identity + refs -- through the SAME function the transaction uses.
  //
  // This used to be a second, inlined copy of those checks, and it had drifted away from the
  // original in three places at once: repository identity compared paths instead of git-common-dirs
  // (so a receipt from the feature's own worktree -- the only place the brief tells a worker to
  // work -- failed as REPOSITORY_MISMATCH), branch membership was checked against the integration
  // branch rather than the feature's, and the baseline was the first active worker's, which under
  // concurrency can belong to a different feature entirely. The result was a diagnostic that called
  // a valid receipt a forgery, in exactly the situation it exists for.
  //
  // validation-protocol.md says every path that consumes a receipt applies the same rule. It says
  // that because it must be true, not because it was.
  if (!state) state = readState(missionDir); // surfaces the original input error for the git path
  // Tolerantly: this is a diagnostic, and someone runs it precisely when the mission is in a state
  // they do not understand. Routing it through the plan reader made it die with
  // `features_source_missing` on a mission whose plan is gone -- refusing to run at the only moment
  // it is needed, which is the failure `brief` was built to avoid. The branch and session the git
  // checks need are runtime facts, and runtime.json holds them whatever the plan is doing.
  let feature = null;
  let planNote = null;
  try {
    const doc = readMissionFeatures(missionDir, state);
    feature = featureId ? (doc.features || []).find((f) => f.id === String(featureId)) : null;
    if (featureId && !feature) throw new RejectError(`unknown feature '${featureId}' in this mission's plan`, 'unknown_feature');
  } catch (e) {
    if (e instanceof RejectError && e.code === 'unknown_feature') throw e;
    planNote = e.message;
    if (featureId && fs.existsSync(runtimePath(missionDir))) {
      try {
        const rt = JSON.parse(fs.readFileSync(runtimePath(missionDir), 'utf8'));
        const r = (rt.features || {})[String(featureId)];
        if (r) feature = { id: String(featureId), branch: r.branch || null, worktree: r.worktree || null,
          currentWorkerSessionId: r.currentWorkerSessionId || null, workerSessionIds: r.workerSessionIds || [] };
      } catch { /* unreadable runtime too: fall through to the mission-level defaults */ }
    }
  }
  // The feature's own last session, so the baseline is the one this receipt was built against. The
  // old code took the first ACTIVE worker's, which under concurrency belongs to another feature.
  const sessionId = (feature && (feature.currentWorkerSessionId || (feature.workerSessionIds || []).slice(-1)[0]))
    || activeWorkerIds(state)[0];
  const out = verifyReceiptGit(receipt, state, missionDir, sessionId, feature);
  if (!out.ok) throw out.err;
  const branch = (feature && feature.branch) || state.expectedBranch;
  process.stdout.write(`receipt-check OK: ${receipt.commitId} on ${branch}\n`);
  if (planNote) process.stdout.write(`note: the plan could not be read (${planNote}); judged from runtime state alone\n`);
}


function lastBaseline(missionDir, workerSessionId) {
  // The most recent worker_started event for this worker session in progress_log (carries baselineCommit)
  const p = path.join(missionDir, 'progress_log.jsonl');
  if (!fs.existsSync(p)) return null;
  const lines = fs.readFileSync(p, 'utf8').split('\n').filter(Boolean);
  for (let i = lines.length - 1; i >= 0; i--) {
    try {
      const ev = JSON.parse(lines[i]);
      if (ev.event === 'worker_started' && ev.workerSessionId === workerSessionId && ev.baselineCommit) {
        return ev.baselineCommit;
      }
    } catch {
      /* skip malformed line */
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// log subcommand: append an event to progress_log.jsonl
// ---------------------------------------------------------------------------

function cmdLog(args) {
  const { positionals, options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  if (positionals.length < 1 || !missionDir) {
    throw new InputError('usage: devflow.cjs log --mission-dir <dir> <event> [--json <payload>]');
  }
  const event = positionals[0];
  // Authoritative events are writable only on the internal path (logEvent); the CLI log rejects state-machine events -- stops a worker forging receipts/state
  if (/^(worker_|feature_|mission_)/.test(event)) {
    throw new RejectError(
      `event '${event}' is a state-machine authority event and cannot be written via CLI log (single receipt entry = end-feature)`,
      'log_authority_event_blocked'
    );
  }
  let payload = {};
  const jsonArg = options['json'];
  if (jsonArg) {
    try {
      payload = JSON.parse(jsonArg);
    } catch (e) {
      throw new InputError(`--json payload is not valid JSON: ${e.message}`);
    }
  }
  if (!fs.existsSync(missionDir)) throw new InputError(`missionDir does not exist: ${missionDir}`);
  logEvent(missionDir, event, payload);
  process.stdout.write(`logged ${event}\n`);
}

// ---------------------------------------------------------------------------
// end-feature subcommand (FR-010 single receipt entry; env locates it, plus ownership/single-shot and hard checks)
// Invocation: cat receipt.json | devflow.cjs end-feature (env: MISSION_DIR/WORKER_SESSION_ID/FEATURE_ID)
// ---------------------------------------------------------------------------

/** How long a worker waits for the receipt lock before giving up. */
const MISSION_LOCK_WAIT_MS = 30000;

/**
 * end-feature transaction lock: the ownership/single-shot checks and the event write must form one atomic transaction (FR-010 single-shot).
 *
 * Acquired atomically via mkdir. **A held lock is waited for, not refused.** It used to be refused
 * outright, which was harmless while only one worker could exist and became a defect the moment
 * concurrency was admitted: two workers whose `touches` are disjoint are *supposed* to finish at
 * the same time, and the loser's receipt -- committed, on its branch, correct -- was thrown away
 * with `mission_busy` and the work redone. The transaction is short (a few file writes), so the
 * wait is short too; `mission_busy` now means the holder is wedged, not merely busy.
 *
 * A stale lock (>60s = crash leftover, or one whose owner file was never written) is removed and
 * retried. A freshly created lock is never stolen, so a lock-creation race cannot delete someone
 * else's lock.
 */
function acquireMissionLock(missionDir, waitMs = MISSION_LOCK_WAIT_MS) {
  const lockPath = path.join(missionDir, '.end-feature.lock');
  const tryAcquire = () => {
    try {
      fs.mkdirSync(lockPath);
      fs.writeFileSync(path.join(lockPath, 'owner'), `${process.pid}@${new Date().toISOString()}`);
      return lockPath;
    } catch (e) {
      if (e.code !== 'EEXIST') throw e;
      return null;
    }
  };
  const sleep = (ms) => {
    // Synchronous on purpose: this process has nothing else to do until it owns the lock, and the
    // callers are spread across sync and async paths.
    try {
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
    } catch {
      const until = Date.now() + ms;
      while (Date.now() < until) { /* fallback spin where SharedArrayBuffer is unavailable */ }
    }
  };
  const deadline = Date.now() + Math.max(0, waitMs);
  for (;;) {
    const lock = tryAcquire();
    if (lock) return lock;
    try {
      const age = Date.now() - fs.statSync(lockPath).mtimeMs;
      if (age > 60000) {
        fs.rmSync(lockPath, { recursive: true, force: true });
        const stolen = tryAcquire();
        if (stolen) return stolen;
      }
    } catch { /* lock directory already gone: the next round acquires it */ }
    if (Date.now() >= deadline) return null;
    sleep(50);
  }
}

function releaseMissionLock(lockPath) {
  if (lockPath) fs.rmSync(lockPath, { recursive: true, force: true });
}

function readStdin() {
  return new Promise((resolve, reject) => {
    let data = '';
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', (d) => (data += d));
    process.stdin.on('end', () => resolve(data));
    process.stdin.on('error', reject);
  });
}

async function cmdEndFeature(args) {
  // Flags first, environment as the fallback. A worker inherits the environment; a
  // subagent inherits nothing, and its only way in is the invocation `claim` printed for it.
  const { options } = parseArgs(args || []);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  const workerSessionId = options['worker-session'] || process.env.WORKER_SESSION_ID;
  const featureId = options['feature'] || process.env.FEATURE_ID;
  if (!missionDir || !workerSessionId || !featureId) {
    throw new InputError(
      'end-feature requires --mission-dir, --worker-session and --feature (or env MISSION_DIR, WORKER_SESSION_ID, FEATURE_ID)'
    );
  }
  // Read one atomic JSON payload from stdin (async read: a spawned child's stdin is a pipe, and a sync read would come back empty)
  const raw = await readStdin();
  let receipt;
  try {
    receipt = JSON.parse(raw);
  } catch (e) {
    throw new InputError(`receipt on stdin is not valid JSON: ${e.message}`);
  }
  const lock = acquireMissionLock(missionDir);
  if (!lock) {
    throw new RejectError(
      `another end-feature transaction still holds the lock after ${MISSION_LOCK_WAIT_MS / 1000}s; it is wedged, not merely busy`,
      'mission_busy'
    );
  }
  try {
    return endFeatureTransaction(missionDir, workerSessionId, featureId, receipt);
  } finally {
    releaseMissionLock(lock);
  }
}

/** End-to-end transaction under the lock: ownership (session + feature) -> single-shot -> hard checks -> persistence (FR-010) */
/**
 * How many times one worker session may resubmit after a shape rejection.
 *
 * Bounded because a worker that cannot produce a well-formed receipt should stop rather than
 * loop; the worker timeout is a backstop, not a budget.
 */
const MAX_RECEIPT_RESUBMITS = 3;

/** Shape rejections already recorded for this worker session (the log is the count's authority). */
function countReceiptRejections(missionDir, workerSessionId) {
  let n = 0;
  for (const e of readEvents(missionDir)) {
    if (e.event === 'receipt_rejected' && e.workerSessionId === workerSessionId && e.class === 'shape') n += 1;
  }
  return n;
}

/**
 * The machine-readable half of a rejection, on stdout; the human sentence stays on stderr.
 *
 * A worker cannot act on English. `resubmit` says whether correcting and sending again is the
 * right move, which separates "you typed the field name wrong" from "your work did not pass".
 */
function emitRejection(payload) {
  process.stdout.write(JSON.stringify(Object.assign({ rejected: true }, payload), null, 2) + '\n');
}

function endFeatureTransaction(missionDir, workerSessionId, featureId, receipt) {
  const state = readState(missionDir);
  // Ownership: must be the currently active worker
  // Single-shot first: a session that already submitted is no longer active, and "you already
  // submitted" is the useful answer -- "you are not an active worker" sends the reader looking for
  // a scheduling problem that is not there. This ordering started mattering when the lock began
  // waiting instead of refusing: the loser of a race now arrives *after* the winner released it.
  const alreadySubmitted = readEvents(missionDir)
    .some((e) => e.event === 'worker_completed' && e.workerSessionId === workerSessionId);
  if (alreadySubmitted) {
    throw new RejectError(`workerSessionId ${workerSessionId} already submitted a receipt; duplicate rejected`, 'duplicate_receipt');
  }
  if (!activeWorkerIds(state).includes(workerSessionId)) {
    // Answered in the same envelope as every other rejection, and told what to do. A worker that
    // hits this has usually been handed a session id that is not the one `claim` issued -- an
    // orchestrator retyped it, or a brief was assembled by hand instead of passed through. That
    // happened during this system's own acceptance run. The plain-text throw left the worker with a
    // failure outside the three documented classes and no stated next step, so it guessed; the one
    // that guessed well found the real id in state.json and resubmitted, which is the right move
    // and should not have required inference.
    emitRejection({
      class: 'ownership', errors: ['ownership_rejected'], resubmit: false, code: 'ownership_rejected',
      active: activeWorkerIds(state),
      hint: 'this session is not the active worker for any feature. Do not retype the id: the one `claim` '
        + 'issued is in your brief, and the mission records the live sessions in state.json under activeWorkers. '
        + 'Report the mismatch to the orchestrator rather than resubmitting under a different id.',
    });
    throw new RejectError(
      `workerSessionId ${workerSessionId} is not an active worker (active=${activeWorkerIds(state).join(', ') || 'none'}); ownership rejected`,
      'ownership_rejected'
    );
  }
  // Ownership: env FEATURE_ID must match this feature's currentWorkerSessionId (stops cross-feature impersonation)
  const featuresDoc = readMissionFeatures(missionDir, state);
  const feature = (featuresDoc.features || []).find((f) => f.id === featureId);
  if (!feature || feature.currentWorkerSessionId !== workerSessionId) {
    throw new RejectError(
      `FEATURE_ID ${featureId} does not match current worker session (expected ${feature ? feature.currentWorkerSessionId : 'unknown feature'}); ownership rejected`,
      'ownership_rejected'
    );
  }
  // Single-shot: this session already submitted
  const p = path.join(missionDir, 'progress_log.jsonl');
  if (fs.existsSync(p)) {
    const lines = fs.readFileSync(p, 'utf8').split('\n').filter(Boolean);
    for (let i = lines.length - 1; i >= 0; i--) {
      try {
        const ev = JSON.parse(lines[i]);
        if (ev.event === 'worker_completed' && ev.workerSessionId === workerSessionId) {
          throw new RejectError(`workerSessionId ${workerSessionId} already submitted a receipt; duplicate rejected`, 'duplicate_receipt');
        }
      } catch (e) {
        if (e instanceof RejectError) throw e;
        /* skip malformed line */
      }
    }
  }
  // Hard checks (same logic as receipt-check: input exit 2 / semantic exit 1)
  const reportOnly = featureIsReportOnly(missionDir, featureId, state);
  const acceptance = featureValidatorKind(missionDir, featureId, state) === 'acceptance';
  const { input, semantic } = receiptErrors(receipt, { reportOnly, acceptance });
  if (input.length) {
    // A shape error is a typo, not a defect. Nothing has been persisted at this point, so the
    // worker may correct the receipt and submit again within the same session -- which was always
    // physically possible and never declared, bounded, or recorded. Losing a whole worker attempt
    // and stopping the run because a field name was wrong priced a typo like failed work.
    const prior = countReceiptRejections(missionDir, workerSessionId);
    const exhausted = prior >= MAX_RECEIPT_RESUBMITS;
    const kinds = input.map((e) => e.kind);
    logEvent(missionDir, 'receipt_rejected', { workerSessionId, featureId, class: 'shape', errors: kinds, attempt: prior + 1 });
    emitRejection({
      class: 'shape',
      errors: kinds,
      resubmit: !exhausted,
      attemptsRemaining: Math.max(0, MAX_RECEIPT_RESUBMITS - prior - 1),
      code: exhausted ? 'receipt_resubmit_exhausted' : 'receipt_shape_invalid',
    });
    throw new InputError(`receipt missing/malformed fields:\n` + kinds.map((k) => `  ${k}`).join('\n'));
  }
  if (semantic.length) {
    // Not a typo: a contradictory claim about the work. Offering a retry here would invite the
    // worker to restate the claim instead of fixing the code.
    const kinds = semantic.map((e) => e.kind);
    logEvent(missionDir, 'receipt_rejected', { workerSessionId, featureId, class: 'semantic', errors: kinds });
    emitRejection({ class: 'semantic', errors: kinds, resubmit: false, code: 'receipt_semantic_failed' });
    throw new RejectError(
      `receipt semantic validation failed:\n` + kinds.map((k) => `  ${k}`).join('\n'),
      'receipt_semantic_failed'
    );
  }
  if (receipt.successState === 'success' && !commitExempt(receipt, reportOnly)) {
    const outcome = verifyReceiptGit(receipt, state, missionDir, workerSessionId, featureFromSnapshot(missionDir, featureId));
    if (!outcome.ok) {
      logEvent(missionDir, 'receipt_rejected', { workerSessionId, featureId, class: 'git', errors: [outcome.err.code] });
      emitRejection({ class: 'git', errors: [outcome.err.code], resubmit: false, code: outcome.err.code });
      throw outcome.err;
    }
  }
  // Persistence: handoff file + worker_completed event + releasing this worker's slot
  fs.mkdirSync(path.join(missionDir, 'handoff'), { recursive: true });
  fs.writeFileSync(path.join(missionDir, 'handoff', `${workerSessionId}.json`), JSON.stringify(receipt, null, 2) + '\n');
  logEvent(missionDir, 'worker_completed', {
    workerSessionId,
    featureId,
    successState: receipt.successState,
    failureClass: receipt.failureClass,
    commitId: receipt.commitId,
  });
  removeActiveWorker(state, workerSessionId);
  fs.writeFileSync(statePath(missionDir), JSON.stringify(state, null, 2) + '\n');
  // Say what this did and did not do. A worker that reads "accepted" and an orchestrator that
  // sees the feature still in_progress will conclude the status has to be set by hand -- and
  // hand-editing the plan is the one thing this system exists to make unnecessary.
  const settleHint = receipt.successState === 'success'
    ? `the receipt is recorded; the feature is NOT complete yet.\n` +
      `  the orchestrator settles it with:  devflow.cjs next-feature --mission-dir ${missionDir}\n` +
      `  which runs the plan's checks, then reviews and merges the branch. Never set a status by hand.\n`
    : `the receipt is recorded; the orchestrator decides what happens next at its next round.\n`;
  process.stdout.write(`end-feature accepted (${receipt.successState})\n  ${settleHint}`);
}

/**
 * Whether this receipt owes no git verification: a report-only feature that claimed no commit.
 * Every receipt-consuming path (receipt-check, end-feature, the scheduler's re-validation)
 * must ask the same question -- a path that forgets discards valid receipts as forgeries.
 */
function commitExempt(receipt, reportOnly) {
  return !!reportOnly && !receipt.commitId;
}

// ---------------------------------------------------------------------------
// Active workers.
//
// One worker at a time was a consequence of one worktree, not a design goal. With a worktree per
// feature, several can be in flight -- so ownership became a set. `activeWorkerSessionId` is still
// read from older mission state so a mission proposed before this change keeps running.
// ---------------------------------------------------------------------------

/** The plan's entry for a feature id, read from the mission snapshot. Null when unknown. */
function featureFromSnapshot(missionDir, featureId) {
  try {
    const state = readState(missionDir);
    const doc = readMissionFeatures(missionDir, state);
    return (doc.features || []).find((f) => f.id === featureId) || null;
  } catch {
    return null;
  }
}

function activeWorkers(state) {
  const list = Array.isArray(state.activeWorkers) ? state.activeWorkers.slice() : [];
  // The single-value form is still authoritative when it names a worker the list does not: a
  // mission proposed before this change, or state written by hand, carries only that field.
  if (state.activeWorkerSessionId && !list.some((w) => w.workerSessionId === state.activeWorkerSessionId)) {
    list.push({ workerSessionId: state.activeWorkerSessionId, featureId: null });
  }
  return list;
}
function activeWorkerIds(state) {
  return activeWorkers(state).map((w) => w.workerSessionId);
}
function addActiveWorker(state, workerSessionId, featureId) {
  const list = activeWorkers(state).filter((w) => w.workerSessionId !== workerSessionId);
  list.push({ workerSessionId, featureId });
  state.activeWorkers = list;
  // Kept in step for anything still reading the single-value form, including an older CLI.
  state.activeWorkerSessionId = list.length === 1 ? list[0].workerSessionId : null;
}
function removeActiveWorker(state, workerSessionId) {
  const list = activeWorkers(state).filter((w) => w.workerSessionId !== workerSessionId);
  state.activeWorkers = list;
  state.activeWorkerSessionId = list.length === 1 ? list[0].workerSessionId : null;
}
function clearActiveWorkers(state) {
  state.activeWorkers = [];
  state.activeWorkerSessionId = null;
}

/**
 * git checks for a success receipt (shared by receipt-check and end-feature).
 * Returns { ok: true } or { ok: false, err: RejectError }.
 */
function verifyReceiptGit(receipt, state, missionDir, workerSessionId, feature) {
  const repoRoot = state.repoRoot;
  // A worker builds on its feature's own branch, inside its own worktree. The branch a commit must
  // belong to is therefore the feature's, not the integration branch -- the integration branch only
  // receives it later, by fast-forward, once it has been reviewed and re-verified.
  const expectedBranch = (feature && feature.branch) || state.expectedBranch;
  const baseline = lastBaseline(missionDir, workerSessionId || activeWorkerIds(state)[0]);
  if (!baseline) {
    return { ok: false, err: new RejectError('no worker_started baseline found in progress_log; cannot validate ancestry', 'no_baseline') };
  }
  const repoPathReal = realpathOrNull(receipt.repoPath);
  const top = repoPathReal ? git(['rev-parse', '--show-toplevel'], repoPathReal) : { ok: false };
  const actualRoot = top.ok ? top.stdout : null;
  // A linked worktree has its own top-level, so identity is "the same repository", not "the same
  // directory": the common git dir is what the main tree and all its worktrees share.
  const sameRepo = actualRoot && (actualRoot === repoRoot || (() => {
    const a = git(['rev-parse', '--git-common-dir'], actualRoot);
    const b = git(['rev-parse', '--git-common-dir'], repoRoot);
    return a.ok && b.ok && realpathOrNull(path.resolve(actualRoot, a.stdout)) === realpathOrNull(path.resolve(repoRoot, b.stdout));
  })());
  if (!sameRepo) {
    return {
      ok: false,
      err: new RejectError(
        `repoPath does not resolve to locked repoRoot (locked=${repoRoot}, actual=${actualRoot || 'n/a'})`,
        'REPOSITORY_MISMATCH'
      ),
    };
  }
  const cat = git(['cat-file', '-e', `${receipt.commitId}^{commit}`], actualRoot);
  if (!cat.ok) return { ok: false, err: new RejectError(`commit object does not exist: ${receipt.commitId}`, 'commit_not_found') };
  const anc = git(['merge-base', '--is-ancestor', baseline, receipt.commitId], actualRoot);
  if (!anc.ok) return { ok: false, err: new RejectError(`commitId is not a descendant of baseline ${baseline}`, 'not_baseline_descendant') };
  const contains = git(['branch', '--all', '--contains', receipt.commitId], actualRoot);
  const onBranch = contains.ok && contains.stdout.split('\n')
    .map((l) => l.replace(/^[*+]?\s*/, '').trim()).includes(expectedBranch);
  if (!onBranch) return { ok: false, err: new RejectError(`commitId not contained in expected branch '${expectedBranch}'`, 'branch_mismatch') };
  const diff = git(['diff', '--stat', `${baseline}..${receipt.commitId}`], actualRoot);
  if (!diff.ok || diff.stdout === '') return { ok: false, err: new RejectError('diff baseline..commitId is empty', 'empty_diff') };
  return { ok: true };
}

/**
 * The commit a receipt's work moved to when its branch was rewritten under it, or null.
 *
 * Narrow on purpose. It applies only when the pinned commit failed the checks *because it is no
 * longer on the branch* -- the signature of a rebase or an amend -- and it re-runs the identical
 * checks against the branch's current head rather than waiving them. A commit that never existed,
 * one that does not descend from the spawn baseline, one on a foreign repository and one with an
 * empty diff all still fail, exactly as before.
 *
 * Accepting the branch instead of the frozen sha is safe because the branch is what everything
 * downstream already judges: the reviewer reads it, and the merge gate re-runs the plan's own
 * verification against it before any fast-forward. What ships is verified either way.
 */
function rebasedReceiptHead(receipt, state, missionDir, workerSessionId, feature, err) {
  if (!err || err.code !== 'branch_mismatch') return null;
  const branch = (feature && feature.branch) || state.expectedBranch;
  const head = git(['rev-parse', '--verify', `refs/heads/${branch}`], state.repoRoot);
  if (!head.ok || head.stdout === receipt.commitId) return null;
  const recheck = verifyReceiptGit(
    Object.assign({}, receipt, { commitId: head.stdout }), state, missionDir, workerSessionId, feature
  );
  return recheck.ok ? head.stdout : null;
}

// ---------------------------------------------------------------------------
// Mission load + eligibility (shared state-machine helpers).
//
// loadMission re-derives the plan-with-runtime and re-runs the propose-time trust checks
// (planDigest, contract authority, append-only audit, skill bindings, coverage); eligible*
// answers "what can be picked up next". Every loop command (wave/claim/next-feature) uses these.
// ---------------------------------------------------------------------------
function loadMission(missionDir) {
  const state = readState(missionDir);
  const featuresDoc = readMissionFeatures(missionDir, state);
  // planDigest comparison: tampering with planning fields -> reject (FR-011); runtime-field changes do not false-positive
  const digest = planDigest(featuresDoc.features, featuresDoc, injectedFeatureIds(readEvents(missionDir)));
  if (digest !== state.planDigest) {
    throw new RejectError(
      `the plan changed since propose (${state.featuresSourcePath || 'features.json'}): planning fields were edited outside 'amend'. ` +
        `Recompiling mid-mission is a plan change — propose under a new session to adopt it, or restore the file.`,
      'features_tampered'
    );
  }
  // Contract authority re-check (edge case: validation-contract.json edited by hand mid-mission):
  // (1) the source file digest matches the one locked at propose; (2) fulfills <-> assertions exist in both directions
  if (!state.contractPath || !fs.existsSync(state.contractPath)) {
    throw new RejectError(`contract source missing: ${state.contractPath || '(not recorded)'}`, 'contract_missing');
  }
  // One read feeds both the whole-file digest and every per-assertion consumer below.
  const contractBytes = fs.readFileSync(state.contractPath);
  const curDigest = crypto.createHash('sha256').update(contractBytes).digest('hex');
  if (curDigest !== state.contractDigest) {
    throw new RejectError(
      // `amend --add-assertion` re-records the digest, so reaching here means the file was edited
      // by hand -- and the supported path was being withheld from the one message that shows up
      // exactly when someone needs it.
      `validation-contract.json changed since propose (digest mismatch): it was edited outside 'amend'. ` +
        `To add an assertion, use 'amend --add-assertion' with the feature that claims it — that re-records the digest. ` +
        `To undo a hand edit, restore the file. Only a contract whose existing assertions were rewritten needs a new session, ` +
        `because the contract is the definition of done and a moving one proves nothing.`,
      'contract_changed'
    );
  }
  // The plan source is no longer duplicated into missionDir, so `planDigest` above already IS
  // the check that the committed plan has not moved: the plan the mission runs and the plan in the
  // repository are the same file. A separate source digest would be the same fact hashed twice.
  let contractDoc;
  try {
    contractDoc = JSON.parse(contractBytes.toString('utf8'));
  } catch {
    throw new InputError(`contract ${state.contractPath}: unreadable JSON`);
  }
  const assertions = contractAssertions(state.contractPath, contractDoc);
  const errs = validateAll(featuresDoc, assertions);
  if (errs.length) {
    throw new RejectError(`features/contract recheck failed:\n${formatErrors(errs)}`, 'recheck_failed');
  }
  // Append-only, checked rather than promised. The digest above proves no APPROVED feature was
  // rewritten; this proves nothing arrived except through a path that recorded why it arrived.
  const events = readEvents(missionDir);
  const baseline = proposedBaseline(events);
  if (baseline.featureIds) {
    const attested = new Set([...baseline.featureIds, ...injectedFeatureIds(events), ...amendedFeatureIds(events)]);
    const smuggled = featuresDoc.features.filter((f) => !attested.has(f.id)).map((f) => f.id);
    if (smuggled.length) {
      throw new RejectError(
        `feature(s) appeared in the plan with no event attesting how: ${smuggled.join(', ')}; append through 'amend'`,
        'unattested_features'
      );
    }
  }
  // The contract may only GAIN assertions, and only through `amend --add-assertion`. Without this
  // the digest check degrades from a tamper detector into a change detector: re-digesting after an
  // edit would make "an assertion's behaviour was rewritten" indistinguishable from "one was added".
  if (baseline.assertionHashes) {
    const added = new Set();
    for (const e of events) {
      if (e.event === 'contract_amended') for (const id of e.added || []) added.add(id);
    }
    const now = contractAssertionHashes(state.contractPath, contractDoc);
    const changed = Object.keys(baseline.assertionHashes)
      .filter((id) => now[id] !== baseline.assertionHashes[id]);
    if (changed.length) {
      throw new RejectError(
        `assertion(s) changed since propose: ${changed.join(', ')}; the contract may only gain assertions`,
        'assertion_rewritten'
      );
    }
    const unattested = Object.keys(now).filter((id) => !(id in baseline.assertionHashes) && !added.has(id));
    if (unattested.length) {
      throw new RejectError(
        `assertion(s) appeared with no event attesting how: ${unattested.join(', ')}; add through 'amend --add-assertion'`,
        'unattested_assertions'
      );
    }
  }
  // Bound skill artifacts are re-verified here, before any feature is picked up: an approved
  // plan must not execute against an artifact that was edited after it was approved.
  verifySkillBindings(state, state.repoRoot);
  // Coverage gate: each assertion claimed by exactly one feature (both directions)
  const claims = new Map();
  for (const f of featuresDoc.features) {
    for (const v of f.fulfills || []) {
      claims.set(v, (claims.get(v) || 0) + 1);
    }
  }
  const coverage = [...assertions].filter((a) => !claims.has(a) || claims.get(a) !== 1);
  if (coverage.length) {
    throw new RejectError(
      `coverage gate failed: assertions not claimed exactly once: ${coverage.join(', ')}`,
      'coverage_failed'
    );
  }
  return { state, featuresDoc };
}

function eligibleFeature(features) {
  const byId = new Map(features.map((f) => [f.id, f]));
  for (const f of features) {
    if (f.status !== 'pending') continue;
    const depsOk = (f.dependsOn || []).every((d) => byId.has(d) && byId.get(d).status === 'completed');
    if (depsOk) return f;
  }
  return null;
}

/**
 * Every feature that could be picked up right now, in plan order.
 *
 * The model-driven loop is handed the whole set because a wave is what P3 will schedule in
 * parallel. Returning the set now keeps that door open without
 * anything downstream having to change its mind about what "eligible" means.
 */
function eligibleFeatures(features) {
  const byId = new Map(features.map((f) => [f.id, f]));
  return features.filter(
    (f) => f.status === 'pending' &&
      (f.dependsOn || []).every((d) => byId.has(d) && byId.get(d).status === 'completed')
  );
}

// ---------------------------------------------------------------------------
// The state machine, extracted (D1).
//
// One set of transitions, one driver: the model-driven next-feature/claim/release loop.
// The blocking-process `run` executor is gone; these functions exist so every loop command shares
// one state machine. `tests/devflow/dual-path-parity.test.cjs` keeps the loop's two invocation
// shapes (env mode, brief mode) landing in the same place.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The plan and the runtime, kept apart.
//
// They used to be one file inside missionDir: a full copy of the plan carrying the statuses on
// top. That put the plan in a git-ignored directory, which had two consequences. Recompiling the
// committed plan mid-mission diverged the two with nothing to notice (now `features_source_changed`),
// and -- worse -- a feature appended by `amend` existed ONLY in the ignored copy, so the record of
// what was added and why could not be committed and `git clean -fdx` took it with everything else.
//
// Now: the plan is the committed file the mission was proposed with, and missionDir holds only
// what must not be in git -- per-feature runtime state, plus the validator features the scheduler
// injects at runtime (which were never part of the plan and are already digest-exempt).
// ---------------------------------------------------------------------------

const RUNTIME_FIELDS = ['status', 'workerSessionIds', 'currentWorkerSessionId', 'branch', 'worktree', 'prId', 'issueId'];

function runtimePath(missionDir) {
  return path.join(missionDir, 'runtime.json');
}

/** The plan the mission was approved with, from the committed source. */
function readPlanSource(state) {
  const p = state.featuresSourcePath;
  if (!p || !fs.existsSync(p)) {
    throw new RejectError(`plan source missing: ${p || '(not recorded)'}`, 'features_source_missing');
  }
  return JSON.parse(fs.readFileSync(p, 'utf8'));
}

/**
 * The plan with its runtime state applied — the shape everything downstream already expects.
 *
 * A mission proposed before the split still has its snapshot and no runtime.json; it is read from
 * the snapshot so a run in flight is not broken by this change, and writes migrate it forward.
 */
function readMissionFeatures(missionDir, state) {
  // A mission is pre-split exactly when its state does not name a plan source -- not when
  // runtime.json happens to be missing. The worker owns missionDir and can delete that file, and
  // "delete one file to choose which plan is read" is not a decision anyone should be able to make.
  const legacy = path.join(missionDir, 'features.json');
  if (!state.featuresSourcePath && fs.existsSync(legacy)) {
    return JSON.parse(fs.readFileSync(legacy, 'utf8'));
  }
  const doc = readPlanSource(state);
  let runtime = { features: {}, injected: [] };
  if (fs.existsSync(runtimePath(missionDir))) {
    try {
      runtime = Object.assign(runtime, JSON.parse(fs.readFileSync(runtimePath(missionDir), 'utf8')));
    } catch { /* unreadable runtime: treat as a fresh mission rather than refusing to start */ }
  }
  // Injected validators go where they went before: the front, so they run ahead of what is queued.
  doc.features = [...(runtime.injected || []), ...doc.features];
  for (const f of doc.features) {
    const rt = (runtime.features || {})[f.id] || {};
    f.status = rt.status || f.status || 'pending';
    f.workerSessionIds = rt.workerSessionIds || f.workerSessionIds || [];
    f.currentWorkerSessionId = rt.currentWorkerSessionId !== undefined ? rt.currentWorkerSessionId : (f.currentWorkerSessionId || null);
    f.branch = rt.branch !== undefined ? rt.branch : (f.branch || null);
    f.worktree = rt.worktree !== undefined ? rt.worktree : (f.worktree || null);
    // Forge adapter state (t3): a --forge mission's PR and Issue projections live in
    // the runtime, keyed by the feature. Absent stays null; they are never derived.
    f.prId = rt.prId !== undefined ? rt.prId : (f.prId || null);
    f.issueId = rt.issueId !== undefined ? rt.issueId : (f.issueId || null);
  }
  return doc;
}

/** Persist only what is runtime. The plan is not ours to write; it is committed. */
function writeMissionRuntime(missionDir, featuresDoc, injectedIds, knownState) {
  // A mission proposed before the split has no plan outside missionDir to write runtime against.
  // It stays on the old scheme for its whole life: reading it one way and writing it another is
  // what a half-migration is, and it bricked such a mission on its first state change -- the
  // snapshot was deleted and nothing was left to read the plan from.
  //
  // `knownState` is for propose, which writes the runtime before state.json exists; without it the
  // very first write mistakes a brand-new mission for a pre-split one.
  const st = knownState || (() => {
    try {
      return readState(missionDir);
    } catch {
      return {};
    }
  })();
  if (!st.featuresSourcePath) {
    fs.writeFileSync(path.join(missionDir, 'features.json'), JSON.stringify(featuresDoc, null, 2) + '\n');
    return;
  }
  const injected = injectedIds instanceof Set ? injectedIds : new Set(injectedIds || []);
  const out = { features: {}, injected: [] };
  for (const f of featuresDoc.features) {
    const rt = {};
    for (const k of RUNTIME_FIELDS) if (f[k] !== undefined) rt[k] = f[k];
    out.features[f.id] = rt;
    if (injected.has(f.id)) {
      const record = {};
      for (const k of Object.keys(f)) if (!RUNTIME_FIELDS.includes(k)) record[k] = f[k];
      out.injected.push(record);
    }
  }
  fs.writeFileSync(runtimePath(missionDir), JSON.stringify(out, null, 2) + '\n');
  // The snapshot is gone; leaving a stale one behind would be a second answer to "what is the plan".
  const legacy = path.join(missionDir, 'features.json');
  if (fs.existsSync(legacy)) fs.rmSync(legacy);
}

const writeState = (missionDir, state) =>
  fs.writeFileSync(statePath(missionDir), JSON.stringify(state, null, 2) + '\n');
const writeFeaturesDoc = (missionDir, featuresDoc) =>
  writeMissionRuntime(missionDir, featuresDoc, injectedFeatureIds(readEvents(missionDir)));

/**
 * Restart recovery, run at the start of a round.
 *
 * (1) leftover in_progress: apply its receipt when one exists (crash after the event was written
 *     but before state cleanup), otherwise requeue -- the worker died with its scheduler.
 * (2) environmental pauses (branch_mismatch / dirty_worktree / gate_failed) are re-verified before
 *     every spawn anyway, so they return to pending; human-type pauses never auto-resume.
 */
/**
 * `workersAreDead` is the caller's knowledge, and only one caller has it.
 *
 * The loop's workers are subagents that outlive a tool call, and its prescribed settle step is a
 * `next-feature`/`wave` call made *while they are running* -- so requeueing an `in_progress`
 * feature at round start would destroy a live worker's session, and its later receipt is refused as
 * `ownership_rejected`. Under the default concurrency of 2 that made the documented loop impossible:
 * settling worker A necessarily killed worker B.
 *
 * The model has no way to know a subagent died, which is exactly why `release` exists and says so.
 * So the model path never guesses: an `in_progress` feature stays in flight until a receipt settles
 * it or the orchestrator reports the death.
 */
function recoverMission(missionDir, state, featuresDoc, maxAttempts, { workersAreDead = false } = {}) {
  let schedulingBlocked = false;
  let blockReason = null;
  // A worktree git refused to remove is kept along with the record of where it is -- which would be
  // a state with no exit if nothing ever tried again: the feature is already `completed`, so
  // `cancel` refuses it and `release` does not apply, while the closing checklist asks for a
  // repository holding only the main tree. Retrying is recovery, not scheduling, so it belongs here
  // rather than in one driver's loop: the usual reason for a refusal is a fix worker still holding
  // the tree, and that resolves itself. It lived in `next-feature` for exactly one round of review
  // before this was noticed -- `run` merges through the same path and would have leaked forever.
  for (const f of featuresDoc.features) {
    if (f.status === 'completed' && f.worktree) recycleFeatureWorktree(missionDir, state, f);
  }
  for (const f of featuresDoc.features) {
    if (f.status === 'in_progress') {
      const sid = f.currentWorkerSessionId;
      const completed = sid ? findCompletedReceipt(missionDir, sid, state) : null;
      if (completed) {
        const receipt = completed.receipt;
        if (receipt.successState === 'success') {
          // Delegate rather than re-decide. This branch used to write `completed` directly, which
          // was right when a receipt meant the work was already on the branch that ships -- and
          // wrong the moment completion became "the branch merged". A crash between the receipt
          // and the settle would have marked the feature done with its commits stranded.
          logEvent(missionDir, 'resume_applied_receipt', { featureId: f.id, successState: 'success', commitId: receipt.commitId });
          applyReceipt(missionDir, state, featuresDoc, f, sid, maxAttempts, 1800000);
          continue;
        }
        f.currentWorkerSessionId = null;
        if (receipt.failureClass === 'defect') {
          // Only defect is retryable; recovery is bound by the same budget (crash window: the Nth defect is already persisted)
          const defectCount = countAttempts(missionDir, f.id, state);
          if (defectCount >= maxAttempts) {
            f.status = 'failed';
            schedulingBlocked = true;
            blockReason = 'feature_retry_limit_exceeded';
            logEvent(missionDir, 'resume_applied_receipt', { featureId: f.id, successState: receipt.successState, failureClass: 'defect', exhausted: true });
            logEvent(missionDir, 'feature_retry_limit_exceeded', { featureId: f.id, attempts: defectCount });
          } else {
            f.status = 'pending';
            logEvent(missionDir, 'resume_applied_receipt', { featureId: f.id, successState: receipt.successState, failureClass: 'defect' });
          }
        } else {
          // blocked/boundary-violation/partial: human-type, recovery keeps the paused semantics
          f.status = 'paused';
          schedulingBlocked = true;
          blockReason = receipt.failureClass === 'blocked' ? 'blocked' : receipt.failureClass === 'boundary-violation' ? 'boundary_violation' : 'partial';
          logEvent(missionDir, 'resume_applied_receipt', {
            featureId: f.id,
            successState: receipt.successState,
            failureClass: receipt.failureClass || 'partial',
          });
          logEvent(missionDir, 'feature_paused', { featureId: f.id, pauseReason: blockReason });
        }
      } else if (workersAreDead) {
        f.status = 'pending';
        f.currentWorkerSessionId = null;
        logEvent(missionDir, 'resume_requeue', { featureId: f.id, reason: 'stale_in_progress' });
      }
    } else if (f.status === 'paused') {
      const reason = lastPauseReason(missionDir, f.id);
      if (reason === 'branch_mismatch' || reason === 'dirty_worktree' || reason === 'gate_failed') {
        // Environmental, and a red gate belongs here: the pre-spawn check re-verifies on the
        // next round, so fixing the code is the whole recovery procedure. What bounds the retrying
        // is the milestone repair budget, not a permanent block.
        f.status = 'pending';
        logEvent(missionDir, 'resume_environment', { featureId: f.id, reason });
      }
    }
  }
  // Same knowledge, same gate: `state.activeWorkers` is what `end-feature` checks ownership
  // against, so clearing it on a routine poll refuses the receipt of a worker that is still
  // running -- `ownership_rejected`, for a receipt that was entirely valid. Only a driver that
  // knows its workers are gone may declare the roster empty.
  if (workersAreDead && activeWorkerIds(state).length) {
    clearActiveWorkers(state);
    writeState(missionDir, state);
  }
  writeFeaturesDoc(missionDir, featuresDoc);

  // A persisted human-type paused (blocked/boundary/retry-limit) blocks scheduling of independent features even without a leftover in_progress
  if (state.state === 'paused' && state.pauseReason && !RECOVERABLE_PAUSE.includes(state.pauseReason)) {
    schedulingBlocked = true;
    blockReason = state.pauseReason;
  }
  if (schedulingBlocked) {
    state.state = 'paused';
    state.pauseReason = blockReason;
    writeState(missionDir, state);
  } else if (state.state === 'paused' && RECOVERABLE_PAUSE.includes(state.pauseReason)) {
    // Recovery just acted on this pause -- the feature went back to pending and the check that
    // failed re-runs at the next spawn. Leaving the mission-level flag set reports "paused" and a
    // list of eligible work in the same breath, identically before and after the cause is fixed,
    // so the only way to tell a wedged mission from a recovered one is to attempt a claim and read
    // its exit code. The red gate stays visible in the log and still refuses the spawn.
    state.state = 'orchestrator_turn';
    state.pauseReason = null;
    writeState(missionDir, state);
  }
  return { schedulingBlocked, blockReason };
}

/**
 * The checks that stand between an eligible feature and a running worker: right branch, clean
 * worktree, and the milestone's pre-spawn gate. All three pause the mission rather than skipping
 * the feature -- nothing behind a red gate is worth scheduling.
 */
function preSpawnChecks(missionDir, state, featuresDoc, feature, verificationTimeoutMs) {
  const repoRoot = state.repoRoot;
  const pause = (pauseReason, payload) => {
    feature.status = 'paused';
    state.state = 'paused';
    state.pauseReason = pauseReason;
    writeState(missionDir, state);
    // The reason travels in the event whatever the event is called. Recovery reads it back to
    // decide whether the pause was environmental, and keying that read on one event name left the
    // gate -- the case its own comment singles out -- permanently paused.
    logEvent(missionDir, payload.event || 'feature_paused', Object.assign({ pauseReason }, payload.body));
    writeFeaturesDoc(missionDir, featuresDoc);
    return { ok: false, pauseReason };
  };

  const headRef = git(['symbolic-ref', '--short', 'HEAD'], repoRoot);
  if (!headRef.ok || headRef.stdout !== state.expectedBranch) {
    return pause('branch_mismatch', { body: { featureId: feature.id, pauseReason: 'branch_mismatch' } });
  }
  const st = git(['status', '--porcelain'], repoRoot);
  if (!st.ok || st.stdout.length > 0) {
    // Say whose mess it is. A worker that failed without reverting looks identical to the
    // user's own uncommitted edits, and the operator otherwise goes hunting for the wrong one.
    const prior = readEvents(missionDir);
    const lastWorker = [...prior].reverse().find((e) => e.event === 'worker_completed' || e.event === 'feature_completed');
    const afterFailedReceipt = !!lastWorker && lastWorker.event === 'worker_completed' && lastWorker.successState !== 'success';
    const dirty = st.ok ? st.stdout.split('\n').filter(Boolean) : [];
    // An amend deliberately leaves the plan modified, so this pause is now a routine step rather
    // than an anomaly -- and reporting it as an unexplained dirty tree would put the message two
    // steps away from its cause, which is the failure mode this whole guide keeps warning about.
    const planRel = state.featuresSourcePath ? path.relative(repoRoot, state.featuresSourcePath) : null;
    // Not a fixed-width slice: `git()` trims its whole output, so the first porcelain line loses
    // the leading space of its status column and column offsets stop lining up.
    const pathOf = (line) => line.trim().split(/\s+/).slice(1).join(' ');
    const planDirty = !!planRel && dirty.some((l) => pathOf(l) === planRel);
    // No separate "committed" flag is needed: once the amend is committed the plan is no longer
    // dirty, so the condition clears itself.
    const uncommittedAmend = planDirty && prior.some((e) => e.event === 'amended');
    return pause('dirty_worktree', {
      body: {
        featureId: feature.id,
        pauseReason: 'dirty_worktree',
        afterFailedReceipt,
        uncommittedAmend,
        planPath: planDirty ? planRel : undefined,
        dirtyPaths: dirty.slice(0, 10).join('; ').slice(0, 500),
      },
    });
  }
  const pre = runVerificationSteps(feature, featureWorkdir(state, feature), verificationTimeoutMs, 'pre-spawn');
  for (const res of pre.results) {
    logEvent(missionDir, 'verification_step', { featureId: feature.id, phase: 'pre-spawn', ...res });
  }
  if (!pre.ok) {
    const failed = pre.results[pre.results.length - 1];
    return pause('gate_failed', {
      event: 'verification_failed',
      body: {
        featureId: feature.id, phase: 'pre-spawn', stepId: failed.id,
        exitCode: failed.exitCode, expectedExit: failed.expectedExit, timedOut: failed.timedOut,
      },
    });
  }
  return { ok: true };
}

/**
 * Where a feature's worktree lives.
 *
 * Outside the repository on purpose: nothing a worker does can dirty the tree the scheduler checks
 * before every spawn, and `git clean -fdx` inside the repository -- the one flag that removes
 * ignored files, and the documented way a running mission gets destroyed -- cannot reach it.
 */
/**
 * One directory for every mission on this machine: `~/.loop-worktrees/<repo>-<hash>/<session>`.
 *
 * The requirement is only that it be OUTSIDE the repository -- the scheduler checks the integration
 * tree is clean before every spawn, so a worker writing inside it would self-lock the pipeline, and
 * `git clean -fdx` in the repository would delete work in flight. The old answer, the repository's
 * own parent, met that and nothing else: it colonised whatever directory happened to hold the
 * project (a workspaces folder, Downloads), scattered one concept across as many roots as the user
 * has project parents, and left nothing that could be inventoried or cleaned as a whole.
 *
 * Not a cache directory, deliberately: a cache is by definition disposable and gets cleaned by
 * things that do not ask, and what lives here is unmerged work.
 *
 * The hash is over the repository's real path, because two checkouts can share a basename and
 * creating a worktree removes a path it finds occupied -- without it, one mission would delete
 * another's work in flight. The basename stays in front so the directory is still readable.
 */
function defaultWorktreeRoot(repoRoot, sessionId) {
  const real = realpathOrNull(repoRoot) || path.resolve(repoRoot);
  const hash = crypto.createHash('sha256').update(real).digest('hex').slice(0, 8);
  return path.join(os.homedir(), '.loop-worktrees', `${path.basename(real)}-${hash}`, sessionId || 'mission');
}

function worktreeRootFor(state) {
  if (state.worktreeRoot) return state.worktreeRoot;
  // No recorded root means the mission was proposed before `propose` started writing one down, and
  // its worktrees are already on the old path. Deriving the new one here would point a running
  // mission at an empty directory and orphan whatever is in flight -- the exact half-migration that
  // bricked real missions once already. Old missions keep the old answer, forever.
  return path.join(path.dirname(state.repoRoot), '.loop-worktrees', path.basename(state.repoRoot), state.sessionId || 'mission');
}
function featureBranchName(state, feature) {
  return `loop/${state.sessionId || 'mission'}/${feature.id}`;
}

/**
 * Open a worktree and branch for one feature.
 *
 * A path that already exists means a previous attempt was never recycled. It is not reused: a
 * stale worktree carries the previous attempt's uncommitted state into this one, and the diff
 * reviewed would not be the diff that was written.
 */
function createFeatureWorktree(missionDir, state, feature) {
  const dir = path.join(worktreeRootFor(state), feature.id);
  const branch = featureBranchName(state, feature);
  if (fs.existsSync(dir)) {
    logEvent(missionDir, 'worktree_stale_removed', { featureId: feature.id, worktree: dir });
    git(['worktree', 'remove', '--force', dir], state.repoRoot);
    try {
      fs.rmSync(dir, { recursive: true, force: true });
    } catch { /* already gone */ }
  }
  // A branch already at this name belongs to an earlier attempt. It used to be force-deleted on
  // sight -- `git branch -D`, no question asked about what it held -- so re-claiming a feature
  // whose worker died silently destroyed everything that worker had committed. `cancel` states the
  // rule this broke, in this same file: deleting the commits a branch holds contradicts the reason
  // the branch was kept. "Stale" was a claim about worth that nothing measured.
  //
  // So measure it. A branch already contained by the integration branch holds nothing that is not
  // already safe, and goes. One that does not is preserved under a name that says what it is, and
  // the log names the commit, because a preserved branch nobody can find is not preserved.
  const head = git(['rev-parse', '--verify', `refs/heads/${branch}`], state.repoRoot);
  if (head.ok) {
    const merged = git(['merge-base', '--is-ancestor', branch, state.expectedBranch], state.repoRoot).ok;
    if (merged) {
      logEvent(missionDir, 'worktree_stale_branch', {
        featureId: feature.id, branch, head: head.stdout, discarded: true,
        reason: `already contained by ${state.expectedBranch}`,
      });
      git(['branch', '-D', branch], state.repoRoot);
    } else {
      const salvage = `salvage/${branch.replace(/^loop\//, '')}-${head.stdout.slice(0, 8)}`;
      const renamed = git(['branch', '-m', branch, salvage], state.repoRoot);
      logEvent(missionDir, 'worktree_stale_branch', {
        featureId: feature.id, branch, head: head.stdout,
        discarded: false, preservedAs: renamed.ok ? salvage : null,
        error: renamed.ok ? undefined : (renamed.stderr || '').slice(0, 300),
      });
      if (!renamed.ok) {
        // Refusing beats destroying: the commits are still on the original branch, and a human can
        // decide what they are worth. Proceeding would delete them to make room.
        throw new RejectError(
          `'${branch}' holds commits not in ${state.expectedBranch} (head ${head.stdout.slice(0, 8)}) ` +
            `and could not be preserved as '${salvage}': ${renamed.stderr || renamed.stdout}`,
          'stale_branch_unsalvageable'
        );
      }
    }
  }
  fs.mkdirSync(path.dirname(dir), { recursive: true });
  const add = git(['worktree', 'add', '-b', branch, dir, state.expectedBranch], state.repoRoot);
  if (!add.ok) {
    throw new RejectError(`could not create worktree for ${feature.id}: ${add.stderr || add.stdout}`, 'worktree_failed');
  }
  feature.branch = branch;
  feature.worktree = dir;
  logEvent(missionDir, 'worktree_created', { featureId: feature.id, branch, worktree: dir });
  return { branch, worktree: dir };
}

/**
 * Remove a feature's worktree and branch. `-d` refuses an unmerged branch, which is the point.
 *
 * git's refusals are the safety here, so its answers are read rather than discarded. They used to
 * be: all three calls threw their exit code away, `worktree_recycled` was logged unconditionally,
 * and `worktree`/`branch` were nulled either way. A refused removal therefore left the worktree and
 * branch on disk while the mission erased its own record of where they were — nothing could recycle
 * them afterwards, and nothing could even name them. The loop reported the opposite of what
 * happened, which is worse than reporting nothing.
 */
function recycleFeatureWorktree(missionDir, state, feature, { force = false, keepBranch = false, allowMissingDirectoryRecovery = false } = {}) {
  if (!feature.worktree) return { outcome: 'ok', recoveredDirectory: null };
  if (allowMissingDirectoryRecovery && !fs.existsSync(feature.worktree)) {
    // The directory is already gone (e.g. manually cleaned after a worktree_recycle_partial).
    // Nothing is left to remove; drop any stale admin entry and clear the pointer. This is the
    // recovery path: ownership may transfer once the leftover directory is absent. Gated behind
    // an explicit caller option -- merge/recover paths must not silently clear a missing
    // worktree record without the partial-recovery context. The original path is captured
    // first so the caller can emit the recovery audit event with it.
    const directory = feature.worktree;
    git(['worktree', 'prune'], state.repoRoot);
    feature.worktree = null;
    return { outcome: 'ok', recoveredDirectory: directory };
  }
  // `--force` discards whatever the attempt left behind; without it git refuses a dirty worktree,
  // which is the check wanted when the work is about to be merged rather than thrown away.
  const args = ['worktree', 'remove'];
  if (force) args.push('--force');
  args.push(feature.worktree);
  const removed = git(args, state.repoRoot);
  if (!removed.ok) {
    // git can fail to delete the working files and still drop the admin entry (observed on
    // 2.48: remove_worktree unregisters before the file tree is gone). Detect which half failed:
    const stillRegistered = git(['worktree', 'list', '--porcelain'], state.repoRoot)
      .stdout.split('\n').some((l) => l.startsWith('worktree ') && l.slice(9) === feature.worktree);
    if (!stillRegistered) {
      // Partial failure: the registry no longer knows this worktree, but the directory remains
      // (it may hold content the failed delete could not remove). The directory is an undeleted
      // artifact -- returning 'partial' lets the caller record a durable recovery state instead
      // of pretending the worktree is gone. feature.worktree is deliberately NOT cleared: it is
      // the recovery record naming the leftover directory.
      logEvent(missionDir, 'worktree_recycle_partial', {
        featureId: feature.id, branch: feature.branch, directory: feature.worktree,
        error: (removed.stderr || removed.stdout || '').slice(0, 300),
        hint: 'orphan directory remains; remove it manually, then retry',
      });
      return { outcome: 'partial', recoveredDirectory: null };
    }
    // Full refusal: the worktree is still registered and on disk. Returns 'refused' so callers
    // that must not mutate state (cancel) can refuse instead of recording a lifecycle that did
    // not happen.
    logEvent(missionDir, 'worktree_recycle_refused', {
      featureId: feature.id, branch: feature.branch, worktree: feature.worktree,
      error: (removed.stderr || removed.stdout || '').slice(0, 300),
    });
    return { outcome: 'refused', recoveredDirectory: null };
  }
  git(['worktree', 'prune'], state.repoRoot);
  // The branch is a separate answer. `-d` refuses an unmerged branch, and that refusal means the
  // branch still holds commits that exist nowhere else: keep it, keep the record of it, say so.
  let branchKept = keepBranch === true;
  if (feature.branch && !keepBranch) {
    const deleted = git(['branch', force ? '-D' : '-d', feature.branch], state.repoRoot);
    if (!deleted.ok) {
      branchKept = true;
      logEvent(missionDir, 'branch_delete_refused', {
        featureId: feature.id, branch: feature.branch,
        head: git(['rev-parse', feature.branch], state.repoRoot).stdout || null,
        error: (deleted.stderr || '').slice(0, 300),
      });
    }
  }
  logEvent(missionDir, 'worktree_recycled', {
    featureId: feature.id, branch: feature.branch, worktree: feature.worktree, branchKept,
  });
  feature.worktree = null;
  if (!branchKept) feature.branch = null;
  return { outcome: 'ok', recoveredDirectory: null };
}

/**
 * Do two features write to overlapping paths?
 *
 * Compared on the static prefix of each glob, because that is the part that cannot be wrong: a
 * declaration of `src/auth/**` and one of `src/auth/session.ts` overlap whatever the wildcards do.
 * An empty declaration means "unknown", which intersects everything -- ambiguity resolves toward
 * serialization, because a false serialization costs wall-clock and a false parallelization costs
 * a conflict nobody can attribute.
 */
function staticPrefix(glob) {
  const cut = glob.search(/[*?[]/);
  const head = cut === -1 ? glob : glob.slice(0, cut);
  return head.replace(/\/+$/, '');
}
function touchesIntersect(a, b) {
  const A = Array.isArray(a) ? a.filter(Boolean) : [];
  const B = Array.isArray(b) ? b.filter(Boolean) : [];
  if (!A.length || !B.length) return true; // unknown writes conflict with everything
  for (const x of A) {
    for (const y of B) {
      const px = staticPrefix(x);
      const py = staticPrefix(y);
      if (px === '' || py === '') return true; // a repo-wide glob
      if (px === py || px.startsWith(`${py}/`) || py.startsWith(`${px}/`)) return true;
    }
  }
  return false;
}

/**
 * Why this feature may or may not start alongside what is already running.
 * Returns null when it may; otherwise a RejectError-shaped reason.
 */
function concurrencyBlock(state, featuresDoc, feature) {
  const inFlight = featuresDoc.features.filter(
    (f) => f.id !== feature.id && (f.status === 'in_progress' || f.status === 'in_review')
  );
  const running = inFlight.filter((f) => f.status === 'in_progress');
  const cap = typeof state.maxConcurrency === 'number' ? state.maxConcurrency : 1;
  if (running.length >= cap) {
    return {
      code: 'concurrency_cap',
      message: `${running.length} worker(s) already running and the mission's cap is ${cap} (${running.map((f) => f.id).join(', ')})`,
    };
  }
  // A feature awaiting merge still owns its paths: its branch has not landed, so a concurrent
  // writer to the same paths is exactly the conflict this admission rule exists to prevent.
  //
  // Two report-only features are the exception, and only against each other. An empty `touches` is
  // read as "unknown, so conflicts with everything" -- the right default for a plan-authored
  // feature, where silence really is missing information. A report-only feature commits nothing;
  // that is why it gets no worktree. Its write-set is not unknown, it is empty, and devflow is the
  // party that knows it, having injected the feature itself. A milestone's two validators are meant
  // to run at the same boundary and are the mission's whole proof step, so serializing them costs a
  // round for nothing.
  //
  // Not extended to report-only against implementation: a validator reads the repository while an
  // implementation feature could merge into it, and that hazard is real.
  const bothReportOnly = (a, b) => a.reportOnly === true && b.reportOnly === true;
  const clash = inFlight.find((f) => !bothReportOnly(f, feature) && touchesIntersect(f.touches, feature.touches));
  if (clash) {
    return {
      code: 'touches_overlap',
      message: `'${feature.id}' writes paths that '${clash.id}' (${clash.status}) is still holding` +
        `${(feature.touches || []).length && (clash.touches || []).length ? '' : ' — an empty `touches` means "unknown", which conflicts with everything'}`,
    };
  }
  return null;
}

/** Where a feature's work happens: its worktree once it has one, the repository otherwise. */
function featureWorkdir(state, feature) {
  return (feature && feature.worktree) || state.workingDirectory;
}

/**
 * Allocate the worker session and mark the feature in_progress. The session exists on disk before
 * any worker does, so a crash between the two is a leftover in_progress rather than lost work.
 */
function startWorkerSession(missionDir, state, featuresDoc, feature) {
  const workerSessionId = `w-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  const spawnId = `sp-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  // A report-only feature commits nothing, so a worktree, a branch and a diff check would all be
  // theatre. Everything that builds gets one -- serial work included, because the exception is
  // where "just this once, straight onto the integration branch" lives.
  // The admission decision, recorded before anything runs. A concurrency decision nobody can
  // reconstruct is a concurrency decision nobody can debug -- and when a conflict later appears
  // between two features admitted as disjoint, this is what says which `touches` was wrong.
  logEvent(missionDir, 'wave_admitted', {
    featureId: feature.id,
    touches: feature.touches || [],
    concurrent: featuresDoc.features.filter((f) => f.id !== feature.id && f.status === 'in_progress').map((f) => f.id),
    cap: typeof state.maxConcurrency === 'number' ? state.maxConcurrency : 1,
  });
  if (!feature.reportOnly) createFeatureWorktree(missionDir, state, feature);
  const baselineCommit = git(['rev-parse', 'HEAD'], state.repoRoot).stdout;
  const attempts = countAttempts(missionDir, feature.id, state);
  feature.status = 'in_progress';
  feature.currentWorkerSessionId = workerSessionId;
  feature.workerSessionIds = [...(feature.workerSessionIds || []), workerSessionId]; // audit mapping: session history
  addActiveWorker(state, workerSessionId, feature.id);
  state.state = 'orchestrator_turn';
  writeState(missionDir, state);
  logEvent(missionDir, 'worker_started', {
    workerSessionId,
    spawnId,
    featureId: feature.id,
    baselineCommit,
    attempt: attempts + 1,
  });
  writeFeaturesDoc(missionDir, featuresDoc);
  return { workerSessionId, spawnId, baselineCommit, attempts };
}

/** The environment injected into a worker process. Named here so `claim` can hand out the same facts. */
function workerEnvFor(missionDir, state, feature, workerSessionId) {
  return {
    MISSION_DIR: missionDir,
    WORKING_DIRECTORY: featureWorkdir(state, feature),
    FEATURE_ID: feature.id,
    FEATURE_JSON: JSON.stringify(feature),
    WORKER_SESSION_ID: workerSessionId,
    DEVFLOW_CLI: path.resolve(__dirname, 'devflow.cjs'),
  };
}

/**
 * Settle a feature against the receipt its worker submitted.
 *
 * Returns `{ settled: false }` when there is no receipt -- the caller decides what that means
 * (a dead process, an unreleased claim).
 */
/**
 * `attempt` is counted here, not handed in. The model loop reads it at settle, after the receipt is
 * on disk -- so counting at this point is the one measurement that is the same either way, and a
 * mission driven by the documented loop gets the full budget rather than `maxAttempts - 1` real
 * tries while its log claims the whole budget had been spent.
 */
function applyReceipt(missionDir, state, featuresDoc, feature, workerSessionId, maxAttempts, verificationTimeoutMs) {
  const completed = findCompletedReceipt(missionDir, workerSessionId, state);
  if (!completed) return { settled: false };
  const receipt = completed.receipt;
  // A feature that built on its own branch has its plan verification run once, at the merge gate:
  // that is where it runs against what would actually ship, after review and after any rebase.
  // Running it here as well would double every check and report each one twice.
  const deferToMerge = !feature.reportOnly && !!feature.branch;
  const verification = receipt.successState === 'success' && !deferToMerge
    ? runVerificationSteps(feature, featureWorkdir(state, feature), verificationTimeoutMs, 'post-receipt')
    : { ok: true, results: [] };
  for (const res of verification.results) {
    logEvent(missionDir, 'verification_step', { featureId: feature.id, phase: 'post-receipt', ...res });
  }
  if (receipt.successState === 'success' && !verification.ok) {
    const failed = verification.results[verification.results.length - 1];
    logEvent(missionDir, 'verification_failed', {
      featureId: feature.id, workerSessionId, phase: 'post-receipt', stepId: failed.id,
      exitCode: failed.exitCode, expectedExit: failed.expectedExit, timedOut: failed.timedOut,
    });
  }
  if (receipt.successState === 'success' && verification.ok) {
    feature.currentWorkerSessionId = null;
    removeActiveWorker(state, workerSessionId);
    // A report-only feature has no branch to merge; its work IS its report, so it completes here.
    if (feature.reportOnly || !feature.branch) {
      // A report-only REVIEW feature writes `reviews/<id>.json` (plain, per reportDestinationBlock).
      // settleReviews only consumes `-r<n>` files for `in_review` features, so without recording it
      // here the report was dropped and a blocking finding never reached lastReviewVerdict. An
      // assertions validator's report lives at `reports/` (recorded via record-verdicts, yourMove),
      // so the presence of the plain reviews path is the review-kind signal.
      const reviewReport = path.join(missionDir, 'reviews', `${feature.id}.json`);
      if (feature.status !== 'completed' &&
          fs.existsSync(reviewReport) && reviewEvents(missionDir, feature.id).length === 0) {
        try {
          recordReviewCore(missionDir, state, featuresDoc, feature.id, reviewReport,
            reviewEvents(missionDir, feature.id).length + 1);
        } catch (e) {
          logEvent(missionDir, 'review_record_refused', {
            featureId: feature.id, round: 1, reportPath: reviewReport,
            error: String(e.message || e).slice(0, 300),
          });
        }
      }
      feature.status = 'completed';
      writeState(missionDir, state);
      logEvent(missionDir, 'feature_completed', { featureId: feature.id, commitId: receipt.commitId });
      recordMilestoneCompletions(missionDir, featuresDoc.features);
      injectMilestoneValidators(missionDir, featuresDoc, state);
      sealCompletedMilestones(missionDir, featuresDoc.features, state);
      writeFeaturesDoc(missionDir, featuresDoc);
      // Proof tickets close via the adapter, never the validator: one comment
      // (evidence pointer + status) + close (clause 2). Only validator features
      // are proof tickets; a report-only review closes through review flow.
      // CR-07 (code review): this runs AFTER the milestone trio — a close
      // failure must not abort between `feature_completed` and the trio, which
      // would wedge the milestone (the probe skips completed features, so a
      // skipped trio never gets a second chance). The issue stays open for the
      // reconcile probe / close-forge-ticket to retry; the milestone is done.
      if (state.forge === true && feature.validatorKind) {
        try {
          forgeCloseProofTicket(missionDir, state, feature, receipt);
        } catch (closeErr) {
          logEvent(missionDir, 'forge_ticket_close_failed', {
            featureId: feature.id, issueId: feature.issueId,
            error: String(closeErr.message || closeErr).slice(0, 300),
          });
        }
      }
      return { settled: true, stop: false, outcome: 'completed' };
    }
    // Everything else has built on its own branch and is not done until that branch has been
    // reviewed, re-verified and fast-forwarded. A receipt is a claim; the merge is the judgment.
    feature.status = 'in_review';
    writeState(missionDir, state);
    logEvent(missionDir, 'feature_awaiting_merge', { featureId: feature.id, commitId: receipt.commitId, branch: feature.branch });
    writeFeaturesDoc(missionDir, featuresDoc);
    return { settled: true, stop: false, outcome: 'in_review' };
  }
  // partial | failure: only defect consumes budget and retries (FR-008); blocked/boundary-violation do not consume budget.
  // A receipt that satisfied its own claim but failed the plan's verification is a defect too.
  const isDefect = receipt.failureClass === 'defect' || (receipt.successState === 'success' && !verification.ok);
  const attempt = countAttempts(missionDir, feature.id, state);
  if (isDefect) {
    if (attempt >= maxAttempts) {
      feature.status = 'failed';
      feature.currentWorkerSessionId = null;
      removeActiveWorker(state, workerSessionId);
      recycleFeatureWorktree(missionDir, state, feature, { force: true });
      state.state = 'paused';
      state.pauseReason = 'feature_retry_limit_exceeded';
      writeState(missionDir, state);
      logEvent(missionDir, 'feature_retry_limit_exceeded', { featureId: feature.id, attempts: attempt });
      if (state.forge === true) forgeAbandonTicketIssue(missionDir, state, feature, 'feature_retry_limit_exceeded');
      writeFeaturesDoc(missionDir, featuresDoc);
      return { settled: true, stop: true, outcome: 'failed' };
    }
    feature.status = 'pending';
    feature.currentWorkerSessionId = null;
    removeActiveWorker(state, workerSessionId);
    // The next attempt gets a fresh worktree; reusing this one would carry a failed attempt's
    // state into it and the diff reviewed would not be the diff that was written.
    recycleFeatureWorktree(missionDir, state, feature, { force: true });
    writeState(missionDir, state);
    logEvent(missionDir, 'feature_requeued', { featureId: feature.id, reason: 'defect_retry', attempt });
    writeFeaturesDoc(missionDir, featuresDoc);
    return { settled: true, stop: false, outcome: 'requeued' };
  }
  // partial | failure: only defect consumes budget and retries (FR-008); blocked/boundary-violation do not consume budget.
  // A receipt that satisfied its own claim but failed the plan's verification is a defect too.
  feature.status = 'paused';
  feature.currentWorkerSessionId = null;
  removeActiveWorker(state, workerSessionId);
  state.state = 'paused';
  state.pauseReason = receipt.failureClass === 'blocked' ? 'blocked' : receipt.failureClass === 'boundary-violation' ? 'boundary_violation' : 'partial';
  writeState(missionDir, state);
  logEvent(missionDir, 'feature_paused', { featureId: feature.id, pauseReason: state.pauseReason });
  writeFeaturesDoc(missionDir, featuresDoc);
  return { settled: true, stop: true, outcome: 'paused' };
}

/**
 * The merge gate: the only way a feature's work reaches the integration branch.
 *
 * Order matters and nothing skips a step. The branch is re-verified where it was built, then
 * fast-forwarded. `--ff-only` is the whole point: if it cannot fast-forward, the branch is not on
 * top of the current integration branch, which means what was verified is not what would ship.
 */
function mergeFeature(missionDir, state, featuresDoc, feature, verificationTimeoutMs, maxAttempts = 3) {
  if (!feature.branch || !feature.worktree) {
    return { ok: false, stop: true, code: 'no_branch', message: `feature '${feature.id}' has no branch to merge` };
  }
  // A review verdict is required when the mission declares reviewers. devflow cannot force this
  // on a host that has none -- the same reason validator injection is opt-in -- but a mission that
  // asked for the gate does not get to skip it.
  if (state.reviewGate === true) {
    const verdict = lastReviewVerdict(missionDir, feature.id);
    if (!verdict) {
      return { ok: false, stop: true, code: 'review_missing', message: `feature '${feature.id}' has no recorded review; nothing merges unreviewed` };
    }
    if (verdict.status !== 'pass') {
      return {
        ok: false, code: 'review_blocking',
        message: `feature '${feature.id}' has ${verdict.blocking} blocking review finding(s) from round ${verdict.round}`,
      };
    }
  }
  const verification = runVerificationSteps(feature, feature.worktree, verificationTimeoutMs, 'post-receipt');
  for (const res of verification.results) {
    logEvent(missionDir, 'verification_step', { featureId: feature.id, phase: 'post-receipt', ...res });
  }
  if (!verification.ok) {
    const failed = verification.results[verification.results.length - 1];
    const workerSessionId = (feature.workerSessionIds || []).slice(-1)[0] || null;
    logEvent(missionDir, 'verification_failed', {
      featureId: feature.id, workerSessionId, phase: 'post-receipt', stepId: failed.id,
      exitCode: failed.exitCode, expectedExit: failed.expectedExit, timedOut: failed.timedOut,
    });
    // The plan's own check contradicting the worker's claim is a defect, on the same budget as
    // any other -- the branch is discarded and the feature goes back for another attempt.
    const attempt = countAttempts(missionDir, feature.id, state) ;
    feature.currentWorkerSessionId = null;
    recycleFeatureWorktree(missionDir, state, feature, { force: true });
    let stop = true;
    if (attempt >= maxAttempts) {
      feature.status = 'failed';
      state.state = 'paused';
      state.pauseReason = 'feature_retry_limit_exceeded';
      logEvent(missionDir, 'feature_retry_limit_exceeded', { featureId: feature.id, attempts: attempt });
    } else {
      feature.status = 'pending';
      logEvent(missionDir, 'feature_requeued', { featureId: feature.id, reason: 'defect_retry', attempt });
      stop = false; // there is budget left: the next round picks it up again
    }
    writeState(missionDir, state);
    writeFeaturesDoc(missionDir, featuresDoc);
    return { ok: false, stop, code: 'verification_failed', message: `feature '${feature.id}' fails its own verification on its branch` };
  }
  // The plan is read-only to a worker, and a worktree is what made that enforceable at the gate
  // rather than two rounds later. Before worktrees a stray edit surfaced as `contract_changed` on
  // the next load, blaming the mission for something one branch did; now the branch that did it is
  // named, and it is refused before it lands. Observed with a real agent, which "helpfully"
  // updated the contract it was building against.
  const specDir = path.dirname(state.contractPath || '');
  if (specDir) {
    const rel = path.relative(state.repoRoot, specDir);
    const touchedPlan = git(['diff', '--name-only', `${state.expectedBranch}...${feature.branch}`], state.repoRoot);
    const offending = touchedPlan.ok
      ? touchedPlan.stdout.split('\n').filter((f) => f && rel && !path.relative(rel, f).startsWith('..'))
      : [];
    if (offending.length) {
      logEvent(missionDir, 'merge_rejected', { featureId: feature.id, branch: feature.branch, planPaths: offending.slice(0, 10) });
      return {
        ok: false, stop: true, code: 'plan_modified',
        message: `'${feature.branch}' modifies the plan directory, which is read-only to workers: ${offending.slice(0, 5).join(', ')}`,
      };
    }
  }
  // --forge missions hand the integration to the forge adapter here (merge-gate
  // time): PR creation, CI, ceremony approve, `pr merge --ff --recycle-worktree`.
  if (state.forge === true) {
    return forgeMergeFeature(missionDir, state, featuresDoc, feature);
  }
  const merge = git(['merge', '--ff-only', feature.branch], state.repoRoot);
  if (!merge.ok) {
    logEvent(missionDir, 'merge_rejected', { featureId: feature.id, branch: feature.branch, error: (merge.stderr || '').slice(0, 300) });
    return {
      ok: false, code: 'not_fast_forward',
      message: `'${feature.branch}' is not on top of ${state.expectedBranch}; rebase it and re-verify (never --no-ff)`,
    };
  }
  const mergeCommit = git(['rev-parse', 'HEAD'], state.repoRoot).stdout;
  const branch = feature.branch;
  recycleFeatureWorktree(missionDir, state, feature);
  feature.status = 'completed';
  feature.currentWorkerSessionId = null;
  logEvent(missionDir, 'feature_merged', { featureId: feature.id, branch, commitId: mergeCommit });
  logEvent(missionDir, 'feature_completed', { featureId: feature.id, commitId: mergeCommit });
  recordMilestoneCompletions(missionDir, featuresDoc.features);
  injectMilestoneValidators(missionDir, featuresDoc, state);
  sealCompletedMilestones(missionDir, featuresDoc.features, state);
  writeState(missionDir, state);
  writeFeaturesDoc(missionDir, featuresDoc);
  return { ok: true, commitId: mergeCommit };
}

/** The latest review verdict recorded for a feature, or null when none was ever recorded. */
/**
 * The canonical review history for a feature: every review_recorded event NOT
 * superseded by a later review_superseded event naming it, in log order.
 *
 * A supersede targets one exact event. New records carry a `reviewId` (uuid);
 * legacy records written before reviewId existed are targeted by the
 * `(featureId, round, ts)` triple. Targeting by round alone is refused as
 * ambiguous when the log holds duplicate rounds (which is exactly the audit
 * defect this repair exists for).
 */
function reviewEvents(missionDir, featureId) {
  const events = readEvents(missionDir);
  const superseded = new Set();
  for (const e of events) {
    if (e.event !== 'review_superseded') continue;
    if (e.reviewId) {
      superseded.add(e.reviewId);
    } else if (e.featureId && e.round !== undefined && (e.targetTs || e.ts)) {
      // targetTs is the schema since the audit-repair fix; the bare `ts` fallback
      // reads legacy corrections whose payload overwrote the logEvent timestamp
      // with the target's (an audited schema bug kept for append-only history).
      superseded.add(`${e.featureId}:${e.round}:${e.targetTs || e.ts}`);
    }
  }
  return events.filter((e) => {
    if (e.event !== 'review_recorded') return false;
    if (featureId !== undefined && e.featureId !== featureId) return false;
    const byId = e.reviewId ? superseded.has(e.reviewId) : false;
    const byTriple = superseded.has(`${e.featureId}:${e.round}:${e.ts}`);
    return !byId && !byTriple;
  });
}

function lastReviewVerdict(missionDir, featureId) {
  let latest = null;
  for (const e of reviewEvents(missionDir, featureId)) latest = e;
  return latest;
}

/** No receipt: the feature goes back to pending under the observed reason. This is `release`. */
function requeueWithoutReceipt(missionDir, state, featuresDoc, feature, reason) {
  const sid = feature.currentWorkerSessionId;
  feature.status = 'pending';
  feature.currentWorkerSessionId = null;
  if (sid) removeActiveWorker(state, sid); else clearActiveWorkers(state);
  writeState(missionDir, state);
  logEvent(missionDir, 'feature_requeued', { featureId: feature.id, reason });
  writeFeaturesDoc(missionDir, featuresDoc);
}

/**
 * Review findings that reached the end of the mission without a home.
 *
 * A blocking finding cannot get past the merge gate, so what survives is non-blocking: recorded,
 * counted, and then read by nothing. The closing checklist says every finding is resolved or
 * tracked with a disposition, and that was enforced nowhere -- `reconcile` only knows items some
 * feature listed in `addressesItemId`, so a finding that never became a fix feature is invisible to
 * it too. It is the loop's decision whether to fix or dismiss one; it is not the loop's decision to
 * never hear about it.
 *
 * Open means the LATEST review round for that feature still lists it -- the same rule that decides
 * whether a blocking finding persisted. An item some feature was amended to address is covered.
 */
/**
 * Branches preserved from an earlier attempt that are still on disk.
 *
 * A salvaged branch is deliberate evidence -- work a dead worker committed, kept because throwing
 * it away is the one thing a loop must never do quietly. But it is renamed out of `loop/*`, which
 * is the only place any document tells a reader to look for leftovers, and nothing else in the
 * system mentions the name. Evidence nobody can find is not evidence, so closing names it.
 */
/**
 * What workers said about the procedure they were held to.
 *
 * The receipts are on disk and nothing read them. A deviation a worker disclosed -- "the red phase
 * was impossible here, so I substituted mutation testing" -- is the only evidence that a role file
 * asks for something a real task cannot give, and it was being discarded one mission at a time.
 * Reported, never enforced: whether a procedure should change is a judgment, and a channel that
 * grades its users stops carrying anything but compliance.
 */
/**
 * How this repository builds and tests, as earlier workers in this mission found out.
 *
 * Every brief promises "report what you found … so the next worker does not repeat it", and nothing
 * kept that promise: the facts were requested through `discoveredIssues`, a channel for problems,
 * and read by nobody. A real mission paid for it twice over -- its operator wrote a driver that
 * pre-warmed each worktree because otherwise "each worker spends 15 minutes rediscovering".
 *
 * Deduplicated by the fact itself: three workers reporting "there is no package.json" is one fact.
 */
function discoveredOps(missionDir) {
  const dir = path.join(missionDir, 'handoff');
  if (!fs.existsSync(dir)) return [];
  const seen = new Map();
  for (const f of fs.readdirSync(dir).sort()) {
    if (!f.endsWith('.json')) continue;
    let receipt;
    try {
      receipt = JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8'));
    } catch { continue; }
    const found = receipt && receipt.handoff && receipt.handoff.opsFindings;
    for (const o of Array.isArray(found) ? found : []) {
      if (!o || !o.fact) continue;
      const key = String(o.fact).trim().toLowerCase();
      if (!seen.has(key)) seen.set(key, { fact: String(o.fact).trim(), command: o.command || null });
    }
  }
  return [...seen.values()];
}

function skillFeedbackReported(missionDir) {
  const dir = path.join(missionDir, 'handoff');
  if (!fs.existsSync(dir)) return [];
  const out = [];
  for (const f of fs.readdirSync(dir).sort()) {
    if (!f.endsWith('.json')) continue;
    let receipt;
    try {
      receipt = JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8'));
    } catch { continue; }
    const sf = receipt && receipt.handoff && receipt.handoff.skillFeedback;
    if (!sf || typeof sf !== 'object') continue;
    const sessionId = f.replace(/\.json$/, '');
    const ws = lastWorkerStarted(missionDir, sessionId);
    for (const d of Array.isArray(sf.deviations) ? sf.deviations : []) {
      out.push({ kind: 'deviation', featureId: ws ? ws.featureId : null, sessionId,
        step: d.step, insteadOf: d.whatIDidInstead, why: d.why });
    }
    for (const c of Array.isArray(sf.suggestedChanges) ? sf.suggestedChanges : []) {
      // Workers write these as {step, suggestion} as often as plain strings; rendering the object
      // verbatim buries the sentence a human is meant to read inside its own punctuation.
      const text = typeof c === 'string' ? c
        : [c && c.step ? `on "${c.step}"` : null, (c && (c.suggestion || c.text)) || JSON.stringify(c)]
          .filter(Boolean).join(': ');
      out.push({ kind: 'suggestion', featureId: ws ? ws.featureId : null, sessionId, text });
    }
  }
  return out;
}

function salvagedBranches(missionDir, state) {
  const out = [];
  for (const e of readEvents(missionDir)) {
    if (e.event !== 'worktree_stale_branch' || !e.preservedAs) continue;
    if (!git(['rev-parse', '--verify', `refs/heads/${e.preservedAs}`], state.repoRoot).ok) continue;
    out.push({ branch: e.preservedAs, featureId: e.featureId, head: e.head || null });
  }
  return out;
}

function openReviewFindings(missionDir, features) {
  const latest = new Map(); // featureId -> the last review_recorded for it
  for (const e of reviewEvents(missionDir)) {
    if (e.featureId) latest.set(e.featureId, e);
  }
  const covered = new Set();
  for (const f of features) {
    for (const id of f.addressesItemId || []) covered.add(id);
  }
  const open = [];
  for (const [featureId, e] of latest) {
    for (const id of e.nonBlockingIds || []) {
      if (!covered.has(id)) open.push({ itemId: id, featureId, round: e.round });
    }
  }
  return open;
}

/**
 * The end of a mission, or the reason it is not the end.
 *
 * Every feature completing is not the same as every assertion being proven, so the ledger gate
 * applies wherever the mission has a proof path at all -- validators were requested, or the plan
 * itself declares `proves`. A mission with neither is not gated: it has no way to prove anything,
 * and failing it would be a gate nobody can pass.
 */
function closingReview(missionDir, state, featuresDoc) {
  const open = featuresDoc.features.filter((f) => f.status !== 'completed' && f.status !== 'cancelled');
  const openFindings = openReviewFindings(missionDir, featuresDoc.features);
  const salvaged = salvagedBranches(missionDir, state);
  // A blocking finding blocks. The merge gate enforces that for a feature that merges, and a
  // report-only review has no merge to stop -- so its blocking verdict stopped nothing at all, and a
  // real mission closed as `completed` while carrying one. Closing is the only gate such a feature
  // passes through, and this is computed before the ledger check rather than after: an unproven
  // assertion returns early, and a mission that is blocked for two reasons should say both.
  const blockingReviews = featuresDoc.features
    .map((f) => ({ id: f.id, v: lastReviewVerdict(missionDir, f.id) }))
    .filter((x) => x.v && x.v.status !== 'pass')
    .map((x) => ({ featureId: x.id, round: x.v.round, blocking: x.v.blocking }));
  const proofPlanned = proofCoverageApplies(featuresDoc.features, false);
  if (open.length === 0 && (state.validatorsRequested === true || proofPlanned)) {
    const ledger = missionLedger(missionDir, state);
    writeLedgerProjection(missionDir, ledger);
    if (ledger.unproven.length) {
      state.state = 'paused';
      state.pauseReason = 'assertions_unproven';
      writeState(missionDir, state);
      logEvent(missionDir, 'assertions_unproven', { unproven: ledger.unproven });
      return { done: false, open, unproven: ledger.unproven, openFindings, salvaged, blockingReviews };
    }
  }
  if (open.length === 0 && blockingReviews.length) {
    return { done: false, open, unproven: [], openFindings, salvaged, blockingReviews };
  }
  if (open.length === 0) {
    state.state = 'completed';
    clearActiveWorkers(state); // end-feature already cleared the on-disk value; stops a stale in-memory value from resurrecting
    writeState(missionDir, state);
    // Reported, not blocked: fixing or dismissing a non-blocking finding is the loop's judgment,
    // and there is no pre-completion dismissal command to gate it on. The mission is over; the
    // orchestrator's closing report is what has to account for these.
    logEvent(missionDir, 'mission_completed', {
      features: featuresDoc.features.length,
      openFindings: openFindings.map((f) => f.itemId),
      salvaged: salvaged.map((b) => b.branch),
    });
    return { done: true, open: [], unproven: [], openFindings, salvaged, blockingReviews: [] };
  }
  return { done: false, open, unproven: [], openFindings, salvaged, blockingReviews };
}

// ---------------------------------------------------------------------------
// The model-driven loop: next-feature / claim / release (D1).
//
// devflow keeps the state and the receipt authority; the model keeps the judgment and does the
// spawning to a subagent. There is no blocking CLI driver (the old `run` loop, which owned the
// workers' process lifetimes, is gone) -- a subagent cannot be inside a tool call it is also
// supposed to be driving. The commands here invert the old model, and both share the transitions
// above, so neither can drift.
// ---------------------------------------------------------------------------

/** The full text of the assertions a feature is held to, keyed by claim kind. */
/**
 * What an assertion validator is on the hook for: every assertion the milestone's own implementation
 * features claimed. Derived, never stored on the feature -- storing it would be a claim, and a claim
 * is what the proof-coverage gate counts. The obligation and the claim are different facts.
 */
function milestoneAssertionIds(features, milestone) {
  const ids = new Set();
  for (const f of features || []) {
    if (f.milestone !== milestone || f.reportOnly === true) continue;
    for (const id of f.fulfills || []) ids.add(id);
  }
  return [...ids];
}

/**
 * The full text of a set of assertions, as every brief prints it. Shared rather than repeated: the
 * worker brief and the review brief are held to the same assertions, and two renderers of one fact
 * drift the first time either is touched.
 */
/**
 * Where a report-only role puts what it produces.
 *
 * `validator.md` opens by promising "Your brief names the assertion ids assigned to you …, the
 * report path, and the evidence directory"; `reviewer.md` promises the same. The rendered brief
 * named none of them, so a report-only worker had nowhere legal to write -- and the only worked
 * example writes into the repository, where a write dirties the integration tree the scheduler
 * checks before every spawn and self-locks the whole mission with `dirty_worktree`. The mission
 * directory is the one place it may write, and it is ignored by git for exactly this reason.
 */
function reportDestinationBlock(missionDir, state, feature, kind, claim) {
  // A review's findings go where `record-review` consumes them, exactly as a per-feature review's
  // do. Sending them to `reports/` instead left the milestone reviewer's findings -- including a
  // BLOCKING one, in a real mission -- in a file nothing reads: they never reached `openFindings`,
  // never reached closing, and the mission completed with the finding outstanding. Two review paths
  // producing one kind of artifact must produce it in one place.
  const reportPath = kind === 'review'
    ? path.join(missionDir, 'reviews', `${feature.id}.json`)
    : path.join(missionDir, 'reports', `${feature.id}.json`);
  const evidenceDir = path.join(missionDir, 'evidence', feature.id);
  const L = [];
  L.push('## Where your report goes');
  L.push('');
  L.push(`- **Write your report to:** \`${reportPath}\` (create the directory if it is not there)`);
  L.push(`- **Capture evidence as you go, into:** \`${evidenceDir}\` — evidence reconstructed afterwards is a description, not an observation, and a \`pass\` with no artifact is not proven`);
  L.push('- **This feature commits nothing**, so write nowhere else. A write inside the repository dirties the integration tree the scheduler checks before every spawn, and stops the whole mission.');
  L.push('');
  if (kind === 'validation' || kind === 'acceptance') {
    L.push(`Report shape and acceptance rules: \`${path.resolve(__dirname, '..', 'references', 'validation-protocol.md')}\`. Check yours before handing it over — these are the same rules the scheduler applies:`);
    L.push('');
    L.push('```bash');
    // Bound, not a placeholder. The brief closes by promising "every argument is explicit: it does
    // not depend on your environment", and a command with `<contract>` in it breaks that promise in
    // the one place the worker is told to run something.
    const ids = ((claim && claim.feature && claim.feature.assertions && claim.feature.assertions.proves) || [])
      .map((a) => a.id).join(',');
    // `--evidence-root` is not optional here, and leaving it out was a mismatch this brief created
    // itself: validation-check resolves relative artifact paths against the REPORT's directory by
    // default, while the brief sends evidence to `evidence/<feature>`. Every `pass` written with a
    // relative path would then fail as `evidence-missing-on-disk`. A real validator hit it, worked
    // around it with absolute paths, and warned the next one -- which is the warning this line
    // replaces.
    L.push(`node ${path.resolve(__dirname, 'devflow.cjs')} validation-check ${reportPath} --contract ${state.contractPath}${ids ? ` --assigned ${ids}` : ''} --evidence-root ${evidenceDir}`);
    L.push('```');
  } else {
    L.push('Findings use an `itemId` matching `F-<n>` — enforced by the recorder, not a style preference — and a `severity` of `blocking` or `non_blocking`, which is the field that decides: the verdict is derived from your findings, never from a summary line. An empty `findings` array is how a clean review says so.');
    L.push('');
    L.push('The orchestrator records it with, and your findings do not enter the mission until it does:');
    L.push('');
    L.push('```bash');
    L.push(`node ${path.resolve(__dirname, 'devflow.cjs')} record-review --mission-dir ${missionDir} --feature ${feature.id} --report ${reportPath}`);
    L.push('```');
  }
  L.push('');
  return L.join('\n');
}

function renderAssertionBlock(label, entries) {
  if (!entries.length) return '';
  return `\n## ${label}\n\n${entries.map((a) => {
    // A chain assertion walks the fact across layers; the numbered steps are what the worker
    // reproduces, so they travel as prose, not as an opaque JSON array. Without them the worker
    // sees a behavior and a surface and cannot know the contract demanded `GUI -> API -> table`.
    const chain = Array.isArray(a.chain) && a.chain.length
      ? `\n\n- Chain:\n${a.chain.map((s, i) => `  ${i + 1}. ${s}`).join('\n')}`
      : '';
    return `### ${a.id}${a.title ? ` — ${a.title}` : ''}\n${a.missing
      ? '(not found in the contract — report this rather than guessing)'
      : `${a.behavior}\n\n- Surface / tool: ${a.tool || 'unspecified'}\n- Evidence expected: ${a.evidence || 'unspecified'}${chain}`}`;
  }).join('\n\n')}\n`;
}

function assertionTextFor(state, feature, allFeatures) {
  let byId = new Map();
  try {
    const contract = readJsonFile(state.contractPath);
    for (const a of (contract && contract.assertions) || []) {
      if (a && a.id) byId.set(a.id, a);
    }
  } catch { /* loadMission already proved the contract readable; an empty map is still honest */ }
  const pick = (ids) => (ids || []).map((id) => {
    const a = byId.get(id);
    return a
      ? { id, title: a.title || '', behavior: a.behavior || '', tool: a.tool || '', evidence: a.evidence || '', chain: Array.isArray(a.chain) ? a.chain : undefined }
      : { id, title: '', behavior: '', tool: '', evidence: '', missing: true };
  });
  // Only this feature's own assertions travel with it. Handing a worker the whole contract invites
  // it to implement against the exam instead of the brief -- observed, not hypothetical.
  // An injected assertion validator claims nothing, so its list comes from its milestone.
  const proves = feature.validatorKind === 'assertions'
    ? milestoneAssertionIds(allFeatures, feature.milestone)
    : feature.proves;
  return { fulfills: pick(feature.fulfills), proves: pick(proves) };
}

/**
 * What kind of work this feature is, which decides what a worker must be told.
 *
 * Not cosmetic: an implementation worker needs the assertions it must make true and the commit
 * rules; a validator needs the assertions it must exercise and the rule that it may not fix them;
 * a scrutiny reviewer needs the completed work to review and must not write code at all. One
 * shape for all three would be wrong for at least two.
 */
function featureKind(feature) {
  // `acceptance` must be judged first: it is report-only AND carries `proves` (the whole contract),
  // so the second branch below -- reportOnly + any proves -> validation -- would otherwise swallow
  // it and route the whole-contract gate through a milestone-validator brief.
  if (feature.reportOnly === true && feature.validatorKind === 'acceptance') return 'acceptance';
  if (feature.reportOnly === true && feature.validatorKind === 'assertions') return 'validation';
  if (feature.reportOnly === true && Array.isArray(feature.proves) && feature.proves.length) return 'validation';
  if (feature.reportOnly === true) return 'review';
  return 'implementation';
}

/** featureKind → the role a spawn offer carries, so an executor can dispatch without reading
 * the brief to find out what a role's procedure actually is. Acceptance is still a validator in
 * the spawn sense (it exercises a surface and owns its environment); only its brief is its own. */
const FEATURE_KIND_ROLE = { implementation: 'worker', validation: 'validator', review: 'reviewer', acceptance: 'validator' };

/** The operational manifest, when the plan carries one (W12). Absent is normal, not an error. */
function readOpsManifest(state) {
  for (const dir of [path.dirname(state.contractPath || ''), state.workingDirectory]) {
    if (!dir) continue;
    const p = path.join(dir, 'ops.yaml');
    if (fs.existsSync(p)) {
      try {
        return { path: p, text: fs.readFileSync(p, 'utf8') };
      } catch { /* unreadable manifest is the same as none, and says so below */ }
    }
  }
  return null;
}

/**
 * The deterministic half of a worker's brief (D2): everything that follows from the plan and the
 * claim, and nothing that requires judgment. The orchestrator appends the judgment half -- why
 * this feature now, what the last round taught, what state the repository was left in.
 *
 * Layered on purpose (D6): what is the same for every worker lives in references/worker-brief.md
 * and is read, not repeated. What travels here is only what is true of THIS feature.
 */
function renderBrief(missionDir, state, feature, claim) {
  const kind = featureKind(feature);
  // Each role is held to its own procedure: what a worker must do is not what a validator must do,
  // and a reviewer that follows the worker's file will start fixing what it reviews.
  const roleFile = { implementation: 'worker.md', validation: 'validator.md', review: 'reviewer.md', acceptance: 'acceptance.md' }[kind];
  const proceduresPath = path.resolve(__dirname, '..', 'references', 'roles', roleFile);
  const ops = readOpsManifest(state);
  const L = [];
  const list = (items) => (items || []).map((x) => `- ${x}`).join('\n');
  const assertionBlock = renderAssertionBlock;

  L.push(`# Feature ${feature.id}${feature.milestone ? ` (milestone: ${feature.milestone})` : ''}`);
  L.push('');
  L.push(`Attempt ${claim.attempt}. Worker session \`${claim.workerSessionId}\`.`);
  L.push('');
  // The summary branches with the file. It used to be the worker's, pasted onto every brief, so a
  // reviewer was told it is held to TDD -- one half of a fact branched and the other half copied.
  const roleGist = {
    implementation: 'TDD, verification hygiene, the receipt, when to escalate',
    validation: 'exercising the surface rather than reading it, evidence, verdicts, what not to fix',
    review: 'what to read and in what order, what makes a finding, what is not yours to fix',
    acceptance: 'owning the environment end to end, proving the whole contract, aggregating verdicts',
  }[kind];
  L.push(`**Read \`${proceduresPath}\` first** — it is the procedure you are held to (${roleGist}). This brief carries only what is specific to this feature.`);
  L.push('');

  if (kind === 'implementation') {
    L.push('## What to build');
    L.push('');
    L.push(feature.description || '(no description — escalate rather than guess)');
    L.push('');
    L.push('### Done means');
    L.push('');
    L.push(list(feature.expectedBehavior));
    L.push(assertionBlock('Assertions this feature must make true', claim.feature.assertions.fulfills));
  } else if (kind === 'acceptance') {
    // The acceptance brief is the validation brief plus one asymmetry: a milestone validator is
    // handed an already-standing environment by the implementation work, while the whole-contract
    // gate must bring the system up itself. That is why it gets its own role file -- the procedure
    // for provisioning, re-validating only non-passed assertions, and aggregating is not the
    // milestone validator's and does not belong in validator.md. Everything else (the surface
    // exercise, the do-not-fix rule, the report destination) is the validation procedure.
    L.push('## What to prove');
    L.push('');
    L.push(feature.description || '(no description — escalate rather than guess)');
    L.push('');
    L.push('This is not a build. Exercise each assertion below through its real surface and report a');
    L.push('verdict plus at least one evidence artifact for each. **Do not fix what you are testing** —');
    L.push('a finding becomes a fix feature for someone else.');
    L.push('');
    L.push('**You own the environment** — unlike a milestone validator you bring it up and take it');
    L.push('down yourself. The procedure is acceptance.md §2; this brief does not repeat it.');
    L.push('');
    L.push('### Done means');
    L.push('');
    L.push(list(feature.expectedBehavior));
    L.push(assertionBlock('Assertions to exercise', claim.feature.assertions.proves));
  } else if (kind === 'validation') {
    L.push('## What to prove');
    L.push('');
    L.push(feature.description || '(no description — escalate rather than guess)');
    L.push('');
    L.push('This is not a build. Exercise each assertion below through its real surface and report a');
    L.push('verdict plus at least one evidence artifact for each. **Do not fix what you are testing** —');
    L.push('a finding becomes a fix feature for someone else.');
    L.push('');
    L.push('### Done means');
    L.push('');
    L.push(list(feature.expectedBehavior));
    L.push(assertionBlock('Assertions to exercise', claim.feature.assertions.proves));
  } else {
    L.push('## What to review');
    L.push('');
    L.push(feature.description || '(no description — escalate rather than guess)');
    L.push('');
    L.push('You are the reviewer, not the implementer: **do not change code, and do not fix what you find.**');
    L.push('The programmatic gate has already run and passed — the scheduler runs it before spawning you,');
    L.push('and a red one would have stopped this round. You are here for what it cannot see. Report');
    L.push('findings with file and line; each one becomes a fix feature for somebody else.');
    L.push('');
    L.push('### Done means');
    L.push('');
    L.push(list(feature.expectedBehavior));
    L.push('');
    L.push('### What to review, and where the evidence is');
    L.push('');
    L.push(`- Which features this milestone completed: \`node ${path.resolve(__dirname, 'devflow.cjs')} milestones --mission-dir ${missionDir}\``);
    L.push(`- The gate it had to pass, and the checklist your report must address: \`node ${path.resolve(__dirname, 'devflow.cjs')} scrutiny --mission-dir ${missionDir}\``);
    L.push(`- Each completed feature's receipt: \`${path.join(missionDir, 'handoff')}/<workerSessionId>.json\``);
    L.push(`- The diff each one claims: \`git -C ${state.workingDirectory} show <commitId>\` (the commitId is in its receipt)`);
    L.push('');
  }

  L.push('## Where');
  L.push('');
  if (feature.worktree) {
    L.push(`- **Work here, and only here:** \`${feature.worktree}\``);
    L.push(`- **Commit on this branch:** \`${feature.branch}\` — it is yours alone`);
    L.push(`- Integration branch: \`${state.expectedBranch}\` in \`${state.repoRoot}\` — never \`checkout\`, \`merge\` or push there; the orchestrator fast-forwards it once your branch has been reviewed`);
  } else {
    L.push(`- Repository: \`${state.workingDirectory}\` (branch \`${state.expectedBranch}\`)`);
    L.push('- This feature commits nothing, so it has no worktree and no branch of its own.');
  }
  if (state.forge === true) {
    // Clause 1: one owner. The adapter holds DEVFLOW_FORGE_CALLER; a forge command from this
    // session logs caller 'external' — an unattributed write on the forge's single-owner record.
    // Stating it here is what makes the attribution guarantee observable to the worker.
    L.push(`- **Forge mode: never run \`git forge\`.** Every forge write — issue filing, PR create, CI, review, merge, ticket close — belongs to the orchestrator's adapter; a forge command from your session lands as an unattributed external write.`);
  }
  if ((feature.touches || []).length) {
    L.push(`- You own these paths and only these: ${feature.touches.map((t) => `\`${t}\``).join(', ')} — another worker may hold the rest right now`);
  }
  if (feature.reportOnly !== true) {
    // A report-only receipt must OMIT commitId, which the receipt section says. Telling it two
    // paragraphs earlier that its commit must descend from a baseline is the same prompt arguing
    // with itself, and the worker has to guess which half is real.
    L.push(`- Your baseline commit: \`${claim.baselineCommit}\` — a success receipt's commit must descend from it`);
  }
  L.push(`- Plan and contract: \`${path.dirname(state.contractPath || missionDir)}\``);
  if (ops) {
    L.push(`- Commands and services: \`${ops.path}\` — use it rather than inventing commands`);
    L.push('');
    L.push('```yaml');
    L.push(ops.text.trim());
    L.push('```');
  } else {
    const learned = discoveredOps(missionDir);
    if (learned.length) {
      L.push('- **What earlier workers in this mission already found out — do not re-derive it:**');
      for (const o of learned) L.push(`  - ${o.fact}${o.command ? ` (\`${o.command}\`)` : ''}`);
      if (feature.reportOnly !== true) {
        L.push('  - Anything you have to discover beyond this, report in `opsFindings` so the worker after you does not pay for it again.');
      }
      // Deduplication here is exact-string, so two workers describing one fact in different words
      // both survive -- observed in a real mission, where the list reached ten entries of which
      // roughly half were restatements. Deciding that two sentences mean the same thing is a
      // judgment, and judgments belong to the orchestrator: `ops.yaml` is the authored version and
      // takes precedence over this list entirely, so the list's job is to grow until someone
      // curates it, and to say so once it is long enough to be worth curating.
      if (learned.length >= 6) {
        L.push(`  - *(${learned.length} entries, and this list only deduplicates identical wording. If it reads as repetitive, write \`ops.yaml\` beside the contract: an authored manifest replaces this list entirely.)*`);
      }
    } else if (feature.reportOnly !== true) {
      L.push('- No ops manifest in this mission, and nothing discovered yet: work out how this repository builds and tests, and report what you found in `opsFindings` — `{ "fact": "...", "command": "..." }` — so the next worker does not repeat it.');
    }
  }
  if ((feature.verification && feature.verification.steps || []).length) {
    L.push('');
    L.push('The scheduler will run these itself after your receipt — your own claim does not override them:');
    L.push('');
    L.push(list(feature.verification.steps.map((s) => `\`${s.command}\` (expected exit: ${s.expectedExit})`)));
  }
  if (feature.reportOnly === true) {
    L.push('');
    L.push(reportDestinationBlock(missionDir, state, feature, kind, claim));
  }
  L.push('');
  L.push('## Submitting your receipt');
  L.push('');
  L.push('One receipt, JSON on stdin, to exactly this command:');
  L.push('');
  L.push('```bash');
  L.push(claim.endFeatureCommand);
  L.push('```');
  L.push('');
  if (feature.reportOnly === true) {
    // This costs a whole attempt when it is missing. `commitExempt` waives the git checks only for
    // a report-only receipt that carries NO commitId; supplying one -- the natural thing to do, and
    // what a real validator did -- routes the receipt back through the ancestry and diff checks and
    // it is rejected as `empty_diff`. The brief already says this feature commits nothing; this is
    // the consequence of that sentence, and the brief was leaving the worker to find it out.
    L.push('**Omit `commitId` and `repoPath` entirely.** This feature commits nothing, and a receipt that names a commit is checked against the diff it claims — which is empty, so it is rejected.');
    L.push('');
  }
  if (kind === 'acceptance') {
    // The environment obligation is enforced at receipt time (receiptErrors): telling the validator
    // here costs one line; letting it find out from a rejected receipt costs an attempt.
    L.push('**A success receipt must carry `handoff.environment`** — `{ "healthCheck": "<the probe that proved the system was up>", "tornDown": true }`. You owned the environment; this is where that ownership becomes auditable.');
    L.push('');
  }
  L.push('Every argument is explicit: it does not depend on your environment.');
  L.push('');
  return L.join('\n');
}

function resolveMissionDir(options) {
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  if (!missionDir) throw new InputError('--mission-dir is required (or env MISSION_DIR)');
  if (!fs.existsSync(statePath(missionDir))) {
    throw new RejectError(`no mission at ${missionDir} (no state.json); propose first`, 'not_proposed');
  }
  return path.resolve(missionDir);
}

/**
 * What can be worked on right now -- and, when nothing can, why.
 *
 * Also the round boundary: it recovers a leftover session, settles any receipt submitted since the
 * last call, and (with --start-round) charges the milestone repair budget exactly once per loop
 * entry. The model loop has no invocation to key on, so it charges once per turn explicitly
 * rather than on every poll.
 */
/**
 * Record one reviewer's verdict on one feature's branch, before it merges.
 *
 * The verdict is derived from the findings, not taken from the reviewer's own summary line: a
 * report that lists a blocking finding and calls itself a pass is a contradiction, and the
 * findings are the part with evidence attached.
 *
 * `itemId` is immutable across rounds. Without that, "did the fix work?" is undecidable -- the
 * same finding coming back under a new id looks like a new finding, and a fix that changed
 * nothing looks like progress.
 */
/**
 * The mutation core of record-review: validate a review report against a feature, derive the verdict
 * from its findings (never from the summary line), supersede any same-round record, and log
 * `review_recorded`. Shared by the record-review CLI and `applyReceipt`, which records a report-only
 * feature's review when its commit-exempt receipt settles -- the report at `reviews/<id>.json` was
 * otherwise dropped because `settleReviews` only consumes `-r<n>` files for `in_review` features.
 *
 * @returns {{ featureId, round, status, blocking, persisted }}
 */
function recordReviewCore(missionDir, state, featuresDoc, featureId, reportPath, roundOpt) {
  const feature = featuresDoc.features.find((f) => f.id === String(featureId));
  if (!feature) throw new RejectError(`unknown feature '${featureId}' in this mission's plan`, 'unknown_feature');
  const report = readJsonFile(reportPath);
  const findings = Array.isArray(report.findings) ? report.findings : null;
  if (!findings) throw new InputError(`${reportPath}: missing findings[] (an empty array is how a clean review says so)`);

  const problems = [];
  const seen = new Set();
  for (const f of findings) {
    if (!f || typeof f !== 'object') { problems.push('a finding is not an object'); continue; }
    if (typeof f.itemId !== 'string' || !/^F-\d+$/.test(f.itemId)) problems.push(`itemId must look like F-001, got ${JSON.stringify(f.itemId)}`);
    else if (seen.has(f.itemId)) problems.push(`itemId ${f.itemId} appears twice in one report`);
    else seen.add(f.itemId);
    if (!['blocking', 'non_blocking'].includes(f.severity)) problems.push(`${f.itemId || '?'}: severity must be blocking or non_blocking`);
    if (typeof f.description !== 'string' || !f.description.trim()) problems.push(`${f.itemId || '?'}: a finding with no description cannot be acted on`);
  }
  if (problems.length) {
    throw new RejectError(`review report rejected:\n${problems.map((x) => `  ${x}`).join('\n')}`, 'review_invalid');
  }
  const blocking = findings.filter((f) => f.severity === 'blocking');
  const derived = blocking.length ? 'fail' : 'pass';
  if (report.status && report.status !== derived) {
    throw new RejectError(
      `report says '${report.status}' but lists ${blocking.length} blocking finding(s); the findings decide`,
      'review_contradicts_findings'
    );
  }
  const canonicalBefore = reviewEvents(missionDir, feature.id);
  const round = roundOpt !== undefined ? Number(roundOpt) : canonicalBefore.length + 1;
  if (!Number.isInteger(round) || round < 1) throw new InputError(`--round must be a positive integer, got '${roundOpt}'`);
  const sameRound = canonicalBefore.filter((old) => old.round === round);
  if (sameRound.length > 1) {
    throw new RejectError(
      `re-recording round ${round} is ambiguous: ${sameRound.length} canonical records share it; ` +
        'supersede the duplicates explicitly with review-supersede first',
      'review_ambiguous'
    );
  }
  for (const old of sameRound) {
    if (old.round !== round) continue;
    logEvent(missionDir, 'review_superseded', {
      featureId: feature.id,
      reviewId: old.reviewId || null,
      round: old.round,
      targetTs: old.ts,
      supersededStatus: old.status,
      supersededItemIds: old.itemIds || [],
      reason: `superseded by re-record of round ${round} (reviewId ${crypto.randomUUID()})`,
      supersededBy: crypto.randomUUID(),
    });
  }
  const priorAfter = reviewEvents(missionDir, feature.id);
  const lastIds = priorAfter.length ? new Set(priorAfter[priorAfter.length - 1].itemIds || []) : new Set();
  const persisted = blocking.map((f) => f.itemId).filter((id) => lastIds.has(id));
  logEvent(missionDir, 'review_recorded', {
    featureId: feature.id,
    reviewId: crypto.randomUUID(),
    round,
    status: derived,
    blocking: blocking.length,
    nonBlocking: findings.length - blocking.length,
    itemIds: findings.map((f) => f.itemId),
    nonBlockingIds: findings.filter((f) => f.severity === 'non_blocking').map((f) => f.itemId),
    persisted,
    reportPath: path.resolve(reportPath),
    head: feature.branch
      ? (git(['rev-parse', feature.branch], state.repoRoot).stdout || null)
      : null,
  });
  return { featureId: feature.id, round, status: derived, blocking: blocking.length, nonBlocking: findings.length - blocking.length, persisted };
}

function cmdRecordReview(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  const reportPath = options['report'];
  if (!featureId || !reportPath) {
    throw new InputError('usage: devflow.cjs record-review --mission-dir <dir> --feature <id> --report <review.json> [--round <n>]');
  }
  const { state, featuresDoc } = loadMission(missionDir);
  const out = recordReviewCore(missionDir, state, featuresDoc, featureId, reportPath, options['round']);
  if (options['json'] === true || options['json'] === 'true') {
    process.stdout.write(JSON.stringify(out, null, 2) + '\n');
    return;
  }
  process.stdout.write(
    `review round ${out.round} for ${out.featureId}: ${out.status} (${out.blocking} blocking, ${out.nonBlocking} non-blocking)\n` +
      (out.persisted.length
        ? `persisted across rounds: ${out.persisted.join(', ')} — escalate rather than sending another fix round\n`
        : '')
  );
}

/**
 * Audit repair: mark one recorded review verdict as superseded, so the canonical
 * review history (reviewEvents) no longer includes it.
 *
 * record-review is append-only; a duplicate round or a mislabelled itemId could
 * previously poison the persistence ledger with no way out. This is the escape
 * hatch: it targets ONE exact event — by reviewId for records written after
 * reviewId existed, or by the (featureId, round, ts) triple for legacy rows —
 * and appends a review_superseded event. Nothing is deleted; the audit trail
 * keeps both the bad record and the correction, and every reader that matters
 * (lastReviewVerdict, openReviewFindings, round numbering, takenItemIds) now
 * derives from reviewEvents, which excludes superseded entries.
 *
 * Targeting by round alone is refused: the log can hold duplicate rounds, and
 * suppressing by an ambiguous key could silence the wrong audit record.
 */
function cmdReviewSupersede(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  const reviewId = options['review-id'];
  const ts = options['ts'];
  const round = options['round'];
  const reason = options['reason'];
  if (!featureId || !reason) {
    throw new InputError(
      'usage: devflow.cjs review-supersede --mission-dir <dir> --feature <id> --review-id <uuid> --reason <why>\n' +
        '  (legacy records without a reviewId: --feature <id> --round <n> --ts <iso-timestamp> --reason <why>)'
    );
  }
  const events = readEvents(missionDir);
  const targets = events.filter((e) => {
    if (e.event !== 'review_recorded' || e.featureId !== featureId) return false;
    if (reviewId) return e.reviewId === reviewId;
    if (round !== undefined && ts) return String(e.round) === String(round) && e.ts === ts;
    return false;
  });
  if (targets.length === 0) {
    throw new RejectError(
      reviewId
        ? `no review_recorded with reviewId '${reviewId}' for feature '${featureId}'`
        : `no review_recorded for feature '${featureId}' round ${round} at ts ${ts}`,
      'review_not_found'
    );
  }
  if (targets.length > 1) {
    throw new RejectError(
      `the given key matches ${targets.length} review_recorded events; use --review-id for an exact target`,
      'review_ambiguous'
    );
  }
  const target = targets[0];
  logEvent(missionDir, 'review_superseded', {
    featureId,
    reviewId: target.reviewId || null,
    round: target.round,
    targetTs: target.ts,
    supersededStatus: target.status,
    supersededItemIds: target.itemIds || [],
    reason,
    supersededBy: crypto.randomUUID(),
  });
  process.stdout.write(
    `superseded review round ${target.round} for ${featureId} (${target.status}, ${(target.itemIds || []).length} item(s)): ${reason}\n`
  );
}

/**
 * Grow the plan by appending, never by rewriting.
 *
 * That single restriction is what keeps features.json simultaneously a schedule and an execution
 * history: at any point "was this in the original plan, and if not, what caused it?" has an answer.
 * Wanting to change what an existing feature means is legitimate -- it just is not an edit. Cancel
 * it, append its replacement, and let the log show both.
 *
 * Every appended feature names exactly one cause. An amend with no cause is an unexplained plan
 * change, which is the thing this exists to prevent.
 */
function cmdAmend(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featurePath = options['add-feature'];
  const assertionPath = options['add-assertion'];
  if (!featurePath && !assertionPath) {
    throw new InputError(
      'usage: devflow.cjs amend --mission-dir <dir> (--add-feature <feature.json> | --add-assertion <assertion.json>) [--json]'
    );
  }
  const { state, featuresDoc } = loadMission(missionDir);
  const events = readEvents(missionDir);
  const out = { added: [], assertions: [] };
  // Nothing is written until everything validates. A half-applied amend leaves a contract the
  // mission's own digest no longer matches, which pauses the mission on a change nobody made.
  const contract = readJsonFile(state.contractPath);

  // --- an assertion first: a fix feature may claim one that does not exist yet ------------------
  if (assertionPath) {
    const incoming = readJsonFile(assertionPath);
    const list = Array.isArray(incoming) ? incoming : [incoming];
    const known = new Set((contract.assertions || []).map((a) => a.id));
    for (const a of list) {
      if (!a || typeof a !== 'object' || typeof a.id !== 'string' || !ASSERTION_ID_RE.test(a.id)) {
        throw new InputError(`an added assertion needs a VAL-xxx id, got ${JSON.stringify(a && a.id)}`);
      }
      if (known.has(a.id)) {
        throw new RejectError(`assertion ${a.id} already exists; the contract may only gain assertions`, 'assertion_exists');
      }
      for (const k of ['title', 'behavior', 'evidence']) {
        if (typeof a[k] !== 'string' || !a[k].trim()) {
          throw new InputError(`${a.id}: '${k}' is required — an assertion with no ${k} cannot be closed`);
        }
      }
      contract.assertions.push(Object.assign({ status: 'pending' }, a));
      known.add(a.id);
      out.assertions.push(a.id);
    }
  }

  // --- then the feature -------------------------------------------------------------------------
  if (featurePath) {
    const incoming = readJsonFile(featurePath);
    const list = Array.isArray(incoming) ? incoming : [incoming];
    const existing = new Map(featuresDoc.features.map((f) => [f.id, f]));
    const sealed = new Set(milestoneStates(featuresDoc.features, readEvents(missionDir))
      .filter((ms) => ms.sealed).map((ms) => ms.milestone));
    for (const raw of list) {
      const f = Object.assign({
        dependsOn: [], touches: [], fulfills: [], expectedBehavior: [],
        status: 'pending', workerSessionIds: [], currentWorkerSessionId: null, addressesItemId: [],
        branch: null, worktree: null,
      }, raw, { origin: 'amend' });
      if (!f.id || typeof f.id !== 'string') throw new InputError('an appended feature needs an id');
      // Never reuse an id, including a cancelled one: the plan is also the execution history.
      if (existing.has(f.id)) {
        throw new RejectError(`feature id '${f.id}' is already used (status ${existing.get(f.id).status}); ids are never reused`, 'id_reused');
      }
      // A review fix covers the finding that caused it. reconcile refuses a disposition for an
      // item the feature does not list, and compile emits that list empty for everything -- so
      // without this, a review finding could never be reconciled through the supported flow. The
      // cause already names the item; an explicit list still wins, because one fix may close more
      // findings than the one that triggered it.
      if (f.cause && f.cause.kind === 'review' && f.cause.itemId && !(raw.addressesItemId || []).length) {
        f.addressesItemId = [f.cause.itemId];
      }
      if (!f.cause || !AMEND_CAUSE_KINDS.includes(f.cause.kind)) {
        throw new RejectError(
          `appended feature '${f.id}' must name a cause of kind ${AMEND_CAUSE_KINDS.join(' | ')}`,
          'missing_cause'
        );
      }
      // A `misc-*` milestone is the bucket for small non-blocking debt, and it is validated as one
      // batch. Its cap exists so that round stays feasible -- and an uncapped bucket fills with
      // exactly the work that was too small to argue about, one feature at a time.
      if (/^misc-/.test(f.milestone || '')) {
        const held = featuresDoc.features.filter((x) => x.milestone === f.milestone && x.status !== 'cancelled').length;
        if (held >= MISC_MILESTONE_CAP) {
          throw new RejectError(
            `${f.milestone} already holds ${held} feature(s), which is its cap: its validation round has to stay feasible. ` +
              `Open a new misc milestone rather than overfilling this one.`,
            'misc_milestone_full'
          );
        }
      }
      // A sealed milestone passed its gate and its proof. Appending to it means shipping
      // unvalidated work behind a green record.
      if (sealed.has(f.milestone)) {
        throw new RejectError(
          `milestone '${f.milestone}' is sealed; append to an unsealed or follow-up milestone instead`,
          'milestone_sealed'
        );
      }
      featuresDoc.features.push(f);
      existing.set(f.id, f);
      out.added.push(f.id);
    }
  }

  // --- the gates the plan was approved under, against the amended whole -------------------------
  const assertionIds = new Set((contract.assertions || []).map((a) => a.id).filter(Boolean));
  const errs = validateAll(featuresDoc, assertionIds);
  if (errs.length) {
    throw new RejectError(`the amended plan does not validate:\n${formatErrors(errs)}`, 'amend_invalid');
  }
  const claims = new Map();
  for (const f of featuresDoc.features) {
    for (const v of f.fulfills || []) claims.set(v, (claims.get(v) || 0) + 1);
  }
  const uncovered = [...assertionIds].filter((a) => claims.get(a) !== 1);
  if (uncovered.length) {
    // An assertion nobody builds fails this gate the moment the next round loads the mission, so
    // the claimant comes in the same call or the amend does not happen at all.
    throw new RejectError(
      `coverage gate failed after the amend: assertions not claimed exactly once: ${uncovered.join(', ')}`,
      'coverage_failed'
    );
  }
  // The self-activating proof gate, applied to the amended whole: a plan that proves some of its
  // contract must prove all of it. Without this, adding an assertion with a fix feature that only
  // fulfills it leaves it unproven and re-records the digests, and no compiled validator's proves
  // list is extensible -- the mission later reaches an unproven ledger with no scheduled proof.
  if (proofCoverageApplies(featuresDoc.features, false)) {
    const perrs = proofCoverageErrors(featuresDoc.features, assertionIds);
    if (perrs.length) {
      throw new RejectError(`proof coverage gate failed after the amend:\n${formatErrors(perrs)}`, 'proof_coverage_failed');
    }
  }

  // --- everything held: now write ---------------------------------------------------------------
  if (out.assertions.length) {
    const prevDigest = state.contractDigest;
    fs.writeFileSync(state.contractPath, JSON.stringify(contract, null, 2) + '\n');
    state.contractDigest = crypto.createHash('sha256').update(fs.readFileSync(state.contractPath)).digest('hex');
    const hashes = contractAssertionHashes(state.contractPath);
    logEvent(missionDir, 'contract_amended', {
      added: out.assertions,
      hashes: out.assertions.map((id) => hashes[id]),
      prevDigest,
      newDigest: state.contractDigest,
    });
  }
  if (out.added.length) {
    const prevDigest = state.planDigest;
    state.planDigest = planDigest(featuresDoc.features, featuresDoc, injectedFeatureIds(events));
    // The appended features go into the COMMITTED plan, which is the whole point: a plan that grew
    // for a reason nobody can find is not an audit trail. This dirties the worktree, and the next
    // spawn refuses a dirty one -- so committing the amend is a step, not an inconvenience.
    const injected = injectedFeatureIds(events);
    const source = readPlanSource(state);
    source.features = featuresDoc.features.filter((f) => !injected.has(f.id)).map((f) => {
      const plan = {};
      for (const k of Object.keys(f)) if (!RUNTIME_FIELDS.includes(k)) plan[k] = f[k];
      return plan;
    });
    fs.writeFileSync(state.featuresSourcePath, JSON.stringify(source, null, 2) + '\n');
    writeFeaturesDoc(missionDir, featuresDoc);
    logEvent(missionDir, 'amended', {
      features: out.added,
      causes: out.added.map((id) => featuresDoc.features.find((f) => f.id === id).cause),
      prevDigest,
      newDigest: state.planDigest,
      reportedToUser: false,
    });
    // Amend additions are tickets too: file their Issue projections now
    // (clause 2: amend additions at amend time; idempotent — a retry reuses).
    if (state.forge === true) {
      for (const id of out.added) {
        const f = featuresDoc.features.find((x) => x.id === id);
        if (f && !f.issueId) f.issueId = forgeEnsureTicketIssue(missionDir, state, f);
      }
      writeFeaturesDoc(missionDir, featuresDoc);
    }
  }
  writeState(missionDir, state);

  if (options['json'] === true || options['json'] === 'true') {
    process.stdout.write(JSON.stringify(out, null, 2) + '\n');
    return;
  }
  if (out.assertions.length) process.stdout.write(`contract gained ${out.assertions.join(', ')}\n`);
  if (out.added.length) {
    process.stdout.write(
      `appended ${out.added.join(', ')} to ${state.featuresSourcePath}\n` +
      `commit it before the next spawn — the plan grew, and that belongs in history\n`
    );
  }
}

/**
 * Drop a feature without erasing it.
 *
 * The plan is also the execution history, so a feature that should no longer be built is cancelled
 * rather than deleted -- and cancelling is the second half of the only legitimate way to change
 * what a feature means: cancel it, append its replacement, and let the log show both. A cancelled
 * feature satisfies nobody's `dependsOn`, so whatever depended on it must be cancelled too or
 * re-pointed by appending.
 */
function cmdCancel(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  const reason = options['reason'];
  if (!featureId || !reason) {
    throw new InputError('usage: devflow.cjs cancel --mission-dir <dir> --feature <id> --reason <why>');
  }
  const { state, featuresDoc } = loadMission(missionDir);
  const feature = featuresDoc.features.find((f) => f.id === String(featureId));
  if (!feature) throw new RejectError(`unknown feature '${featureId}' in this mission's plan`, 'unknown_feature');
  if (feature.status === 'cancelled') {
    throw new RejectError(`feature '${feature.id}' is already cancelled`, 'already_cancelled');
  }
  if (feature.status === 'completed') {
    // Cancelling built work would erase the record of what shipped. Undoing it is a new feature.
    throw new RejectError(`feature '${feature.id}' is already completed; append a feature that undoes it instead`, 'feature_completed');
  }
  // The worktree goes; the branch stays. Cancelling exists because deleting destroys history --
  // deleting the commits it holds would contradict its own reason. A branch left behind is
  // findable (`git branch --list 'loop/*'`) and cheap; work that was thrown away is neither.
  const keptBranch = feature.branch;
  const recycled = recycleFeatureWorktree(missionDir, state, feature, {
    force: true, keepBranch: true, allowMissingDirectoryRecovery: true,
  });
  if (recycled.outcome === 'refused') {
    // Atomicity: a cancellation that could not remove its worktree must not be recorded as done.
    // The feature keeps its status and worker; only the refusal is on the log. Without this the
    // runtime says `cancelled` while the worktree is still on disk -- two answers to one question.
    logEvent(missionDir, 'cancel_refused', {
      featureId: feature.id, branch: feature.branch, worktree: feature.worktree,
    });
    throw new RejectError(
      `feature '${feature.id}' worktree could not be removed; status left unchanged (see worktree_recycle_refused)`,
      'worktree_recycle_refused'
    );
  }
  if (recycled.outcome === 'partial') {
    // The registry is gone but the directory remains: an undeleted artifact. Do NOT cancel --
    // that would erase lifecycle ownership of it. Pause the feature durably (blocks mission
    // closure) with feature.worktree kept as the recovery record naming the leftover directory.
    // The worker is retired too: a paused feature must not appear owned by an active worker --
    // the roster and the status have to agree. The reason is recorded before the retirement.
    // Recovery: remove the orphan directory, then retry cancel (recycle's directory-gone path).
    logEvent(missionDir, 'cancel_partial', {
      featureId: feature.id, branch: feature.branch, directory: feature.worktree,
      reason, hint: 'remove the orphan directory, then retry cancel',
    });
    if (feature.currentWorkerSessionId) {
      removeActiveWorker(state, feature.currentWorkerSessionId);
      feature.currentWorkerSessionId = null;
    }
    feature.status = 'paused';
    writeState(missionDir, state);
    writeFeaturesDoc(missionDir, featuresDoc);
    throw new RejectError(
      `feature '${feature.id}' worktree partially removed; orphan directory remains: ${feature.worktree}. ` +
      'Feature paused. Remove the directory, then retry cancel.',
      'worktree_partial_removal'
    );
  }
  if (recycled.recoveredDirectory) {
    // A prior partial left the directory behind; it is gone now, so the audit trail must say so
    // before the cancellation is recorded.
    logEvent(missionDir, 'worktree_partial_recovered', {
      featureId: feature.id, branch: feature.branch, directory: recycled.recoveredDirectory,
    });
  }
  // Worker detachment happens only after the recycle succeeded: a refused cancel must leave the
  // active-worker roster and currentWorkerSessionId exactly as they were.
  if (feature.currentWorkerSessionId) {
    removeActiveWorker(state, feature.currentWorkerSessionId);
    feature.currentWorkerSessionId = null;
  }
  feature.status = 'cancelled';
  writeState(missionDir, state);
  writeFeaturesDoc(missionDir, featuresDoc);
  logEvent(missionDir, 'feature_cancelled', { featureId: feature.id, reason, keptBranch: keptBranch || null });
  // The forge-side projection follows the ticket: a cancelled ticket's Issue is
  // closed with an abandon-reason comment (clause 2), adapter-owned.
  if (state.forge === true) forgeAbandonTicketIssue(missionDir, state, feature, reason);
  // Cancelling is the strategy-change exit under the autonomy policy (SKILL.md §0.5 case 3):
  // a retry-exhausted `failed` feature, or a paused one, is cancelled so a replacement with a
  // different approach can be amended. If this was the last human stop, the mission-level pause
  // was left behind by its cause — clear it the way resolve-pause does, with the cleared reason on
  // the record rather than a silent state flip. A paused/failed feature of any other feature keeps
  // the mission paused: the reason stays because the stop is still real.
  if (state.state === 'paused' && !RECOVERABLE_PAUSE.includes(state.pauseReason)) {
    const humanStops = featuresDoc.features
      .filter((f) => f.status === 'paused' || f.status === 'failed')
      .map((f) => f.id);
    if (humanStops.length === 0) {
      const clearedReason = state.pauseReason;
      state.state = 'orchestrator_turn';
      state.pauseReason = null;
      writeState(missionDir, state);
      logEvent(missionDir, 'pause_cleared', {
        featureId: feature.id,
        clearedReason,
        reason,
        remainingPaused: [],
        missionUnblocked: true,
      });
      process.stdout.write(`mission resumed: the last human-type stop (${clearedReason}) is cancelled\n`);
    }
  }
  const orphans = featuresDoc.features
    .filter((f) => f.status === 'pending' && (f.dependsOn || []).includes(feature.id))
    .map((f) => f.id);
  process.stdout.write(`cancelled ${feature.id}: ${reason}\n`);
  if (keptBranch) {
    process.stdout.write(`its branch is kept: ${keptBranch} — delete it yourself once you are sure nothing there is wanted\n`);
  }
  if (orphans.length) {
    process.stdout.write(
      `these depend on it and can never become eligible now: ${orphans.join(', ')} — cancel them too, or append replacements\n`
    );
  }
}

/**
 * bind-authority: point a mission at plan/contract files whose content equals the digests locked
 * at propose, without rewriting either digest.
 *
 * Why this exists: the plan is a committed file, and a mission's featuresSourcePath is where that
 * file lives. When the shared plan file evolves (later rounds add features), older missions stop
 * matching it -- loadMission refuses every command with features_tampered. Binding restores such
 * a mission by pointing it at its own immutable snapshot (a byte copy of the approved revision,
 * committed), after proving the snapshot's digests equal the locked ones. The digests are the
 * authority anchor and are never rewritten: if the content does not match, the bind is refused.
 *
 * Checks, in order:
 *   1. containment: each candidate must resolve (realpath) to a regular file inside state.repoRoot
 *      -- a tracked symlink can be clean while its target supplies different plan bytes.
 *   2. tracked and clean: both files must be tracked by git and match HEAD -- an uncommitted
 *      snapshot can be edited without any authority noticing.
 *   3. digest equivalence: planDigest(features) === state.planDigest and
 *      sha256(contract) === state.contractDigest -- full-field coverage (touches, verification,
 *      assertions), so a same-feature-set-but-different-plan file cannot be adopted silently.
 *   4. --reason is required: binding authority is a decision, and it is recorded.
 * On success only featuresSourcePath/contractPath change; both digests stay as locked at propose.
 */
function cmdBindAuthority(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featuresPath = options['features-path'];
  const contractPath = options['contract-path'];
  const reason = options['reason'];
  if (!featuresPath || !contractPath || !reason) {
    throw new InputError(
      'usage: devflow.cjs bind-authority --mission-dir <dir> --features-path <path> --contract-path <path> --reason <why>'
    );
  }
  // Deliberately NOT loadMission: its planDigest check is exactly the mismatch this command heals,
  // and calling it here would make the cure refuse to run.
  const state = readState(missionDir);
  const repoRoot = fs.realpathSync(path.resolve(state.repoRoot || process.cwd()));
  const bound = {};
  for (const [label, p] of [['features', featuresPath], ['contract', contractPath]]) {
    const abs = path.resolve(p);
    let real;
    try {
      real = fs.realpathSync(abs);
    } catch {
      throw new RejectError(`${label} file ${abs} does not resolve to a real path`, `${label}_not_resolvable`);
    }
    if (!fs.statSync(real).isFile()) {
      throw new RejectError(`${label} file ${real} is not a regular file`, `${label}_not_regular_file`);
    }
    const rel = path.relative(repoRoot, real);
    if (rel.startsWith('..') || path.isAbsolute(rel)) {
      throw new RejectError(
        `${label} file ${real} resolves outside the repository (${repoRoot}); ` +
        'a tracked symlink can be clean while its target supplies different plan bytes',
        `${label}_outside_repository`
      );
    }
    const tracked = git(['ls-files', '--error-unmatch', real], repoRoot);
    if (!tracked.ok) {
      throw new RejectError(
        `${label} file ${real} is inside the repository but not tracked by git; commit the snapshot first`,
        `${label}_not_tracked`
      );
    }
    // Both index and worktree must match HEAD: `git diff HEAD` alone passes for a staged
    // modification whose working-tree copy was reverted, and bind would then anchor the mission
    // to content no commit records.
    const unstaged = git(['diff', '--quiet', '--', real], repoRoot);
    if (unstaged.status !== 0) {
      throw new RejectError(
        `${label} file ${real} differs from its index entry; commit the snapshot first`,
        `${label}_not_clean`
      );
    }
    const staged = git(['diff', '--cached', '--quiet', '--', real], repoRoot);
    if (staged.status !== 0) {
      throw new RejectError(
        `${label} file ${real} has staged, uncommitted content; commit the snapshot first`,
        `${label}_staged_not_clean`
      );
    }
    bound[label] = real;
  }
  // Digest equivalence, full-field: the digest covers touches, verification, assertions, the
  // whole plan -- identical feature ids do not mean an identical plan.
  const doc = JSON.parse(fs.readFileSync(bound.features, 'utf8'));
  const digest = planDigest(doc.features, doc, injectedFeatureIds(readEvents(missionDir)));
  if (digest !== state.planDigest) {
    throw new RejectError(
      `features file ${bound.features} does not match the plan locked at propose ` +
      `(planDigest ${state.planDigest.slice(0, 12)} vs ${digest.slice(0, 12)}); ` +
      'binding would silently adopt a different plan',
      'plan_digest_mismatch'
    );
  }
  const contractBytes = fs.readFileSync(bound.contract);
  const contractDigest = crypto.createHash('sha256').update(contractBytes).digest('hex');
  if (contractDigest !== state.contractDigest) {
    throw new RejectError(
      `contract file ${bound.contract} does not match the digest locked at propose ` +
      `(${state.contractDigest.slice(0, 12)} vs ${contractDigest.slice(0, 12)})`,
      'contract_digest_mismatch'
    );
  }
  // Only the paths change. The digests are the anchor; they stay exactly as locked at propose.
  const prevFeatures = state.featuresSourcePath;
  const prevContract = state.contractPath;
  state.featuresSourcePath = bound.features;
  state.contractPath = bound.contract;
  writeState(missionDir, state);
  logEvent(missionDir, 'authority_bound', {
    featuresSource: `${prevFeatures} -> ${state.featuresSourcePath}`,
    contractSource: `${prevContract} -> ${state.contractPath}`,
    reason,
    planDigest: state.planDigest,
    contractDigest: state.contractDigest,
  });
  process.stdout.write(
    `bound ${missionDir}\n` +
    `  features: ${prevFeatures} -> ${state.featuresSourcePath}\n` +
    `  contract: ${prevContract} -> ${state.contractPath}\n` +
    `  digests unchanged (${state.planDigest.slice(0, 12)} / ${state.contractDigest.slice(0, 12)})\n`
  );
}

/** Run the merge gate for one feature: re-verify on its branch, fast-forward, recycle. */
function cmdMergeFeature(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  if (!featureId) throw new InputError('usage: devflow.cjs merge-feature --mission-dir <dir> --feature <id> [--json]');
  const verificationTimeoutMs = (Number(options['timeout']) || 1800) * 1000;
  const maxAttempts = options['max-attempts'] !== undefined ? Number(options['max-attempts']) : 3;
  const { state, featuresDoc } = loadMission(missionDir);
  const feature = featuresDoc.features.find((f) => f.id === String(featureId));
  if (!feature) throw new RejectError(`unknown feature '${featureId}' in this mission's plan`, 'unknown_feature');
  if (feature.status !== 'in_review') {
    throw new RejectError(`feature '${feature.id}' is ${feature.status}, not in_review`, 'feature_not_in_review');
  }
  // Forge missions first: the reconcile probe converges any crash state a merged
  // PR left behind (merged-not-recorded / open issue / remaining worktree) before
  // the merge gate runs, and never re-invokes `pr merge` (clause 7).
  if (state.forge === true) {
    for (const f of featuresDoc.features) {
      if (f.prId && f.status !== 'cancelled' && f.status !== 'failed') {
        forgeReconcileProbe(missionDir, state, featuresDoc, f);
      }
    }
  }
  const merged = mergeFeature(missionDir, state, featuresDoc, feature, verificationTimeoutMs, maxAttempts);
  if (!merged.ok) {
    logEvent(missionDir, 'merge_blocked', { featureId: feature.id, code: merged.code });
    throw new RejectError(merged.message, merged.code);
  }
  process.stdout.write(`merged ${feature.id} into ${state.expectedBranch} (${merged.commitId}); worktree recycled\n`);
}

/**
 * One round boundary: settle what came back, advance every gate that needs no agent, and report.
 *
 * Extracted from `cmdNextFeature` so a second caller can have the same turn without going through
 * the CLI and without a second implementation of it. `wave` is that caller: the difference between
 * the two commands is what happens AFTER this returns, not what a round means. Two implementations
 * of a round boundary would drift, and the drift would be invisible -- both would look right in
 * isolation and disagree about whether a receipt had been settled.
 * Returns the report plus the live `state`/`featuresDoc`, because the caller may go on to mutate
 * them (claiming, rendering) inside the same load.
 */
function settleRound(missionDir, { maxAttempts = 3, verificationTimeoutMs = 1800000, startRound = false } = {}) {
  const { state, featuresDoc } = loadMission(missionDir);
  // Forge missions first: the reconcile probe converges any crash state a merged
  // PR left behind (merged-not-recorded / open issue / remaining worktree) before
  // the merge gate runs, and never re-invokes `pr merge` (clause 7).
  if (state.forge === true) {
    for (const f of featuresDoc.features) {
      if (f.prId && f.status !== 'cancelled' && f.status !== 'failed') {
        forgeReconcileProbe(missionDir, state, featuresDoc, f);
      }
    }
  }
  // Settle first: a worker that submitted its receipt left the feature in_progress, and the
  // transitions that follow a receipt (plan verification, milestone completion, validator
  // injection) belong to the scheduler, not to the worker that wrote it.
  for (const f of featuresDoc.features) {
    if (f.status !== 'in_progress' || !f.currentWorkerSessionId) continue;
    const sid = f.currentWorkerSessionId;
    if (!findCompletedReceipt(missionDir, sid, state)) continue;
    applyReceipt(missionDir, state, featuresDoc, f, sid, maxAttempts, verificationTimeoutMs);
  }
  // A feature whose branch is ready and whose review has landed merges here. With --review-gate on
  // this is where the loop waits: the feature stays in_review until a verdict is recorded.
  const mergeBlocks = [];
  for (const f of featuresDoc.features) {
    if (f.status !== 'in_review') continue;
    const merged = mergeFeature(missionDir, state, featuresDoc, f, verificationTimeoutMs, maxAttempts);
    if (!merged.ok) mergeBlocks.push({ featureId: f.id, code: merged.code, message: merged.message });
  }

  const { schedulingBlocked, blockReason } = recoverMission(missionDir, state, featuresDoc, maxAttempts);

  let exhausted = [];
  if (!schedulingBlocked && startRound) {
    exhausted = chargeRepairRounds(missionDir, featuresDoc.features, state);
    if (exhausted.length) {
      const first = exhausted[0];
      state.state = 'paused';
      state.pauseReason = 'milestone_repair_budget_exhausted';
      writeState(missionDir, state);
      const ledger = state.validatorsRequested === true ? missionLedger(missionDir, state) : { unproven: [] };
      logEvent(missionDir, 'milestone_repair_budget_exhausted', {
        milestone: first.milestone, round: first.round, unproven: ledger.unproven,
      });
    }
  }

  const blocked = schedulingBlocked || exhausted.length > 0;
  const eligible = blocked ? [] : eligibleFeatures(featuresDoc.features);
  const inProgress = featuresDoc.features.filter((f) => f.status === 'in_progress');
  // Same predicate closingReview uses: a cancelled feature is settled, and listing it as open work
  // when the mission is blocked but not when it closes is two answers to one question.
  let closing = {
    done: false,
    open: featuresDoc.features.filter((f) => f.status !== 'completed' && f.status !== 'cancelled'),
    unproven: [],
    openFindings: [],
    salvaged: [],
    blockingReviews: [],
  };
  if (!blocked && !eligible.length && !inProgress.length) {
    closing = closingReview(missionDir, state, featuresDoc);
  }

  const out = {
    missionState: state.state || 'orchestrator_turn',
    pauseReason: state.state === 'paused' ? state.pauseReason || blockReason || null : null,
    done: closing.done,
    eligible: eligible.map((f) => Object.assign({}, f, { assertions: assertionTextFor(state, f, featuresDoc.features) })),
    inProgress: inProgress.map((f) => ({ id: f.id, workerSessionId: f.currentWorkerSessionId })),
    awaitingMerge: featuresDoc.features.filter((f) => f.status === 'in_review')
      .map((f) => Object.assign({ id: f.id, branch: f.branch, worktree: f.worktree },
        mergeBlocks.find((b) => b.featureId === f.id) || {})),
    open: closing.open.map((f) => ({ id: f.id, status: f.status })),
    unproven: closing.unproven,
    openFindings: closing.openFindings || [],
    salvaged: closing.salvaged || [],
    blockingReviews: closing.blockingReviews || [],
  };
  return { out, state, featuresDoc };
}

function cmdNextFeature(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const asJson = options['json'] === true || options['json'] === 'true';
  const maxAttempts = options['max-attempts'] !== undefined ? Number(options['max-attempts']) : 3;
  if (!Number.isInteger(maxAttempts) || maxAttempts < 1) {
    throw new InputError(`--max-attempts must be a positive integer, got '${options['max-attempts']}'`);
  }
  const { out, featuresDoc } = settleRound(missionDir, {
    maxAttempts,
    verificationTimeoutMs: (Number(options['timeout']) || 1800) * 1000,
    startRound: options['start-round'] === true || options['start-round'] === 'true',
  });
  if (asJson) {
    process.stdout.write(JSON.stringify(out, null, 2) + '\n');
    return;
  }
  if (out.done) {
    process.stdout.write(`mission completed: all ${featuresDoc.features.length} feature(s) done\n`);
    return;
  }
  if (out.eligible.length) {
    process.stdout.write(
      `eligible (${out.eligible.length}): ${out.eligible.map((f) => `${f.id} [${f.milestone || '-'}]`).join(', ')}\n`
    );
  } else {
    process.stdout.write(`nothing eligible; mission state=${out.missionState}${out.pauseReason ? ` (${out.pauseReason})` : ''}\n`);
  }
  if (out.inProgress.length) {
    process.stdout.write(`in progress: ${out.inProgress.map((f) => `${f.id}@${f.workerSessionId}`).join(', ')}\n`);
  }
  if (out.unproven.length) process.stdout.write(`unproven: ${out.unproven.join(', ')}\n`);
  // A paused mission with no visible recovery command reads as wedged: name the judgement that
  // clears it. Trigger on the PAUSE, not out.unproven: an already-paused mission takes
  // settleRound's early return, which carries no unproven list. Same derivation as wave's
  // yourMove -- the report path stays the caller's to name.
  if (out.missionState === 'paused' && out.pauseReason === 'assertions_unproven') {
    for (const m of pendingOrchestratorWork(missionDir, readState(missionDir), featuresDoc)) {
      if (m.kind !== 'record-verdicts') continue;
      process.stdout.write(
        `  to clear: record-verdicts --mission-dir <dir> --report <${m.feature}'s report.json> --feature ${m.feature}  (${m.why})\n`
      );
    }
  }
  if (out.open.length) process.stdout.write(`open: ${out.open.map((f) => `${f.id}(${f.status})`).join(', ')}\n`);
}

/**
 * Does the feature's proof claim touch an assertion the ledger still owes? That is the one kind of
 * work an assertions_unproven pause exists to get done, so it is the one kind the pause must not
 * refuse. `proves` only: an injected assertions-validator derives its obligation from the milestone,
 * but such a feature is never pending at closing -- the gate fires only when nothing is open.
 */
function provesUnprovenAssertion(missionDir, state, feature) {
  if (!Array.isArray(feature.proves) || feature.proves.length === 0) return false;
  const unproven = new Set(missionLedger(missionDir, state).unproven);
  return feature.proves.some((id) => unproven.has(id));
}

/**
 * Take a feature and open a worker session for it, returning everything a worker needs.
 *
 * The receipt invocation it prints carries every argument explicitly. A subagent inherits no
 * environment, and `end-feature`'s ownership check reads exactly the three facts a spawned process
 * would have been handed through it -- so the invocation is not a convenience, it is the bridge.
 */
/**
 * Everything that stands between a feature and a worker, as a question rather than a refusal.
 *
 * `claim` asks this and throws; `wave` asks it and moves on to the next candidate. They must ask
 * the SAME question -- a scheduler whose idea of admissible differs from the gate's is the
 * "collisions invisible until runtime" failure, rebuilt one layer up. So the predicate lives here
 * and the throwing lives in `cmdClaim`, rather than the reverse.
 *
 * Excludes `preSpawnChecks`: that one is not a predicate at all. It pauses the mission when it
 * fails, and what it checks -- the integration branch, a clean worktree -- is mission-wide, not
 * per-feature. A caller admitting a wave runs it ONCE, before the loop; running it per candidate
 * would pause the mission on behalf of a feature that merely happened to be first in the list.
 */
function claimBlock(state, featuresDoc, feature, missionDir) {
  if (state.state === 'paused' && state.pauseReason && !RECOVERABLE_PAUSE.includes(state.pauseReason)) {
    // An assertions_unproven pause clears only when a verdict proves the open assertions, and a
    // verdict comes from running a validation feature -- the remedy is scheduleable work, not a
    // human decision. Refusing it deadlocks the mission (forge issue #1), so a pending feature
    // whose proves hits an unproven assertion passes THIS check; every check below still applies.
    const remedial = state.pauseReason === 'assertions_unproven' && missionDir
      && provesUnprovenAssertion(missionDir, state, feature);
    if (!remedial) {
      return { code: state.pauseReason, message: `mission is paused (${state.pauseReason}); resolve it before claiming work` };
    }
  }
  if (feature.status !== 'pending') {
    return { code: 'feature_not_pending', message: `feature '${feature.id}' is ${feature.status}, not pending` };
  }
  const byId = new Map(featuresDoc.features.map((f) => [f.id, f]));
  const unmet = (feature.dependsOn || []).filter((d) => !byId.has(d) || byId.get(d).status !== 'completed');
  if (unmet.length) {
    return { code: 'dependencies_unmet', message: `feature '${feature.id}' depends on incomplete feature(s): ${unmet.join(', ')}` };
  }
  // Concurrency admission: the cap the mission declared, and disjoint writes.
  return concurrencyBlock(state, featuresDoc, feature);
}

function cmdClaim(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  if (!featureId) throw new InputError('usage: devflow.cjs claim --mission-dir <dir> --feature <id> [--json]');
  const verificationTimeoutMs = (Number(options['timeout']) || 1800) * 1000;
  const { state, featuresDoc } = loadMission(missionDir);

  const feature = featuresDoc.features.find((f) => f.id === String(featureId));
  if (!feature) throw new RejectError(`unknown feature '${featureId}' in this mission's plan`, 'unknown_feature');
  const blocked = claimBlock(state, featuresDoc, feature, missionDir);
  if (blocked) throw new RejectError(blocked.message, blocked.code);

  const checks = preSpawnChecks(missionDir, state, featuresDoc, feature, verificationTimeoutMs);
  if (!checks.ok) {
    const last = readEvents(missionDir).filter((e) => e.event === 'feature_paused').pop();
    const hint = last && last.uncommittedAmend
      ? `\n  the plan was amended and not committed (${last.planPath}). Commit it: the plan grew, and that belongs in history.`
      : last && last.dirtyPaths
        ? `\n  uncommitted: ${last.dirtyPaths}`
        : '';
    throw new RejectError(`cannot spawn into this repository state: ${checks.pauseReason}${hint}`, checks.pauseReason);
  }
  const out = openWorkerSession(missionDir, state, featuresDoc, feature,
    { withBrief: options['brief'] === true || options['brief'] === 'true' });
  const { workerSessionId, baselineCommit } = out;
  const wantsBrief = out.brief !== undefined;
  if (options['json'] === true || options['json'] === 'true') {
    process.stdout.write(JSON.stringify(out, null, 2) + '\n');
    return;
  }
  if (wantsBrief) {
    process.stdout.write(out.brief);
    return;
  }
  process.stdout.write(
    `claimed ${feature.id} as ${workerSessionId} (attempt ${out.attempt}, baseline ${baselineCommit})\n` +
      `submit the receipt on stdin to:\n  ${out.endFeatureCommand}\n` +
      `then run next-feature to settle it: a receipt records the work, the merge gate completes it\n`
  );
}

/**
 * Open the worker session and assemble everything a worker needs, brief included.
 *
 * The half of `claim` that acts, separated from the half that refuses, so `wave` can admit a whole
 * wave through the same code path a single `claim` uses. The receipt invocation it builds carries
 * every argument explicitly: a subagent inherits no environment, and `end-feature`'s ownership check
 * reads exactly the three facts a spawned process would have been handed through it.
 */
function openWorkerSession(missionDir, state, featuresDoc, feature, { withBrief = false } = {}) {
  const { workerSessionId, spawnId, baselineCommit, attempts } = startWorkerSession(missionDir, state, featuresDoc, feature);
  const cli = path.resolve(__dirname, 'devflow.cjs');
  const endFeatureArgv = [cli, 'end-feature', '--mission-dir', missionDir, '--feature', feature.id, '--worker-session', workerSessionId];
  const out = {
    missionDir,
    featureId: feature.id,
    workerSessionId,
    spawnId,
    baselineCommit,
    attempt: attempts + 1,
    workingDirectory: featureWorkdir(state, feature),
    worktree: feature.worktree,
    branch: feature.branch,
    integrationBranch: state.expectedBranch,
    expectedBranch: feature.branch || state.expectedBranch,
    devflowCli: cli,
    feature: Object.assign({}, feature, { assertions: assertionTextFor(state, feature, featuresDoc.features) }),
    endFeatureArgv,
    // What a worker runs. Receipt JSON goes on stdin; nothing here depends on the environment.
    endFeatureCommand: `node ${endFeatureArgv.map((a) => (/\s/.test(a) ? JSON.stringify(a) : a)).join(' ')}`,
    env: workerEnvFor(missionDir, state, feature, workerSessionId),
    kind: featureKind(feature),
  };
  if (withBrief) out.brief = renderBrief(missionDir, state, feature, out);
  return out;
}

/**
 * Give a claimed feature back without a receipt.
 *
 * The model loop has no process to watch, so it has to say so explicitly -- and a mission whose
 * only record of a dead subagent is a feature stuck in
 * in_progress is exactly the state that needed a manual state.json edit before this existed.
 */
function cmdRelease(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  const reason = options['reason'] || 'no_receipt';
  const REASONS = new Set([
    'no_receipt', 'user_abort', 'worker_inactivity_timeout', 'worker_process_exit',
    'worker_exit_without_handoff', 'worker_killed_by_user',
  ]);
  if (!featureId) {
    throw new InputError(
      `usage: devflow.cjs release --mission-dir <dir> --feature <id> [--reason <${[...REASONS].join('|')}>]`
    );
  }
  if (!REASONS.has(reason)) {
    throw new InputError(`--reason must be one of: ${[...REASONS].join(', ')}`);
  }
  const { state, featuresDoc } = loadMission(missionDir);
  const feature = featuresDoc.features.find((f) => f.id === String(featureId));
  if (!feature) throw new RejectError(`unknown feature '${featureId}' in this mission's plan`, 'unknown_feature');
  if (feature.status !== 'in_progress') {
    throw new RejectError(`feature '${feature.id}' is ${feature.status}, not in_progress`, 'feature_not_claimed');
  }
  // Releasing over a submitted receipt would throw away real, committed work and reset the
  // feature as if nothing had happened. Settle it instead -- next-feature does that.
  if (feature.currentWorkerSessionId && findCompletedReceipt(missionDir, feature.currentWorkerSessionId, state)) {
    throw new RejectError(
      `worker ${feature.currentWorkerSessionId} already submitted a receipt for '${feature.id}'; run next-feature to settle it`,
      'receipt_present'
    );
  }
  const evidence = feature.worktree;
  requeueWithoutReceipt(missionDir, state, featuresDoc, feature, reason);
  process.stdout.write(`released ${feature.id} back to pending (${reason})\n`);
  if (evidence) {
    // A worker that died without a receipt left its worktree as the only account of what it did.
    // Saying where it is matters, because the next attempt discards it to start clean.
    process.stdout.write(
      `its worktree is still there and is the only account of what happened: ${evidence}\n` +
      `the next attempt on ${feature.id} discards it, so read it first if you need it\n`
    );
  }
}

/**
 * Explicit resume for the resolvable human-type pauses: `blocked`, `boundary_violation`, `partial`.
 *
 * The scheduler never auto-resumes a blocked receipt (see recoverMission), and release
 * only accepts in_progress features, so a worker that reported a blocked receipt whose
 * work is still wanted used to wedge the whole mission: every claim refused, no
 * supported way out. This is the audited recovery: it returns the named feature to
 * pending so it can be claimed again, clears the mission-level pause only when no other
 * human stop remains (a paused feature of another type, or a retry-exhausted `failed`
 * one, keeps the mission paused), and records the resolution in the progress log. The
 * original receipt and its commit stay in the log and on the branch -- resolving is not
 * erasing history, and the next attempt starts from the integration branch, not from
 * the old worktree.
 *
 * The `reason` is the record of what changed outside the receipt, so this is the
 * orchestrator's own instrument under the autonomy policy (SKILL.md §0.5 case 2):
 * clear the blocker and say what changed. A retry-exhausted `failed` feature is NOT
 * resolvable here -- its exit is `cancel` + a replacement feature with a different
 * approach: returning it to pending would be attempt N+1 of the same thing.
 */
const RESOLVABLE_PAUSE_REASONS = ['blocked', 'boundary_violation', 'partial'];
function cmdResolvePause(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  const reason = options['reason'];
  if (!featureId || !reason) {
    throw new InputError('usage: devflow.cjs resolve-pause --mission-dir <dir> --feature <id> --reason <why> (blocked/boundary-violation/partial pauses)');
  }
  const { state, featuresDoc } = loadMission(missionDir);
  const feature = featuresDoc.features.find((f) => f.id === String(featureId));
  if (!feature) throw new RejectError(`unknown feature '${featureId}' in this mission's plan`, 'unknown_feature');
  if (feature.status === 'failed') {
    throw new RejectError(
      `feature '${feature.id}' is failed (retry limit exhausted); its exit is cancel + amend a replacement with a different approach, not a resume`,
      'retry_exhausted_feature'
    );
  }
  if (feature.status !== 'paused') {
    throw new RejectError(`feature '${feature.id}' is ${feature.status}, not paused`, 'feature_not_paused');
  }
  const reasonType = lastPauseReason(missionDir, feature.id);
  if (!RESOLVABLE_PAUSE_REASONS.includes(reasonType)) {
    throw new RejectError(
      `feature '${feature.id}' is paused by '${reasonType || 'unknown'}'; resolvable pauses are ${RESOLVABLE_PAUSE_REASONS.join('/')} (a retry-exhausted feature is cancelled, not resumed)`,
      'unsupported_pause_reason'
    );
  }
  feature.status = 'pending';
  feature.currentWorkerSessionId = null;
  // A feature in `failed` (retry limit exhausted) is a human stop just like a paused one; it must
  // keep the mission paused even when the resolved feature was the only `paused` one.
  const humanStops = featuresDoc.features
    .filter((f) => f.status === 'paused' || f.status === 'failed')
    .map((f) => f.id);
  const missionUnblocked = state.state === 'paused' && humanStops.length === 0;
  if (missionUnblocked) {
    state.state = 'orchestrator_turn';
    state.pauseReason = null;
  }
  writeFeaturesDoc(missionDir, featuresDoc);
  writeState(missionDir, state);
  logEvent(missionDir, 'pause_resolved', {
    featureId: feature.id,
    reason,
    remainingPaused: humanStops,
    missionUnblocked,
  });
  process.stdout.write(
    `resolved pause on ${feature.id} (${reason}); back to pending\n` +
      (missionUnblocked
        ? 'mission resumed: no human-type pauses remain\n'
        : `still paused by: ${humanStops.join(', ')}\n`)
  );
}

function countAttempts(missionDir, featureId, state) {
  // FR-008: only an accepted defect receipt consumes retry budget.
  // Counts only unique worker sessions verified by findCompletedReceipt (handoff + schema + consistency) --
  // forged or invalid raw defect events do not consume budget.
  // A success receipt that failed the plan's verification (t2) is retried on the same budget, so
  // its session counts too: without that the requeue loop has no terminating condition.
  const p = path.join(missionDir, 'progress_log.jsonl');
  if (!fs.existsSync(p)) return 0;
  const seen = new Set();
  for (const line of fs.readFileSync(p, 'utf8').split('\n').filter(Boolean)) {
    try {
      const ev = JSON.parse(line);
      if (ev.event === 'verification_failed' && ev.featureId === featureId && ev.workerSessionId) {
        seen.add(ev.workerSessionId);
      }
      if (ev.event === 'worker_completed' && ev.featureId === featureId && ev.failureClass === 'defect') {
        // A valid pair must belong to this feature: stops a legitimate defect session from another feature being reused to claim budget
        const valid = findCompletedReceipt(missionDir, ev.workerSessionId, state);
        if (valid && valid.ev.featureId === featureId && valid.receipt.failureClass === 'defect') {
          seen.add(ev.workerSessionId);
        }
      }
    } catch { /* skip */ }
  }
  return seen.size;
}

function lastWorkerStarted(missionDir, workerSessionId) {
  // The most recent worker_started event for this worker session (authoritative featureId + baseline)
  const p = path.join(missionDir, 'progress_log.jsonl');
  if (!fs.existsSync(p)) return null;
  const lines = fs.readFileSync(p, 'utf8').split('\n').filter(Boolean);
  for (let i = lines.length - 1; i >= 0; i--) {
    try {
      const ev = JSON.parse(lines[i]);
      if (ev.event === 'worker_started' && ev.workerSessionId === workerSessionId) {
        return { featureId: ev.featureId, baselineCommit: ev.baselineCommit };
      }
    } catch {
      /* skip malformed line */
    }
  }
  return null;
}

/**
 * Scheduler receipt consumption: a worker could append a forged worker_completed event directly,
 * so the handoff file is authoritative -- the event is only an index; a success receipt re-runs the hard git checks.
 * Scanning backwards: find the first valid "event + handoff" pair (later duplicate events that are forged,
 * malformed, inconsistent, or fail the hard checks must not shadow an earlier legitimate receipt);
 * all invalid -> null (treated as no receipt). Authoritative featureId binding: the completion event must share a
 * feature with that session's worker_started event (the handoff carries no featureId, so a legitimate session from another feature cannot be impersonated).
 */
function findCompletedReceipt(missionDir, workerSessionId, state) {
  const p = path.join(missionDir, 'progress_log.jsonl');
  if (!fs.existsSync(p)) return null;
  const lines = fs.readFileSync(p, 'utf8').split('\n').filter(Boolean);
  const handoffPath = path.join(missionDir, 'handoff', `${workerSessionId}.json`);
  const ws = lastWorkerStarted(missionDir, workerSessionId);
  if (!ws) return null; // no authoritative worker_started -> nothing to bind ownership to
  // Loop-invariant: resolved once, not re-read and re-hashed per candidate event.
  const reportOnly = featureIsReportOnly(missionDir, ws.featureId, state);
  for (let i = lines.length - 1; i >= 0; i--) {
    let ev;
    try {
      ev = JSON.parse(lines[i]);
    } catch {
      continue;
    }
    if (ev.event !== 'worker_completed' || ev.workerSessionId !== workerSessionId) continue;
    if (ev.featureId !== ws.featureId) continue; // the completion event must bind to this session's feature
    if (!fs.existsSync(handoffPath)) continue; // a forged event has no handoff -> invalid
    let receipt;
    try {
      receipt = JSON.parse(fs.readFileSync(handoffPath, 'utf8'));
    } catch {
      continue;
    }
    // Same schema contract as end-feature, including the plan's report-only exemption:
    // without it a valid commit-less receipt is discarded as no receipt at all.
    const { input, semantic } = receiptErrors(receipt, { reportOnly, acceptance: featureValidatorKind(missionDir, ws.featureId, state) === 'acceptance' });
    if (input.length || semantic.length) continue;
    // The event and the handoff must agree
    if (ev.successState !== receipt.successState) continue;
    if ((ev.failureClass || null) !== (receipt.failureClass || null)) continue;
    if ((ev.commitId || null) !== (receipt.commitId || null)) continue;
    if (receipt.successState === 'success' && !commitExempt(receipt, reportOnly)) {
      const feature = featureFromSnapshot(missionDir, ws.featureId);
      const out = verifyReceiptGit(receipt, state, missionDir, workerSessionId, feature);
      if (!out.ok) {
        // The receipt pins a commit, and settling re-runs the git checks against it. A rebase
        // between the two rewrites that commit, so the pinned sha is no longer on the branch --
        // and the merge gate is what asks for the rebase, answering `not_fast_forward` with
        // "rebase it and re-verify". Doing as told used to void the receipt.
        const moved = rebasedReceiptHead(receipt, state, missionDir, workerSessionId, feature, out.err);
        if (moved) {
          logEvent(missionDir, 'receipt_commit_moved', {
            featureId: ws.featureId, workerSessionId, from: receipt.commitId, to: moved,
            reason: 'branch rewritten after the receipt (a rebase, or `git commit --amend`)',
          });
          // Downstream records what actually exists. The handoff on disk still names the original,
          // and the event above is what reconciles the two.
          receipt = Object.assign({}, receipt, { commitId: moved });
        } else {
          // Discarding a receipt without a word is what made this unlearnable: the feature went
          // back to pending and the only event said `resume_requeue {stale_in_progress}`, which
          // tells the reader the worker never finished. It had finished.
          logEvent(missionDir, 'receipt_unverifiable', {
            featureId: ws.featureId, workerSessionId, commitId: receipt.commitId || null,
            code: (out.err && out.err.code) || 'unknown',
            message: (out.err && out.err.message ? String(out.err.message) : '').slice(0, 300),
          });
          continue; // hard check failed -> this pair is invalid
        }
      }
    }
    return { ev, receipt };
  }
  return null;
}

function lastPauseReason(missionDir, featureId) {
  const p = path.join(missionDir, 'progress_log.jsonl');
  if (!fs.existsSync(p)) return null;
  const lines = fs.readFileSync(p, 'utf8').split('\n').filter(Boolean);
  for (let i = lines.length - 1; i >= 0; i--) {
    try {
      const ev = JSON.parse(lines[i]);
      // Any event that recorded a pause reason for this feature, not just `feature_paused`: the
      // pre-spawn gate reports its failure as `verification_failed` and would otherwise never be
      // seen as recoverable.
      if (ev.featureId === featureId && ev.pauseReason) return ev.pauseReason;
    } catch { /* skip */ }
  }
  return null;
}

// ---------------------------------------------------------------------------
// gate subcommand: the orchestrator's independent re-run (red before green, t6)
// gate --cmd <shell cmd> [--expect fail|pass] [--target <name>] [--timeout <sec>]
// Emits a JSON summary (raw stdout is handed to the script to parse; no prose assertions); expectation not observed -> exit 1.
// ---------------------------------------------------------------------------

function cmdGate(args) {
  const { options } = parseArgs(args);
  const cmd = options['cmd'];
  const target = options['target'];
  const expect = options['expect'] || 'pass';
  const timeoutMs = (Number(options['timeout']) || 600) * 1000;
  const useJson = options['json'] === '1' || options['json'] === true || options['json'] !== undefined;
  if (!cmd || (expect !== 'fail' && expect !== 'pass')) {
    throw new InputError('usage: devflow.cjs gate --cmd <shell cmd> [--expect fail|pass] [--target <name>] [--json] [--timeout <sec>]');
  }
  if (useJson && (!target || target === '')) {
    throw new InputError('gate --json requires --target <test name>: verdict is target membership in failed[]/passed[]');
  }
  let r;
  try {
    r = spawnSync(cmd, { shell: true, encoding: 'utf8', timeout: timeoutMs, stdio: ['ignore', 'pipe', 'pipe'] });
  } catch (e) {
    throw new RejectError(`gate command failed to run: ${e.message}`, 'gate_failed');
  }
  const out = (r.stdout || '') + (r.stderr || '');
  const timedOut = r.error && r.error.killed;
  let report = null;
  if (useJson) {
    // Parse the test runner's JSON report: whole-output JSON or last-line JSON (mixed-log scenario)
    for (const candidate of [out, out.split('\n').filter(Boolean).pop()]) {
      if (!candidate) continue;
      try {
        const parsed = JSON.parse(candidate);
        if (parsed && typeof parsed === 'object') {
          report = parsed;
          break;
        }
      } catch { /* try next candidate */ }
    }
    if (!report) {
      throw new RejectError('gate --json: no JSON test report found in command output', 'gate_failed');
    }
    if (typeof report.collected !== 'number' || report.collected < 1 || !Array.isArray(report.failed) || !Array.isArray(report.passed)) {
      throw new RejectError(
        `gate --json: malformed report (require collected>0 and failed[]/passed[])`,
        'gate_failed'
      );
    }
  }
  const targetIn = (list) => (target ? list.includes(target) : true);
  let verdict;
  if (expect === 'fail') {
    // Red first: the command exits non-zero (or times out) and the target really failed
    if (useJson) {
      verdict = (r.status !== 0 && !timedOut) && targetIn(report.failed) ? 'red-observed' : 'red-not-observed';
    } else {
      verdict = (r.status !== 0 && !timedOut) && (!target || out.includes(target)) ? 'red-observed' : 'red-not-observed';
    }
  } else if (useJson) {
    verdict = r.status === 0 && !timedOut && report.collected > 0 && targetIn(report.passed) && !targetIn(report.failed) ? 'green-observed' : 'green-not-observed';
  } else {
    verdict = r.status === 0 && !timedOut && (!target || out.includes(target)) ? 'green-observed' : 'green-not-observed';
  }
  const summary = {
    command: cmd,
    expect,
    exitCode: r.status,
    signal: r.signal,
    timedOut,
    targetFound: useJson ? targetIn([...report.passed, ...report.failed]) : !target || out.includes(target),
    verdict,
    output: out.slice(-4000),
  };
  if (useJson) {
    summary.report = { collected: report.collected, passed: report.passed, failed: report.failed };
  }
  process.stdout.write(JSON.stringify(summary, null, 2) + '\n');
  if ((expect === 'fail' && verdict !== 'red-observed') || (expect === 'pass' && verdict !== 'green-observed')) {
    throw new RejectError(`gate ${expect} not observed (exit=${r.status}, timedOut=${timedOut}, verdict=${verdict})`, 'gate_failed');
  }
}
// ---------------------------------------------------------------------------

/**
 * reconcile-forge: run the crash reconcile/resume probe on demand (and re-run
 * the filing sweep for active tickets — idempotent, never duplicates). The probe
 * is keyed by the persisted prId, reads forge refs as the authority, converges
 * the three interrupt states and NEVER re-invokes `pr merge` (clause 7).
 */
function cmdReconcileForge(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const { state, featuresDoc } = loadMission(missionDir);
  if (state.forge !== true) {
    throw new RejectError(`mission '${state.sessionId}' was not proposed with --forge; nothing to reconcile`, 'not_forge_mission');
  }
  const filedBefore = new Set(featuresDoc.features.map((f) => f.issueId).filter(Boolean));
  forgeFileActiveTickets(missionDir, state, featuresDoc);
  const filed = featuresDoc.features.filter((f) => f.issueId && !filedBefore.has(f.issueId)).length;
  const converged = [];
  for (const f of featuresDoc.features) {
    if (f.prId && f.status !== 'cancelled' && f.status !== 'failed') {
      const r = forgeReconcileProbe(missionDir, state, featuresDoc, f);
      if (r) converged.push(f.id);
    }
  }
  process.stdout.write(`reconcile-forge ${state.sessionId}: filed ${filed}, converged ${converged.length ? converged.join(',') : 'nothing'}\n`);
}

/**
 * close-forge-ticket: the orchestrator-side close for a proof (report-only)
 * ticket, carrying the evidence pointer and overall status — exactly one issue
 * comment + close, idempotent (clause 2). Validators never run forge writes;
 * this is the adapter doing it for them.
 */
function cmdCloseForgeTicket(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  if (!featureId) {
    throw new InputError('usage: devflow.cjs close-forge-ticket --mission-dir <dir> --feature <id> [--evidence <pointer>] [--status <status>]');
  }
  const { state, featuresDoc } = loadMission(missionDir);
  if (state.forge !== true) {
    throw new RejectError(`mission '${state.sessionId}' was not proposed with --forge; nothing to close`, 'not_forge_mission');
  }
  const feature = featuresDoc.features.find((f) => f.id === String(featureId));
  if (!feature) throw new RejectError(`unknown feature '${featureId}' in this mission's plan`, 'unknown_feature');
  const status = options['status'] || 'success';
  const evidence = options['evidence'] || null;
  try {
    forgeCloseTicketIssue(missionDir, state, feature, {
      marker: `forge-proof-close: ${feature.id}`,
      body: `status: ${status}; evidence: ${evidence || 'receipt'}`,
    });
  } catch (e) {
    if (e instanceof RejectError) throw e;
    throw forgeBridgeError(String(e.message || e));
  }
  process.stdout.write(`closed ticket issue #${feature.issueId} for ${feature.id} (status: ${status})\n`);
}

/**
 * Close one ticket's Issue on the CANCEL / retry-limit path, the way clause 2
 * names for the non-code exits: one marker comment (the abandon reason) +
 * close, exactly-one-comment idempotence mirroring forgeCloseTicketIssue.
 * Called from applyReceipt's retry-limit branch and cmdCancel. The mission is
 * already terminal (failed/cancelled) — a bridge failure here throws and the
 * caller's pause keeps the close retryable.
 */
function forgeAbandonTicketIssue(missionDir, state, feature, reason) {
  if (!feature.issueId) return;
  forgeCloseTicketIssue(missionDir, state, feature, {
    marker: `forge-abandon-close: ${feature.id}`,
    body: `reason: ${reason}`,
  });
}


// ---------------------------------------------------------------------------
// validation-check subcommand: enforce the acceptance rules in
// references/validation-protocol.md §5 on a validation report.
// validation-check <report.json> --contract <contract> --assigned <ids> [--evidence-root <dir>]
// A verdict without evidence is an opinion: `pass` requires an artifact that
// resolves on disk, and any assertion left unproven fails the report (exit 1).
// ---------------------------------------------------------------------------

const VERDICT_STATES = ['pass', 'fail', 'blocked', 'skipped'];

/** Structural check: anything here means the report is unreadable, not merely wrong (exit 2). */
function validationReportInputErrors(report) {
  const errs = [];
  if (typeof report !== 'object' || report === null || Array.isArray(report)) {
    return [{ kind: 'report-not-object' }];
  }
  if (!Array.isArray(report.assertions)) {
    return [{ kind: 'missing-assertions-array' }];
  }
  report.assertions.forEach((a, i) => {
    if (typeof a !== 'object' || a === null || Array.isArray(a)) {
      errs.push({ kind: 'entry-not-object', at: `assertions[${i}]` });
      return;
    }
    if (typeof a.id !== 'string' || a.id === '') errs.push({ kind: 'entry-missing-id', at: `assertions[${i}]` });
    if (typeof a.status !== 'string' || a.status === '') errs.push({ kind: 'entry-missing-status', at: a.id || `assertions[${i}]` });
  });
  return errs;
}

/**
 * Controlled evidence tokens (Contract 2). Validation Plan `Evidence:` text is free prose, but the
 * recognised tokens below carry force: a token in the contract evidence means the report's `pass`
 * must carry the matching key, or it is an opinion dressed as proof. Tokens the text does not use
 * stay free -- this is an allowlist, not a vocabulary police.
 *
 * The table is token -> { report evidence key, shape predicate }: the key alone is not enough --
 * a `pass` whose `crossChecks` is `[{}]` has the key and none of the evidence. `artifacts` is
 * already enforced unconditionally by `pass-without-evidence`, so tokens mapping to it are never
 * re-checked here (that would double-report one missing key as two distinct errors).
 */
const fieldOk = (v) => (typeof v === 'string' ? v.trim() !== '' : v !== undefined && v !== null);
/** An evidence array must be non-empty AND carry the documented element shape -- `[{}]` is not a cross-check. */
const arrOf = (fields) => (x) => Array.isArray(x) && x.length > 0 && x.every((e) =>
  typeof e === 'string' ? e.trim() !== ''
    : e !== null && typeof e === 'object' && !Array.isArray(e) && fields.every((k) => fieldOk(e[k])));

/**
 * The one table: token -> { report key, shape predicate }. Keeping the key and its shape check in
 * one structure is what stops a future token from being added to the vocabulary without its
 * enforcement -- a separate predicate list with a default-true branch fails open on exactly that.
 */
const EVIDENCE_TOKENS = {
  screenshot: { key: 'artifacts', ok: (ev) => Array.isArray(ev.artifacts) && ev.artifacts.length > 0 },
  terminal: { key: 'artifacts', ok: (ev) => Array.isArray(ev.artifacts) && ev.artifacts.length > 0 },
  stdout: { key: 'artifacts', ok: (ev) => Array.isArray(ev.artifacts) && ev.artifacts.length > 0 },
  console: { key: 'consoleErrors', ok: (ev) => typeof ev.consoleErrors === 'string' && ev.consoleErrors !== '' },
  network: { key: 'network', ok: (ev) => typeof ev.network === 'string' && ev.network.trim() !== '' },
  'db-query': { key: 'dbQueries', ok: (ev) => arrOf(['query', 'observed'])(ev.dbQueries) },
  'state-snapshot': { key: 'stateSnapshots', ok: (ev) => arrOf(['before', 'after'])(ev.stateSnapshots) },
  'cross-layer': { key: 'crossChecks', ok: (ev) => arrOf(['layers', 'expected', 'observed'])(ev.crossChecks) },
};

/**
 * Every controlled token named in an evidence string. The boundary must reject a hyphen on both
 * sides: `\\b` treats `-` as a boundary, so "db-query-log" or "sub-network" would falsely match.
 */
function controlledEvidenceTokens(text) {
  const found = new Set();
  for (const token of Object.keys(EVIDENCE_TOKENS)) {
    if (new RegExp(`(?<![\\w-])${token}(?![\\w-])`).test(String(text || ''))) found.add(token);
  }
  return found;
}

/**
 * Acceptance rules. Every violation here is a semantic rejection (exit 1).
 */
function validationReportErrors(report, assigned, contractIds, evidenceRoot, assertionDocs) {
  const errs = [];
  const assignedSet = new Set(assigned);

  // The assignment itself must be grounded in the contract, which is the assertion authority.
  for (const id of assigned) {
    if (!contractIds.has(id)) errs.push({ kind: 'assigned-not-in-contract', at: id });
  }

  const seen = new Map();
  for (const a of report.assertions) {
    seen.set(a.id, (seen.get(a.id) || 0) + 1);
  }
  // Rule 1: every assigned assertion appears exactly once.
  for (const id of assigned) {
    const n = seen.get(id) || 0;
    if (n === 0) errs.push({ kind: 'missing-verdict', at: id });
    else if (n > 1) errs.push({ kind: 'duplicate-verdict', at: id, detail: `${n} entries` });
  }
  // Rule 2: a validator must not widen its own assignment.
  for (const id of seen.keys()) {
    if (!assignedSet.has(id)) errs.push({ kind: 'unassigned-assertion', at: id });
  }

  const counts = { pass: 0, fail: 0, blocked: 0, skipped: 0 };
  let artifactsChecked = 0;
  for (const a of report.assertions) {
    if (!VERDICT_STATES.includes(a.status)) {
      errs.push({ kind: 'bad-status', at: a.id, detail: a.status });
      continue;
    }
    counts[a.status] += 1;
    const ev = (a.evidence && typeof a.evidence === 'object' && !Array.isArray(a.evidence)) ? a.evidence : {};
    const artifacts = Array.isArray(ev.artifacts) ? ev.artifacts.filter((x) => typeof x === 'string' && x !== '') : [];
    const issue = typeof a.issues === 'string' ? a.issues.trim() : '';

    // Rule 3: a pass with no artifact is an opinion; this is the load-bearing rule.
    if (a.status === 'pass' && artifacts.length === 0) {
      errs.push({ kind: 'pass-without-evidence', at: a.id });
    }
    // Rule 3 (Contract 2): a pass whose contract evidence names a controlled token must carry the
    // matching key. The token list is read from the CONTRACT, not from the worker's report -- a
    // worker that knew which key to fabricate would not be the thing being audited. A chain
    // assertion (the contract carries a non-empty `chain`) additionally demands `crossChecks`
    // regardless of whether the evidence prose wrote the token, because the whole point of a chain
    // is the layer-to-layer comparison.
    if (a.status === 'pass') {
      const doc = (assertionDocs instanceof Map ? assertionDocs : new Map()).get(a.id);
      const tokens = controlledEvidenceTokens(doc && doc.evidence);
      if (doc && Array.isArray(doc.chain) && doc.chain.length) tokens.add('cross-layer');
      for (const token of tokens) {
        const rule = EVIDENCE_TOKENS[token];
        if (rule.key === 'artifacts') continue; // covered by pass-without-evidence
        if (!rule.ok(ev)) {
          errs.push({ kind: 'pass-missing-evidence-type', at: a.id, detail: token });
        }
      }
    }
    // Rule 4: negative verdicts must name what went wrong.
    if (a.status === 'fail' && issue === '') errs.push({ kind: 'fail-without-issue', at: a.id });
    if (a.status === 'blocked' && issue === '') errs.push({ kind: 'blocked-without-issue', at: a.id });
    if (a.status === 'skipped' && issue === '') errs.push({ kind: 'skipped-without-justification', at: a.id });

    // Rule 5: a path that does not resolve reads as proof while proving nothing.
    for (const rel of artifacts) {
      artifactsChecked += 1;
      const p = path.isAbsolute(rel) ? rel : path.join(evidenceRoot, rel);
      if (!fs.existsSync(p)) errs.push({ kind: 'evidence-missing-on-disk', at: a.id, detail: `${rel} -> ${p}` });
    }
  }

  // Only `pass` counts as proven — blocked/skipped are the statuses that get rounded up.
  // This is the report's OUTCOME, not its acceptance: a well-formed report that honestly says
  // "the product is broken" is a good report. Collapsing the two leaves the orchestrator unable
  // to tell a broken product from a broken validator, and makes `skipped` a status no waiver
  // could ever use. Callers that need every assertion proven ask for it with --require-proven.
  const unproven = assigned.filter((id) => {
    const entry = report.assertions.find((a) => a.id === id);
    return !entry || entry.status !== 'pass';
  });

  return { errs, counts, artifactsChecked, unproven };
}

// ---------------------------------------------------------------------------
// assertion ledger (t4): per-assertion status derived from the log.
// validation-state.json is a projection for humans -- deriving from the log is what makes
// "only the scheduler writes it" true, because nothing reads the file as authority.
// ---------------------------------------------------------------------------

const VERDICT_TO_STATUS = { pass: 'passed', fail: 'failed', blocked: 'blocked', skipped: 'skipped' };

function assertionLedger(assertionIds, events) {
  const assertions = {};
  for (const id of assertionIds) assertions[id] = { status: 'pending' };
  for (const e of events || []) {
    if (e.event !== 'assertion_verdict' || !assertions[e.assertionId]) continue;
    // A later verdict supersedes an earlier one: a re-run after a fix must be able to clear it.
    assertions[e.assertionId] = {
      status: VERDICT_TO_STATUS[e.verdict] || 'pending',
      validatedAtMilestone: e.milestone || null,
      evidence: e.evidence || '',
    };
  }
  // Waivers (assertion_waiver events) discharge an assertion that cannot be proven in this mission
  // -- e.g. a real-device hard gate blocked by a pre-existing product defect outside the spec's
  // change surface, explicitly handed to follow-up by the human. A waiver does NOT pretend the
  // assertion passed: the verdict stays on record. It only moves the assertion out of `unproven`
  // so the closing gate can fire. Every waiver carries a reason in the log; the projection shows it.
  const waived = {};
  for (const e of events || []) {
    if (e.event === 'assertion_waiver' && e.assertionId && assertions[e.assertionId]) {
      waived[e.assertionId] = { reason: e.reason || '', at: e.at || null };
    }
  }
  const ids = Object.keys(assertions);
  const unproven = ids.filter((id) => assertions[id].status !== 'passed' && !waived[id]);
  return {
    assertions,
    total: ids.length,
    proven: ids.length - ids.filter((id) => assertions[id].status !== 'passed').length,
    unproven,
    waived,
  };
}

/**
 * Seal every milestone whose work is settled and whose assertions are all proven.
 *
 * Sealing was documented, guarded against, and never actually emitted -- the event existed and
 * only a test ever wrote one. A guard nobody can trip is a guard that is not there, and "no
 * feature is ever appended to a sealed milestone" was consequently true by accident.
 *
 * Called wherever the two inputs can change: a feature completing, and a verdict being recorded.
 */
function sealCompletedMilestones(missionDir, features, state) {
  const events = readEvents(missionDir);
  const ledger = assertionLedger([...contractAssertions(state.contractPath)], events);
  let sealed = false;
  for (const ms of milestoneStates(features, events)) {
    if (ms.sealed || !ms.complete) continue;
    const claimed = new Set();
    for (const f of features) {
      if (f.milestone !== ms.milestone) continue;
      for (const id of [...(f.fulfills || []), ...(f.proves || [])]) claimed.add(id);
    }
    // A milestone that claims nothing has nothing to prove and is not sealed by proof: sealing it
    // would freeze a milestone no validator ever looked at.
    if (!claimed.size) continue;
    if ([...claimed].every((id) => ledger.assertions[id] && ledger.assertions[id].status === 'passed')) {
      logEvent(missionDir, 'milestone_sealed', { milestone: ms.milestone, assertions: [...claimed] });
      sealed = true;
    }
  }
  return sealed;
}

function writeLedgerProjection(missionDir, ledger) {
  fs.writeFileSync(path.join(missionDir, 'validation-state.json'), JSON.stringify({ assertions: ledger.assertions }, null, 2) + '\n');
}

function missionLedger(missionDir, state) {
  const ids = [...contractAssertions(state.contractPath)];
  return assertionLedger(ids, readEvents(missionDir));
}

/**
 * Milestones whose last validator outcome was a failure. Derived from the log, because resume
 * rewrites feature statuses before the charge point and a status read there would see the
 * recovered value. A crashed validator is requeued without a failure event, so infrastructure
 * that flaked is never mistaken for work that cannot converge.
 */
function milestonesInRepair(events) {
  // Per validator, not per milestone: one validator completing says nothing about the other,
  // and collapsing them let a passing assertions-run clear a still-red scrutiny gate.
  const failing = new Map(); // validator featureId -> boolean
  const milestoneOf = new Map(); // validator featureId -> milestone
  for (const e of events) {
    if (e.event === 'milestone_validation_triggered') {
      for (const id of e.featureIds || []) milestoneOf.set(id, e.milestone);
    }
    if (!milestoneOf.has(e.featureId)) continue;
    if (e.event === 'verification_failed') failing.set(e.featureId, true);
    // By reason, not by event name -- the gate reports itself as `verification_failed`, so keying
    // this on `feature_paused` made it a line that reads as coverage and matched nothing.
    if (e.pauseReason === 'gate_failed') failing.set(e.featureId, true);
    if (e.event === 'feature_completed') failing.set(e.featureId, false);
  }
  const out = new Set();
  for (const [featureId, isFailing] of failing) {
    if (isFailing) out.add(milestoneOf.get(featureId));
  }
  return out;
}

/**
 * Charge one repair round per round entered while a milestone stands in repair, regardless of how
 * many validator kinds failed, and report the milestones that have exhausted their budget.
 *
 * "A round was entered" is derived from the log, not taken from the caller. `--start-round` is
 * documented as once per loop entry, but nothing stopped an orchestrator from passing it on every
 * poll and burning a budget nobody spent -- a prose guard, which is the exact thing this project
 * exists to replace. So a second charge requires that some worker started since the milestone's
 * last charge: the budget counts repair attempts, and no worker means no attempt.
 *
 * The conditional is a strict subset of the unconditional charge -- it can only ever charge less --
 * so no legitimate exhaustion is delayed. It also stops a round that crashed before starting any
 * worker from consuming a round on restart, which is the same reasoning `milestonesInRepair` uses
 * to keep flaked infrastructure out of the budget.
 *
 * "Something happened" is any event but the charge itself -- a round that re-ran a red gate logs
 * its verification_step even though it never reaches a worker, so requiring a spawned worker would
 * exempt exactly the milestone that cannot converge. Progress is mission-wide rather than scoped to
 * the milestone: scoping it is tighter, but it redefines what the budget counts, so it is recorded
 * as a refinement rather than taken silently.
 */
/**
 * Did this event happen because work was dispatched, or because someone looked?
 *
 * The repair budget bounds how many ROUNDS a milestone gets, and a round means an attempt was made.
 * The predicate that decides "has anything happened since the last charge" used to count any logged
 * event at all -- which was correct while the loop was hand-driven and looking cost a command, and
 * wrong the moment polling became the primary mode. A poll runs the merge gate; the merge gate logs;
 * so a milestone in repair with a branch parked at the gate awaiting a rebase would be charged a
 * repair round for every poll, and exhaust a two-round budget by being asked how it was doing.
 *
 * The list is what a poll produces BY ITSELF, and it is deliberately an exclusion list rather than a
 * list of what counts: anything new is "something happened" by default, which keeps the conservative
 * reading. `verification_failed` is absent on purpose -- a red check at the gate requeues the
 * feature, and a state transition is exactly what a round is for.
 */
function isPollNoise(e) {
  if (e.event === 'milestone_repair_round') return true;
  // The merge gate re-running its post-receipt verification on a branch that is still parked.
  if (e.event === 'verification_step' && e.phase === 'post-receipt') return true;
  // The gate refusing that same branch again, for the same reason, on every look.
  if (e.event === 'merge_rejected' || e.event === 'merge_blocked') return true;
  return false;
}

function chargeRepairRounds(missionDir, features, state) {
  const events = readEvents(missionDir);
  const inRepair = milestonesInRepair(events);
  if (!inRepair.size) return [];
  const budget = typeof state.repairBudget === 'number' ? state.repairBudget : 2;
  const exhausted = [];
  for (const milestone of inRepair) {
    // Position in the log, never timestamps: two events in the same second are indistinguishable.
    let prior = 0, lastCharge = -1;
    for (let i = 0; i < events.length; i += 1) {
      if (events[i].event === 'milestone_repair_round' && events[i].milestone === milestone) {
        prior += 1;
        lastCharge = i;
      }
    }
    const somethingHappened = events.slice(lastCharge + 1).some((e) => !isPollNoise(e));
    if (prior > 0 && !somethingHappened) continue;
    const round = prior + 1;
    logEvent(missionDir, 'milestone_repair_round', { milestone, round, budget });
    if (round > budget) exhausted.push({ milestone, round });
  }
  return exhausted;
}


// ---------------------------------------------------------------------------
// brief subcommand: the orchestrator's own brief
//
// Every role devflow spawns is handed a rendered, self-contained brief -- worker, reviewer,
// validator. The role that DRIVES the loop was handed nothing, and it is the one role whose context
// reliably dies mid-mission. So its successor reconstructed the mission by reading files, and in
// three separate projects a human ended up hand-writing a handoff document to bridge the gap: state
// tables copied out of runtime.json, in-flight branch shapes, rulings, and rules learned by losing
// work.
//
// This renders the half that needs nobody's cooperation. Everything here is derived from what the
// mission already holds, so it is worth reading on a mission where nobody did anything extra -- and
// gets better when the orchestrator records a decision (`log decision`) or a constraint
// (`log constraint`). That order matters: `ops.yaml` was worth nothing until someone wrote it, and
// nothing ever told anyone to, which is how it ended up read in one place and written in none.
//
// Two properties this command must keep, both tested:
//   READ-ONLY -- it settles no receipt, charges no budget, injects nothing, recovers nothing. The
//     one command that answered "where are we" was `next-feature`, which advances the state machine:
//     looking cost you a move.
//   TOLERANT -- a mission whose plan is missing or tampered still gets a brief saying so. A
//     diagnostic that refuses to run when something is wrong is useless exactly when it is needed.
// ---------------------------------------------------------------------------

/** Decisions and constraints the orchestrator put on the record, newest last. */
function recordedJudgment(missionDir) {
  const out = [];
  for (const e of readEvents(missionDir)) {
    if (e.event !== 'decision' && e.event !== 'constraint') continue;
    out.push({ kind: e.event, text: e.text || e.note || '', because: e.because || null, ts: e.ts });
  }
  return out;
}

function cmdBrief(args) {
  const { options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  if (!missionDir) throw new InputError('usage: devflow.cjs brief --mission-dir <dir> [--json]');
  if (!fs.existsSync(statePath(missionDir))) {
    throw new InputError(`no mission at ${missionDir} (no state.json)`);
  }
  const state = readState(missionDir);
  const events = readEvents(missionDir);

  const problems = [];
  let features = [];
  try {
    const doc = readMissionFeatures(missionDir, state);
    features = doc.features;
    const digest = planDigest(doc.features, doc, injectedFeatureIds(events));
    if (digest !== state.planDigest) {
      problems.push(`the plan no longer matches the one approved at propose (planDigest mismatch): ` +
        `${state.featuresSourcePath || 'features.json'} was edited outside 'amend'. The mission will refuse to run until it is restored.`);
    }
  } catch (e) {
    problems.push(`the plan could not be read: ${e.message}`);
  }
  if (state.contractPath && !fs.existsSync(state.contractPath)) {
    problems.push(`the contract is missing: ${state.contractPath}`);
  }

  const byStatus = {};
  for (const f of features) (byStatus[f.status] = byStatus[f.status] || []).push(f);
  const inFlight = (byStatus.in_progress || []).map((f) => ({
    id: f.id, workerSessionId: f.currentWorkerSessionId, branch: f.branch, worktree: f.worktree,
    attempt: countAttempts(missionDir, f.id, state) + 1,
  }));
  const awaitingMerge = (byStatus.in_review || []).map((f) => ({
    id: f.id, branch: f.branch, worktree: f.worktree,
    review: (() => { const v = lastReviewVerdict(missionDir, f.id); return v ? `${v.status} (round ${v.round})` : 'none recorded'; })(),
  }));
  const ledger = state.contractPath && fs.existsSync(state.contractPath)
    ? missionLedger(missionDir, state) : { total: 0, proven: 0, unproven: [], assertions: {} };
  const milestones = milestoneStates(features, events).map((m) => {
    // What a milestone still owes, computed rather than inferred from the seal flag. Reading
    // `sealed` alone said "awaiting proof" about a finished mission with 10/10 assertions proven --
    // because that mission predates the sealing code and no seal event was ever written. A brief
    // that contradicts its own Proof section three lines later is worse than one that says less.
    const claimed = new Set();
    for (const f of features) {
      if (f.milestone !== m.milestone) continue;
      for (const id of [...(f.fulfills || []), ...(f.proves || [])]) claimed.add(id);
    }
    const unproven = [...claimed].filter((id) => {
      const a = (ledger.assertions || {})[id];
      return !a || a.status !== 'passed';
    });
    return {
      milestone: m.milestone, complete: m.complete, sealed: m.sealed,
      done: m.implementationFeatures.length - m.open.length, total: m.implementationFeatures.length,
      open: m.open, claims: claimed.size, unproven,
    };
  });

  const out = {
    mission: state.missionId || state.sessionId,
    state: state.state || 'orchestrator_turn',
    pauseReason: state.state === 'paused' ? state.pauseReason || null : null,
    repo: state.repoRoot,
    integrationBranch: state.expectedBranch,
    plan: state.featuresSourcePath || path.join(missionDir, 'features.json'),
    problems,
    counts: Object.fromEntries(Object.entries(byStatus).map(([k, v]) => [k, v.length])),
    milestones,
    inFlight,
    awaitingMerge,
    eligible: eligibleFeatures(features).map((f) => f.id),
    assertions: { proven: ledger.proven, total: ledger.total, unproven: ledger.unproven },
    openFindings: openReviewFindings(missionDir, features),
    salvaged: salvagedBranches(missionDir, state),
    judgment: recordedJudgment(missionDir),
    skillFeedback: skillFeedbackReported(missionDir),
  };

  if (options['json'] === true || options['json'] === 'true') {
    process.stdout.write(JSON.stringify(out, null, 2) + '\n');
    return;
  }

  const L = [];
  L.push(`# Mission ${out.mission} — ${out.state}${out.pauseReason ? ` (${out.pauseReason})` : ''}`);
  L.push(`${out.repo}, integration branch ${out.integrationBranch}`);
  L.push(`plan: ${out.plan}`);
  if (problems.length) {
    L.push('');
    L.push('## Blocked before anything else');
    for (const p of problems) L.push(`- ${p}`);
  }
  L.push('');
  L.push('## Where the work stands');
  L.push(Object.entries(out.counts).map(([k, v]) => `${v} ${k}`).join(', ') || 'no features');
  for (const m of milestones) {
    let note = '';
    if (m.sealed) note = ' (sealed)';
    else if (m.complete && m.unproven.length) note = ` (work settled, unproven: ${m.unproven.join(', ')})`;
    else if (m.complete) note = m.claims ? ' (work settled, proof complete)' : ' (work settled, nothing to prove)';
    L.push(`- ${m.milestone}: ${m.done}/${m.total}${note}${m.open.length ? ` — open: ${m.open.join(', ')}` : ''}`);
  }
  if (inFlight.length) {
    L.push('');
    L.push('## In flight');
    for (const f of inFlight) {
      L.push(`- ${f.id} — attempt ${f.attempt}, session ${f.workerSessionId || '(none)'}`);
      L.push(`  branch ${f.branch || '(none)'} in ${f.worktree || out.repo}`);
    }
  }
  if (awaitingMerge.length) {
    L.push('');
    L.push('## Awaiting merge');
    for (const f of awaitingMerge) L.push(`- ${f.id} — review: ${f.review}; branch ${f.branch}`);
  }
  L.push('');
  L.push('## Proof');
  L.push(out.assertions.total
    ? `${out.assertions.proven}/${out.assertions.total} assertions proven` +
      (out.assertions.unproven.length ? `; unproven: ${out.assertions.unproven.join(', ')}` : '')
    : 'no contract assertions to prove');
  if (out.openFindings.length) {
    L.push('');
    L.push('## Review findings still open');
    for (const f of out.openFindings) L.push(`- ${f.itemId} (${f.featureId}, round ${f.round})`);
  }
  if (out.salvaged.length) {
    L.push('');
    L.push('## Preserved work waiting on a decision');
    for (const b of out.salvaged) L.push(`- ${b.branch} — from ${b.featureId}, head ${(b.head || '').slice(0, 8)}`);
  }
  if (out.judgment.length) {
    L.push('');
    L.push('## On the record');
    for (const j of out.judgment) L.push(`- [${j.kind}] ${j.text}${j.because ? ` — ${j.because}` : ''}`);
  }
  if (out.skillFeedback.length) {
    L.push('');
    L.push('## What workers said about the procedure');
    for (const f of out.skillFeedback) {
      if (f.kind === 'deviation') {
        L.push(`- ${f.featureId || f.sessionId} departed from "${f.step}": ${f.insteadOf} — ${f.why}`);
      } else {
        L.push(`- ${f.featureId || f.sessionId} suggests: ${f.text}`);
      }
    }
    L.push('');
    L.push('These are reports, not complaints. A step several workers had to work around is a step that asks for something the task cannot give.');
  }
  L.push('');
  L.push('## What the loop would do next');
  if (problems.length) L.push('resolve the blockers above; nothing else can proceed.');
  else if (out.pauseReason) L.push(`the mission is paused (${out.pauseReason}) and will not hand out work until that is cleared.`);
  else if (out.eligible.length) L.push(`claim one of: ${out.eligible.join(', ')}`);
  else if (inFlight.length) L.push('wait for the receipts in flight, then run next-feature to settle them.');
  else if (awaitingMerge.length) L.push('record the missing reviews, then merge-feature.');
  else if (out.assertions.unproven.length) L.push('every feature is settled; the unproven assertions above are what remain.');
  else L.push('nothing is outstanding — run next-feature to close the mission.');
  L.push('');
  L.push('Nothing was changed by printing this.');
  process.stdout.write(L.join('\n') + '\n');
}


// ---------------------------------------------------------------------------
// review-brief subcommand: the brief for the merge gate's reviewer
//
// `roles/reviewer.md` opens by saying "Your brief names the feature, its branch and worktree, and
// where to write your report." Nothing rendered that brief. `claim --brief` fires only for a
// pending feature, and a feature at the merge gate is `in_review` -- so the loop's MAIN review path
// left the orchestrator to hand-assemble eleven fields from memory, every round.
//
// That cost was measured, not guessed. Assembling it by hand in a real mission, I gave the
// reviewers per-feature ids like `SLUG-001`; `record-review` enforces `F-<n>`, which appears in
// reviewer.md only inside a JSON example and is stated as a rule nowhere. Both reports bounced. A
// rendered brief carries the constraint, which is the whole reason to render one.
// ---------------------------------------------------------------------------

/** Review ids already spent in this mission, so a new round starts somewhere free. */
function takenItemIds(missionDir) {
  const ids = new Set();
  for (const e of reviewEvents(missionDir)) {
    if (e.event === 'review_recorded') for (const id of e.itemIds || []) ids.add(id);
  }
  return [...ids].sort();
}

function cmdReviewBrief(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  if (!featureId) throw new InputError('usage: devflow.cjs review-brief --mission-dir <dir> --feature <id> [--json]');
  const built = buildReviewBrief(missionDir, featureId);
  if (options['json'] === true || options['json'] === 'true') {
    process.stdout.write(JSON.stringify(built, null, 2) + '\n');
    return;
  }
  // No mkdir here. Rendering a brief renders a brief: creating the destination made the text mode
  // touch the mission directory while --json did not, which is one fact with two behaviours.
  process.stdout.write(built.brief + '\n');
}

/** The reviewer's brief, assembled. Shared by `review-brief` and by `wave`, which renders the same
 * round without a second implementation of what a round is. */
function buildReviewBrief(missionDir, featureId) {
  const state = readState(missionDir);
  const featuresDoc = readMissionFeatures(missionDir, state);
  const feature = featuresDoc.features.find((f) => f.id === featureId);
  if (!feature) throw new RejectError(`feature not found: ${featureId}`, 'feature_not_found');
  if (!feature.branch) {
    throw new RejectError(
      `feature '${featureId}' has no branch to review (status ${feature.status}); a review happens at the merge gate, ` +
        `after a success receipt has moved the feature to in_review`,
      'nothing_to_review'
    );
  }

  const events = readEvents(missionDir);
  const prior = reviewEvents(missionDir, featureId);
  const round = prior.length + 1;
  const last = prior[prior.length - 1] || null;
  const sessionId = (feature.workerSessionIds || []).slice(-1)[0] || null;
  const baseline = sessionId ? lastBaseline(missionDir, sessionId) : null;
  const receiptPath = sessionId ? path.join(missionDir, 'handoff', `${sessionId}.json`) : null;
  const reportPath = path.join(missionDir, 'reviews', `${featureId}-r${round}.json`);
  const taken = takenItemIds(missionDir);
  const nextFree = `F-${String(taken.reduce((m, id) => Math.max(m, Number(id.slice(2)) || 0), 0) + 1).padStart(3, '0')}`;
  const proceduresPath = path.resolve(__dirname, '..', 'references', 'roles', 'reviewer.md');
  const assertions = assertionTextFor(state, feature, featuresDoc.features);

  const L = [];
  L.push(`# Review brief — feature ${feature.id}${feature.milestone ? ` (milestone: ${feature.milestone})` : ''}, round ${round}`);
  L.push('');
  L.push(`**Read \`${proceduresPath}\` first** — it is the procedure you are held to (what to read and in what order, what makes a finding, what is not yours to fix). This brief carries only what is specific to this review.`);
  L.push('');
  L.push('You are reviewing one feature\'s diff on its branch, before it merges. You did not write it and you will not fix it.');
  L.push('');
  L.push('## What the feature was supposed to deliver');
  L.push('');
  L.push(feature.description || '(no description — report this rather than guessing)');
  L.push('');
  L.push('### Done means');
  L.push('');
  L.push((feature.expectedBehavior || []).map((x) => `- ${x}`).join('\n') || '- (nothing declared)');
  L.push('');
  L.push(`- Paths it was admitted to write, and only these: ${(feature.touches || []).length ? feature.touches.map((t) => `\`${t}\``).join(', ') : '(none declared — anything outside the description is out of scope)'}`);
  if ((feature.dependsOn || []).length) L.push(`- Depends on: ${feature.dependsOn.join(', ')} (already merged)`);
  L.push(renderAssertionBlock('Assertions this feature must be capable of satisfying', assertions.fulfills));
  L.push('## Where');
  L.push('');
  L.push(`- **Worktree:** \`${feature.worktree || state.workingDirectory}\``);
  L.push(`- **Branch:** \`${feature.branch}\``);
  if (baseline) {
    L.push(`- **The diff to review:** \`git -C ${feature.worktree || state.repoRoot} diff ${baseline}...HEAD\` (\`--stat\` first, for its shape)`);
  } else {
    L.push(`- **The diff to review:** \`git -C ${feature.worktree || state.repoRoot} diff ${state.expectedBranch}...${feature.branch}\``);
  }
  if (receiptPath) L.push(`- **The worker's receipt:** \`${receiptPath}\``);
  L.push(`- **Plan and contract:** \`${path.dirname(state.contractPath || '')}\``);
  L.push('');
  // Conditional, because the promise is. `runVerificationSteps` returns `{ok: true, results: []}`
  // when a feature declares no `verification`, so for a ticket that never wrote a `- Verification:`
  // line the merge gate runs nothing at all -- and telling the reviewer not to run anything either
  // leaves the change with no execution behind it whatsoever, while sounding like the opposite.
  const steps = (feature.verification && feature.verification.steps) || [];
  if (steps.length) {
    L.push('Do not run the test suite — the scheduler re-runs this feature\'s own verification at the merge gate, after you:');
    L.push('');
    for (const st of steps) L.push(`- \`${st.command}\` (expected exit: ${st.expectedExit || 'zero'})`);
    L.push('');
    L.push('You read.');
  } else {
    L.push('**This feature declares no verification steps, so the merge gate will run nothing.** Reading is still your job and fixing is still not — but say so in your report: a change with no executable check behind it is a finding about the plan, not only about the diff.');
  }
  L.push('');

  if (last) {
    L.push(`## This is round ${round}: read the previous review first`);
    L.push('');
    L.push(`- Previous report: \`${last.reportPath || '(path not recorded)'}\` — verdict ${last.status}, ${last.blocking} blocking`);
    if ((last.itemIds || []).length) L.push(`- It raised: ${last.itemIds.join(', ')}`);
    L.push('');
    L.push('**Judge one thing above all: does the fix address that failure, or something adjacent?** Keep an item\'s original id if the problem survives — that is the signal a third round would be wasted and the orchestrator should change approach (SKILL.md §0.5 case 3) instead of sending another fix worker.');
    L.push('');
  }

  L.push('## Your report');
  L.push('');
  L.push(`Write JSON to exactly this path (create the directory if it is not there yet): \`${reportPath}\``);
  L.push('');
  L.push(`**\`itemId\` must match \`F-<n>\`** — enforced by the recorder, not a style preference; a report using any other shape is refused outright.`);
  L.push(taken.length
    ? `Already used in this mission: ${taken.join(', ')}. Start at \`${nextFree}\` for anything new, and **reuse an id unchanged** when a finding from an earlier round is still there.`
    : `Nothing has been used yet: start at \`F-001\`.`);
  L.push('');
  L.push('`severity` is `blocking` or `non_blocking`, and it is the field that decides: the recorder derives the verdict from your findings, not from any summary line you write. An empty `findings` array is how a clean review says so — say it explicitly.');
  L.push('');
  L.push('The orchestrator records it with:');
  L.push('');
  L.push('```bash');
  L.push(`node ${path.resolve(__dirname, 'devflow.cjs')} record-review --mission-dir ${missionDir} --feature ${featureId} --report ${reportPath} --round ${round}`);
  L.push('```');
  L.push('');
  const brief = L.join('\n');
  return {
    featureId, round, branch: feature.branch, worktree: feature.worktree,
    baseline, receiptPath, reportPath, takenItemIds: taken, nextItemId: nextFree, brief,
  };
}


// ---------------------------------------------------------------------------
// fix-brief subcommand: the brief for the receipt-less round after a refused merge
//
// Two rounds share one shape: a blocking review sends a fresh worker back into the feature's own
// worktree, and `not_fast_forward` sends one back to rebase. Neither can `claim` (the feature is
// `in_review`, not pending) and neither submits a receipt, so neither had a renderer -- leaving the
// orchestrator to assemble worktree, branch, the findings by itemId, the re-verify command and the
// "commit here, no end-feature, never --no-ff" rules from memory, on the paths the loop walks most.
// Under concurrency every second branch of every wave takes the rebase path.
//
// The measured cost of hand-assembly is already recorded above `review-brief`: ids the recorder
// refused, because the rule lived only in code.
// ---------------------------------------------------------------------------

function cmdFixBrief(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const featureId = options['feature'];
  if (!featureId) throw new InputError('usage: devflow.cjs fix-brief --mission-dir <dir> --feature <id> [--json]');
  const built = buildFixBrief(missionDir, featureId);
  if (options['json'] === true || options['json'] === 'true') {
    process.stdout.write(JSON.stringify(built, null, 2) + '\n');
    return;
  }
  process.stdout.write(built.brief + '\n');
}

/** The receipt-less round -- a fix after a blocking review, or a rebase after `not_fast_forward`.
 * It works out WHICH of the two the feature needs; `wave` asks it the same question. */
function buildFixBrief(missionDir, featureId) {
  const state = readState(missionDir);
  const featuresDoc = readMissionFeatures(missionDir, state);
  const feature = featuresDoc.features.find((f) => f.id === featureId);
  if (!feature) throw new RejectError(`feature not found: ${featureId}`, 'feature_not_found');
  if (!feature.branch || !feature.worktree) {
    throw new RejectError(
      `feature '${featureId}' has no branch and worktree to go back into (status ${feature.status}); ` +
        `this round exists for a feature already at the merge gate`,
      'nothing_to_fix'
    );
  }

  const last = lastReviewVerdict(missionDir, featureId);
  // Read-only: whether the branch sits on top of the integration branch is what merge-feature
  // answers with `not_fast_forward`, and asking git directly costs nothing and changes nothing.
  const onTop = git(['merge-base', '--is-ancestor', state.expectedBranch, feature.branch], state.repoRoot).ok;
  const integrationHead = git(['rev-parse', state.expectedBranch], state.repoRoot).stdout;

  let findings = [];
  if (last && last.status === 'fail') {
    // The report holds the text; the event holds only ids. Fall back to the ids rather than
    // rendering nothing -- a brief naming F-004 with no description still beats no brief.
    try {
      const report = JSON.parse(fs.readFileSync(last.reportPath, 'utf8'));
      findings = (report.findings || []).filter((f) => f.severity === 'blocking');
    } catch {
      findings = (last.itemIds || []).map((id) => ({ itemId: id, description: '(report unreadable — read it yourself)' }));
    }
  }
  const kind = findings.length ? 'fix' : (!onTop ? 'rebase' : null);
  if (!kind) {
    throw new RejectError(
      `feature '${featureId}' needs neither round: ` +
        `${last ? `its last review was ${last.status}` : 'it has no recorded review'}, and its branch is on top of ${state.expectedBranch}. ` +
        `${last && last.status === 'pass' ? "Run 'merge-feature --feature " + featureId + "'." : "Record a review first."}`,
      'nothing_to_fix'
    );
  }
  const round = (last ? last.round : 0) + 1;
  const proceduresPath = path.resolve(__dirname, '..', 'references', 'roles', 'worker.md');

  const L = [];
  L.push(`# ${kind === 'fix' ? 'Fix' : 'Rebase'} round — feature ${feature.id}, round ${round}`);
  L.push('');
  L.push(`**Read \`${proceduresPath}\` first** — it is the procedure you are held to. This brief carries only what is specific to this round.`);
  L.push('');
  L.push('You are finishing a feature that is already built and already reviewed. Its branch is at the merge gate and cannot land yet.');
  L.push('');

  if (kind === 'fix') {
    L.push('## The findings you are here to fix');
    L.push('');
    for (const f of findings) {
      L.push(`### ${f.itemId}${f.file ? ` — ${f.file}${f.line ? `:${f.line}` : ''}` : ''}`);
      L.push(f.description || '(no description)');
      if (f.suggestedFix) L.push(`\nSuggested: ${f.suggestedFix}`);
      L.push('');
    }
    L.push(`Fix these and nothing else. **A finding that survives keeps its id** — the next review reuses \`${findings.map((f) => f.itemId).join('`, `')}\`, which is what makes "did the fix work" answerable, so do not rename or re-scope one.`);
    L.push('');
    L.push('A problem you believe is not a defect is reported back, not silently left: say which id and why. Fixing something adjacent and calling it done is the failure this round is measured against.');
  } else {
    L.push(`## Rebase onto \`${state.expectedBranch}\``);
    L.push('');
    L.push(`Another feature merged first, so your branch is no longer on top and the merge gate refused it with \`not_fast_forward\`. That refusal is the point: what was verified is no longer what would ship.`);
    L.push('');
    L.push(`- Rebase \`${feature.branch}\` onto \`${integrationHead}\``);
    L.push('- Resolve conflicts **in favour of what your feature was for** — you are the only one who knows that. Never `--no-ff`, never merge.');
    L.push('- A conflict you cannot resolve without changing behaviour this feature does not own means the two features were not disjoint after all: stop and report it, naming the paths. That is the feedback that keeps the concurrency rule honest.');
  }

  L.push('');
  L.push('## Where');
  L.push('');
  L.push(`- **Work here, and only here:** \`${feature.worktree}\``);
  L.push(`- **Commit on this branch:** \`${feature.branch}\` — it is yours alone`);
  L.push(`- Integration branch: \`${state.expectedBranch}\` at \`${integrationHead}\` in \`${state.repoRoot}\` — never \`checkout\`, \`merge\` or push there`);
  if ((feature.touches || []).length) L.push(`- You own these paths and only these: ${feature.touches.map((t) => `\`${t}\``).join(', ')}`);
  L.push('- Do not modify anything under the plan directory; a branch that edits the plan is refused at the gate.');
  L.push('');
  const steps = (feature.verification && feature.verification.steps) || [];
  L.push('## Before you stop');
  L.push('');
  if (steps.length) {
    L.push('Re-run the feature\'s own checks — the scheduler runs these again at the gate, so a red one here is a wasted round:');
    L.push('');
    for (const st of steps) L.push(`- \`${st.command}\` (expected exit: ${st.expectedExit || 'zero'})`);
  } else {
    L.push('This feature declares no verification steps, so re-run whatever the repository uses to prove the change still works, and say what you ran.');
  }
  L.push('');
  L.push('## There is no receipt this round');
  L.push('');
  L.push('**Do not run `end-feature`, and do not `claim`.** The feature is already `in_review` and its worker session is closed; both commands refuse, correctly, because the work was already accounted for. Your output is the commits on the branch plus your report to the orchestrator, which records the next review and re-runs the merge gate.');
  L.push('');
  L.push('Report back: what you changed, which finding each change addresses (by id) or what the rebase touched, the commands you ran with their exit codes, and anything you could not do and why.');
  L.push('');
  // worker.md's escalation channel is a `failureClass` inside the receipt, and this round submits
  // none -- so without saying this, a worker that hits a blocker here has been handed a procedure
  // whose only exit does not exist, and the cheap way out is to press on.
  L.push('**If you hit something that needs a person** — the fix cannot be made inside this feature\'s paths, the finding is wrong, the rebase cannot be resolved without changing behaviour this feature does not own — **stop and say so in that report.** Commit nothing speculative. There is no `failureClass` to put it in this round; the orchestrator is the one holding the pause, and it can only hold it if you say the word.');
  L.push('');
  const brief = L.join('\n');
  return {
    featureId, kind, round, branch: feature.branch, worktree: feature.worktree,
    integrationBranch: state.expectedBranch, integrationHead, onTop,
    findings: findings.map((f) => f.itemId), brief,
  };
}

function cmdAssertions(args) {
  const { options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  if (!missionDir) throw new InputError('usage: devflow.cjs assertions --mission-dir <dir>');
  const state = readState(missionDir);
  process.stdout.write(JSON.stringify(missionLedger(missionDir, state), null, 2) + '\n');
}

/** Fold an accepted validator report into the ledger. Acceptance is validation-check's rules. */
function cmdRecordVerdicts(args) {
  const { options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  const reportPath = options['report'];
  const assignedArg = options['assigned'];
  const featureArg = options['feature'];
  const USAGE = 'usage: devflow.cjs record-verdicts --mission-dir <dir> --report <report.json> (--assigned <ids> | --feature <id>) [--milestone <name>] [--evidence-root <dir>]';
  if (!missionDir || !reportPath || (!assignedArg && !featureArg)) {
    throw new InputError(USAGE);
  }
  // Exactly one source for the assignment. Letting one silently win is how the plan's obligation
  // and a list retyped at the call site drift apart without anyone noticing.
  if (assignedArg && featureArg) {
    throw new InputError(`${USAGE}\nexactly one of --assigned or --feature, not both`);
  }
  const state = readState(missionDir);
  let assigned;
  if (featureArg) {
    // The plan is the authority -- but only once it is verified to still BE the approved plan.
    // The snapshot lives in missionDir, which the worker can write, so reading `proves` out of it
    // without the digest check every other consumer performs would let the party this check
    // exists to bind shrink its own obligation.
    const doc = readMissionFeatures(missionDir, state);
    const digest = planDigest(doc.features, doc, injectedFeatureIds(readEvents(missionDir)));
    if (digest !== state.planDigest) {
      throw new RejectError(
        'features snapshot planDigest mismatch: planning fields were tampered after propose',
        'features_tampered'
      );
    }
    const feature = (doc.features || []).find((f) => f.id === String(featureArg));
    if (!feature) {
      throw new RejectError(`unknown feature '${featureArg}' in this mission's plan`, 'unknown_feature');
    }
    assigned = Array.isArray(feature.proves) ? feature.proves.slice() : [];
    // An injected assertion validator claims nothing on purpose: a validator that claimed proof
    // would let `--validators` alone satisfy the proof-coverage gate. Its obligation is DERIVED
    // from the milestone instead -- and only the brief renderer knew that, so `--feature`, the path
    // SKILL.md prefers precisely because it cannot be shrunk by retyping, failed on every mission
    // the guide mandates. Same derivation as the brief, from the same function: the obligation and
    // the claim are different facts, but there is still only one of each.
    //
    // As tamper-safe as reading `proves` was: the caller above already verified planDigest, and
    // `fulfills` is covered by it too.
    if (!assigned.length && feature.validatorKind === 'assertions') {
      assigned = milestoneAssertionIds(doc.features, feature.milestone);
    }
    if (!assigned.length) {
      // An empty assignment accepts any report at all -- vacuous acceptance is worse than none.
      throw new RejectError(
        `feature '${featureArg}' declares no proves; --feature is for validation features, use --assigned for anything else`,
        'feature_proves_nothing'
      );
    }
  } else {
    assigned = String(assignedArg).split(',').map((x) => x.trim()).filter(Boolean);
  }
  const report = readJsonFile(reportPath);
  const inputErrs = validationReportInputErrors(report);
  if (inputErrs.length) {
    throw new InputError(`validation report missing/malformed fields:\n` + inputErrs.map((e) => `  ${e.kind}${e.at ? ' @ ' + e.at : ''}`).join('\n'));
  }
  // The same rules the hand-invoked checker applies: the ledger must not become the soft path
  // around them.
  const assertionDocs = contractAssertionDocs(state.contractPath);
  const contractIds = new Set(assertionDocs.keys());
  // Default root: with --feature it is the feature's own evidence dir -- the same convention the
  // briefs publish and validation-check is pointed at -- so a report that passed its pre-check
  // does not fail the fold on a different default. --assigned (no feature) keeps missionDir.
  const evidenceRoot = path.resolve(options['evidence-root'] ||
    (featureArg ? path.join(missionDir, 'evidence', featureArg) : missionDir));
  const { errs } = validationReportErrors(report, assigned, contractIds, evidenceRoot, assertionDocs);
  if (errs.length) {
    throw new RejectError(
      `validation report rejected:\n` + errs.map((e) => `  ${e.kind}${e.at ? ' @ ' + e.at : ''}${e.detail ? ' (' + e.detail + ')' : ''}`).join('\n'),
      'validation_report_rejected'
    );
  }
  const milestone = options['milestone'] || null;
  for (const a of report.assertions) {
    const artifacts = (a.evidence && Array.isArray(a.evidence.artifacts) ? a.evidence.artifacts : []).join('; ');
    logEvent(missionDir, 'assertion_verdict', { assertionId: a.id, verdict: a.status, milestone, evidence: artifacts, groupId: report.groupId || null });
  }
  const ledger = missionLedger(missionDir, state);
  writeLedgerProjection(missionDir, ledger);
  // A verdict is the other half of what seals a milestone: the work was already settled, and this
  // is what proved it.
  try {
    const doc = readMissionFeatures(missionDir, state);
    sealCompletedMilestones(missionDir, doc.features || [], state);
  } catch { /* no readable snapshot: sealing is re-evaluated on the next completion anyway */ }
  // Recording the verdict is exactly the action that resolves an `assertions_unproven` pause: it
  // exists because the ledger was unproven at closing. Once it is not, the pause must clear or the
  // next wave/next-feature treats it as a non-recoverable block and never runs the closing gate
  // again -- a mission that is provably done stays wedged. Same shape as recoverMission clearing an
  // environmental pause once its check re-verifies.
  if (ledger.unproven.length === 0) {
    const st = readState(missionDir);
    if (st.state === 'paused' && st.pauseReason === 'assertions_unproven') {
      st.state = 'orchestrator_turn';
      st.pauseReason = null;
      writeState(missionDir, st);
    }
  }
  process.stdout.write(JSON.stringify({ recorded: report.assertions.length, proven: ledger.proven, unproven: ledger.unproven }, null, 2) + '\n');
}

// ---------------------------------------------------------------------------
// waive-assertion subcommand: explicitly discharge an assertion that cannot be proven in this
// mission (e.g. a real-device hard gate blocked by a pre-existing product defect outside the
// spec's change surface), handed to follow-up by the human. Writes an assertion_waiver event the
// ledger reads; the verdict stays on record, the waiver only clears the closing gate. This is the
// human's documented override -- it requires --reason so the log shows why, and it refuses a
// pending assertion (waiving an un-ran assertion hides work never attempted).
// ---------------------------------------------------------------------------
function cmdWaiveAssertion(args) {
  const { options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  const assertionId = options['assertion'];
  const reason = options['reason'];
  if (!missionDir || !assertionId || !reason) {
    throw new InputError('usage: devflow.cjs waive-assertion --mission-dir <dir> --assertion <VAL-NNN> --reason <why>');
  }
  if (!fs.existsSync(missionDir)) throw new InputError(`missionDir does not exist: ${missionDir}`);
  const state = readState(missionDir);
  const contractIds = new Set([...contractAssertions(state.contractPath)]);
  const id = String(assertionId);
  if (!contractIds.has(id)) {
    throw new RejectError(`assertion '${id}' not in this mission's validation contract`, 'unknown_assertion');
  }
  const ledger = missionLedger(missionDir, state);
  const st = ledger.assertions[id] ? ledger.assertions[id].status : 'pending';
  if (st === 'passed') {
    throw new RejectError(`assertion '${id}' is already proven; nothing to waive`, 'assertion_already_proven');
  }
  // CR-09 (code review): the documented contract refuses a PENDING assertion —
  // waiving an un-ran assertion hides work never attempted. Only a verdict
  // that was actually produced (fail/blocked/skipped) can be discharged.
  if (st === 'pending') {
    throw new RejectError(
      `assertion '${id}' has never been run (pending); waiving an un-ran assertion hides work never attempted — run the validator first`,
      'assertion_not_attempted'
    );
  }
  logEvent(missionDir, 'assertion_waiver', { assertionId: id, reason: String(reason) });
  const after = missionLedger(missionDir, state);
  writeLedgerProjection(missionDir, after);
  // Same pause-clearing shape as record-verdicts: once the ledger owes nothing, the closing gate
  // must be allowed to run again or a waived-complete mission stays wedged.
  if (after.unproven.length === 0) {
    const st = readState(missionDir);
    if (st.state === 'paused' && st.pauseReason === 'assertions_unproven') {
      st.state = 'orchestrator_turn';
      st.pauseReason = null;
      writeState(missionDir, st);
    }
  }
  process.stdout.write(JSON.stringify({ waived: id, reason: String(reason), unproven: after.unproven }, null, 2) + '\n');
}

function cmdScrutiny(args) {
  const { options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  if (!missionDir) throw new InputError('usage: devflow.cjs scrutiny --mission-dir <dir>');
  const state = readState(missionDir);
  process.stdout.write(
    JSON.stringify(
      {
        gateCommands: state.gateCommands || [],
        checklist: state.reviewChecklist || [],
        checklistRequired: (state.reviewChecklist || []).length > 0,
      },
      null,
      2
    ) + '\n'
  );
}

function cmdValidationCheck(args) {
  const { positionals, options } = parseArgs(args);
  const contractArg = options['contract'];
  const assignedArg = options['assigned'];
  if (positionals.length !== 1 || !contractArg || !assignedArg) {
    throw new InputError(
      'usage: devflow.cjs validation-check <report.json> --contract <validation-contract.json> --assigned <VAL-001,VAL-002> [--evidence-root <dir>] [--require-proven]'
    );
  }
  const assigned = assignedArg
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
  if (!assigned.length) throw new InputError('--assigned must list at least one assertion id');

  const reportPath = positionals[0];
  const report = readJsonFile(reportPath);
  const inputErrs = validationReportInputErrors(report);
  if (inputErrs.length) {
    throw new InputError(
      `validation report missing/malformed fields:\n` +
        inputErrs.map((e) => `  ${e.kind}${e.at ? ' @ ' + e.at : ''}`).join('\n')
    );
  }
  const assertionDocs = contractAssertionDocs(contractArg);
  const contractIds = new Set(assertionDocs.keys());
  // Evidence paths are relative to --evidence-root, defaulting to the report's own directory.
  const evidenceRoot = path.resolve(options['evidence-root'] || path.dirname(path.resolve(reportPath)));

  const { errs, counts, artifactsChecked, unproven } = validationReportErrors(report, assigned, contractIds, evidenceRoot, assertionDocs);
  if (errs.length) {
    throw new RejectError(
      `validation report rejected:\n` +
        errs.map((e) => `  ${e.kind}${e.at ? ' @ ' + e.at : ''}${e.detail ? ' (' + e.detail + ')' : ''}`).join('\n'),
      'validation_report_rejected'
    );
  }
  process.stdout.write(
    JSON.stringify(
      {
        groupId: report.groupId || null,
        assigned: assigned.length,
        pass: counts.pass,
        fail: counts.fail,
        blocked: counts.blocked,
        skipped: counts.skipped,
        proven: counts.pass,
        unproven,
        outcome: unproven.length ? 'not-all-proven' : 'all-proven',
        evidenceArtifacts: artifactsChecked,
        verdict: 'accepted',
      },
      null,
      2
    ) + '\n'
  );
  // Acceptance passed; the outcome gate is opt-in so the two questions stay distinguishable.
  if (unproven.length && options['require-proven'] !== undefined) {
    throw new RejectError(
      `unproven @ ${unproven.join(',')} (${unproven.length} assertion(s) not proven)`,
      'validation_not_all_proven'
    );
  }
}

// ---------------------------------------------------------------------------
// reconcile subcommand: Phase D reconciliation (t7)
// reconcile --mission-dir <dir> --dispositions <dispositions.json>
// dispositions: {"featureId": "t7", "revision": 1,
//                "items": [{"itemId": "F-001", "verdict": "resolved|persists|regressed", "evidence": "..."}]}
// review-state.json is append-only: one {revision, itemId, verdict, evidence} entry per revision.
// Uncovered itemId -> exit 1; the same itemId persisting across 2 consecutive revisions -> raise
// reconcile_escalate_human (exit 1, the fix-round loop for the item is closed); pass -> exit 0 + summary.
// ---------------------------------------------------------------------------

const DISPO_STATES = ['resolved', 'persists', 'regressed'];
/** Cumulative regressed budget (cap 3, the upper bound of the ticket's 2-3); exhausted -> raise reconcile_escalate_human */
const FIX_ATTEMPTS_MAX = 3;

function cmdReconcile(args) {
  const { options } = parseArgs(args);
  const missionDir = options['mission-dir'] || process.env.MISSION_DIR;
  const dispoPath = options['dispositions'];
  if (!missionDir || !dispoPath) {
    throw new InputError('usage: devflow.cjs reconcile --mission-dir <dir> --dispositions <dispositions.json>');
  }
  const { state, featuresDoc } = loadMission(missionDir); // planDigest + contract re-check: addressesItemId is a hashed planning field, so forged coverage is rejected
  // Phase D only after every feature is completed (the review-loop input contract)
  if (state.state !== 'completed') {
    throw new RejectError(
      `reconcile requires mission completed (state=${state.state}); run must finish all features first`,
      'reconcile_mission_not_completed'
    );
  }
  const dispo = readJsonFile(dispoPath);
  if (typeof dispo !== 'object' || dispo === null || !Array.isArray(dispo.items)) {
    throw new InputError('dispositions must be an object with items[]');
  }
  const featureId = dispo.featureId;
  const feature = (featuresDoc.features || []).find((f) => f.id === featureId);
  if (!feature) throw new RejectError(`feature not found: ${featureId}`, 'reconcile_feature_not_found');
  const reviewPath = path.join(missionDir, 'review-state.json');
  let record;
  if (fs.existsSync(reviewPath)) {
    try {
      record = JSON.parse(fs.readFileSync(reviewPath, 'utf8'));
    } catch (e) {
      throw new RejectError(
        `review-state.json corrupt (${e.message}); disposition/escalation state unreadable — human recovery required`,
        'reconcile_state_corrupt'
      );
    }
    if (!Array.isArray(record.dispositions)) {
      throw new InputError('review-state.json malformed: missing dispositions[]');
    }
  } else {
    // Missing != first round: if the log already holds a reconcile_recorded (for any feature) the state was deleted/lost and an escalated terminal state could be bypassed -> fail-safe reject
    const logPath = path.join(missionDir, 'progress_log.jsonl');
    const recorded = fs.existsSync(logPath)
      ? fs
          .readFileSync(logPath, 'utf8')
          .split('\n')
          .filter(Boolean)
          .some((l) => {
            try {
              return JSON.parse(l).event === 'reconcile_recorded';
            } catch {
              return false;
            }
          })
      : false;
    if (recorded) {
      throw new RejectError(
        'review-state.json missing after prior reconcile rounds; disposition/escalation state lost — human recovery required',
        'reconcile_state_lost'
      );
    }
    record = { dispositions: [] };
  }
  // Derive per-itemId history (the audit array is authoritative; the map is only a checking view)
  const history = {};
  for (const d of record.dispositions) {
    (history[d.itemId] ||= []).push(d);
  }
  const revision = dispo.revision;
  if (typeof revision !== 'number' || !Number.isInteger(revision) || revision < 1) {
    throw new InputError('dispositions must include a positive integer revision');
  }
  // R+1 continuity (isolated per feature): revision must equal that feature's current max revision + 1; skipping numbers is forbidden
  const ownedItems = new Set(feature.addressesItemId || []);
  const priorRevs = Object.entries(history)
    .filter(([itemId]) => ownedItems.has(itemId))
    .flatMap(([, v]) => v.map((h) => h.revision));
  const maxRev = priorRevs.length ? Math.max(...priorRevs) : 0;
  if (revision !== maxRev + 1) {
    throw new InputError(
      `revision ${revision} not contiguous for feature ${featureId} (expected ${maxRev + 1}); R+1 must be sequential`,
      'revision_gap'
    );
  }
  // Stage 1: full validation (staged, nothing written; a failure never touches review-state)
  const seen = new Set();
  const staged = new Map(); // itemId -> {revision, verdict, evidence}
  for (const it of dispo.items) {
    if (!/^F-\d+$/.test(it.itemId)) {
      throw new InputError(`itemId '${it.itemId}' must match ^F-\\d+$ (orchestrator-assigned immutable id)`);
    }
    if (!DISPO_STATES.includes(it.verdict)) {
      throw new InputError(`bad verdict '${it.verdict}' for ${it.itemId}; expected resolved|persists|regressed`);
    }
    if (typeof it.evidence !== 'string' || it.evidence === '') {
      throw new InputError(`itemId ${it.itemId} requires non-empty evidence (audit trail)`);
    }
    if (seen.has(it.itemId)) {
      throw new InputError(`duplicate itemId ${it.itemId} in one revision; dispositions append-only, one entry per item per revision`);
    }
    seen.add(it.itemId);
    if (!(feature.addressesItemId || []).includes(it.itemId)) {
      throw new RejectError(
        `itemId ${it.itemId} not covered by feature ${featureId} (addressesItemId)`,
        'reconcile_uncovered_item'
      );
    }
    const hist = history[it.itemId] || [];
    if (hist.length > 0 && revision <= hist[hist.length - 1].revision) {
      throw new InputError(
        `revision ${revision} not monotonic for ${it.itemId} (last recorded revision ${hist[hist.length - 1].revision}); re-submission must use a new revision`,
        'revision_not_monotonic'
      );
    }
    staged.set(it.itemId, { revision, itemId: it.itemId, verdict: it.verdict, evidence: it.evidence });
  }
  // Completeness (isolated per feature): the first round must dispose of every addressesItemId (new findings also need an explicit three-state verdict);
  // later rounds must dispose of every itemId left unresolved by the previous revision, with no silent omissions
  const coveredThisRound = new Set(seen);
  if (ownedItems.size === 0) {
    // No bound items: empty items is legal (nothing open)
  } else if (priorRevs.length === 0) {
    const missingFirst = [...ownedItems].filter((k) => !coveredThisRound.has(k));
    if (missingFirst.length) {
      throw new RejectError(
        `first revision must disposition every addressesItemId: missing ${missingFirst.join(', ')}`,
        'reconcile_incomplete'
      );
    }
  } else {
    const pendingPrior = Object.entries(history)
      .filter(([itemId, v]) => v.length > 0 && v[v.length - 1].verdict !== 'resolved' && ownedItems.has(itemId))
      .map(([k]) => k);
    const omitted = pendingPrior.filter((k) => !seen.has(k));
    if (omitted.length) {
      throw new RejectError(
        `prior unresolved itemId(s) missing from this revision: ${omitted.join(', ')}`,
        'reconcile_incomplete'
      );
    }
  }
  // An escalated itemId is a persistent terminal state: later automatic reconciles always reject it (a human must act and clear the escalated marker)
  const escalated = record.escalated || {};
  const alreadyEscalated = [...staged.keys()].filter((k) => escalated[k] !== undefined);
  if (alreadyEscalated.length) {
    throw new RejectError(
      `itemId(s) already escalated to human review (${alreadyEscalated.map((k) => `${k}@${escalated[k]}`).join(', ')}); escalation is terminal until human disposition`,
      'reconcile_escalated'
    );
  }
  // Stage 2: one-shot persistence (transactional, append-only; the audit array is authoritative) + sync the derived history for stage 3
  for (const entry of staged.values()) {
    record.dispositions.push(entry);
    (history[entry.itemId] ||= []).push(entry);
  }
  fs.writeFileSync(reviewPath, JSON.stringify(record, null, 2) + '\n');
  // Persistence audit: the reconcile_recorded event makes a missing review-state.json detectable (state-loss fail-safe)
  logEvent(missionDir, 'reconcile_recorded', { featureId, revision, itemCount: staged.size });
  // Stage 3: evaluate escalation for the whole round (collect every itemId that needs a human)
  const escalations = [...staged.keys()].filter((k) => {
    const hist = history[k];
    const lastTwo = hist.slice(-2);
    const regressedCount = hist.filter((h) => h.verdict === 'regressed').length;
    // persists across 2 consecutive revisions -> escalate; regressed exhausting the cumulative fixAttempts budget (cap 3) -> escalate
    return (
      (hist[hist.length - 1].verdict === 'persists' && lastTwo.length === 2 && lastTwo.every((h) => h.verdict === 'persists')) ||
      regressedCount >= FIX_ATTEMPTS_MAX
    );
  });
  if (escalations.length) {
    for (const k of escalations) {
      record.escalated = escalated;
      escalated[k] = revision; // persistent terminal state: escalation marker
    }
    fs.writeFileSync(reviewPath, JSON.stringify(record, null, 2) + '\n');
    throw new RejectError(
      `itemId(s) require a decision: ${escalations.join(', ')} (persists 2 revisions or fixAttempts exhausted); escalation persisted — the fix-round loop is closed, change approach or ask at the autonomy frontier (SKILL.md §0.5)`,
      'reconcile_escalate_human'
    );
  }
  const rounds = Object.fromEntries(
    Object.entries(history).map(([k, v]) => [k, v.map((h) => `${h.revision}:${h.verdict}`).join(',')])
  );
  process.stdout.write(JSON.stringify({ featureId, revision, rounds }, null, 2) + '\n');
}

// ---------------------------------------------------------------------------
// wave subcommand: one call, one orchestrator turn
//
// A subagent can only be created from inside a running agent turn, so the orchestrator process must
// EXIT before a worker can be spawned. Control flow cannot span a spawn. That rules out a long-lived
// program that pauses at spawn points and leaves exactly one shape: a query that settles what came
// back, advances everything reachable without an agent, and hands over the whole next wave at once.
//
// This is that query. It replaces a four-command turn (next-feature, claim, review-brief, fix-brief)
// with one, and -- more to the point -- it makes every decision from durable state rather than from
// anything held between calls. Nothing here may assume it saw the previous turn.
// ---------------------------------------------------------------------------

/**
 * Which round a feature parked at the merge gate needs next.
 *
 * `code` is the merge gate's own refusal, so the vocabulary is the gate's, not a second opinion.
 * The one judgement made here is the `review_blocking` split, and it is the rule that a hand-driven
 * loop got wrong for a whole mission: a blocking verdict stays blocking until a NEW verdict replaces
 * it, so reading the gate and dispatching another fix repeats the same round forever. If the branch
 * has moved since the verdict was recorded, the fix already happened and what is owed is a review.
 *
 * The comparison is against the commit the verdict judged (`review_recorded.head`), not a timestamp:
 * git commit dates have one-second resolution, and a fix landing in the same second as its verdict
 * was indistinguishable from no fix at all.
 */
function roundNeeded(missionDir, state, feature, code) {
  if (code === 'review_missing') return 'review';
  if (code === 'not_fast_forward') return 'rebase';
  if (code !== 'review_blocking') return null; // plan_modified, no_branch, verification_failed: not a round
  const verdict = lastReviewVerdict(missionDir, feature.id);
  const reviewedHead = verdict && verdict.head;
  if (!reviewedHead || !feature.branch) return 'fix';
  const head = git(['rev-parse', feature.branch], state.repoRoot);
  if (!head.ok || !head.stdout) return 'fix';
  return head.stdout !== reviewedHead ? 'review' : 'fix';
}

/**
 * How many fix rounds this feature has already spent, derived rather than counted.
 *
 * Every fix round ends in a fresh verdict, so the round number on the last verdict IS the count. It
 * has to be derived: a counter held in a variable is correct only for an executor that keeps one
 * process for the whole mission, and this executor exits at every handoff. Measured on the loop this
 * replaces: an unfixable finding took nine fix rounds against a budget of two, because the counter
 * was back at zero on every poll.
 */
function fixRoundsSpent(missionDir, featureId) {
  const v = lastReviewVerdict(missionDir, featureId);
  return v && Number.isInteger(v.round) ? Math.max(0, v.round - 1) : 0;
}

/**
 * Work the machine can see is owed but must not do itself.
 *
 * Recording a verdict is a judgement (SKILL.md keeps it with the orchestrator), and so is disposing
 * of what a worker found. Both are therefore invisible to a loop that only asks "what can I spawn" --
 * which is how a mission reaches "nothing left to spawn" with an unproven ledger and calls it done.
 * Naming them is what makes that ending impossible.
 */
function pendingOrchestratorWork(missionDir, state, featuresDoc) {
  const moves = [];
  // Gate on the SAME condition closingReview uses to pause as `assertions_unproven`: the ledger gate
  // is active for an injected validator (`--validators`) OR a plan that itself declares `proves`.
  // Gating only on `validatorsRequested` left a plan-declared proof (proposed WITHOUT --validators)
  // with a completed validator, an unproven ledger, and no record-verdicts move to clear the pause.
  const proofPlanned = proofCoverageApplies(featuresDoc.features, false);
  if ((state.validatorsRequested === true || proofPlanned) && state.contractPath) {
    const ledger = missionLedger(missionDir, state);
    // Freshness by log position: a verdict recorded BEFORE a feature's latest completion cannot
    // reflect that run. A re-validator amended to re-prove a FAILED assertion leaves the ledger
    // 'failed', not 'pending' -- keying only on status never names its report as a move, and the
    // mission stays wedged one step past the claim gate that let the remedy run (forge issue #1).
    // The check also cannot spin: once the fresh report is folded, its verdict sits after the
    // completion and the move goes away by itself.
    const events = readEvents(missionDir);
    const lastVerdictAt = new Map();
    const completedAt = new Map();
    events.forEach((e, idx) => {
      if (e.event === 'assertion_verdict') lastVerdictAt.set(e.assertionId, idx);
      if (e.event === 'feature_completed') completedAt.set(e.featureId, idx);
    });
    for (const f of featuresDoc.features) {
      if (f.status !== 'completed' || f.reportOnly !== true) continue;
      // An injected assertion validator claims nothing -- the obligation is DERIVED from the
      // milestone, exactly where the brief and record-verdicts derive it. Reading `fulfills` +
      // `proves` would be empty here and the move would silently not exist, which is how a
      // completed validator's report reaches no ledger and the mission ends unproven.
      const claimed = f.validatorKind === 'assertions'
        ? milestoneAssertionIds(featuresDoc.features, f.milestone)
        : [...(f.fulfills || []), ...(f.proves || [])];
      const done = completedAt.get(f.id);
      const unrecorded = claimed.filter((id) => {
        const a = (ledger.assertions || {})[id];
        if (!a || a.status === 'pending') return true;
        const seen = lastVerdictAt.get(id);
        return done !== undefined && (seen === undefined || seen < done);
      });
      if (!unrecorded.length) continue;
      moves.push({
        kind: 'record-verdicts',
        feature: f.id,
        assertions: unrecorded,
        why: `'${f.id}' completed but ${unrecorded.length} assertion(s) it claims have no verdict reflecting that run`,
      });
    }
  }
  // A non-blocking finding is "covered" once some feature declares it in `addressesItemId`, so
  // undisposed is derivable here and the move can actually be cleared.
  //
  // `discoveredIssues` and `whatWasLeftUndone` are deliberately NOT here, although SKILL.md makes
  // disposing of them an obligation too. Nothing records a disposition for them, so "undisposed"
  // cannot be derived -- only "reported" can, and a move that no action clears would be repeated
  // every turn until it was ignored, which is worse than not naming it. Making them dispositionable
  // is its own change; until then that obligation stays with the reader of the receipt.
  const open = openReviewFindings(missionDir, featuresDoc.features);
  if (open.length) {
    moves.push({
      kind: 'dispose-finding',
      items: open,
      why: `${open.length} non-blocking review finding(s) that no feature addresses; "non-blocking" is a reason to schedule, not to drop`,
    });
  }
  return moves;
}

/**
 * Record every review report that a reviewer has written and nobody has recorded yet.
 *
 * The round a report belongs to is not guessed: `review-brief` names the destination
 * `reviews/<feature>-r<n>.json` where n is one past the verdicts already on the record, so a file at
 * that exact path is by construction the answer to the round now owed. A report for an already
 * recorded round is not re-recorded -- that path would be a re-record, which `record-review` treats
 * as a supersede, and a poll must never supersede anything.
 */
function settleReviews(missionDir) {
  let rt;
  try {
    rt = JSON.parse(fs.readFileSync(path.join(missionDir, 'runtime.json'), 'utf8'));
  } catch {
    return;
  }
  let state, featuresDoc;
  for (const [featureId, f] of Object.entries(rt.features || {})) {
    if (f.status !== 'in_review') continue;
    const round = reviewEvents(missionDir, featureId).length + 1;
    const reportPath = path.join(missionDir, 'reviews', `${featureId}-r${round}.json`);
    if (!fs.existsSync(reportPath)) continue;
    try {
      // In-process, not a record-review subprocess: the mutation is the same (recordReviewCore), but
      // this keeps `wave` free of a CLI child that would re-acquire the mission lock the parent holds.
      if (!state || !featuresDoc) ({ state, featuresDoc } = loadMission(missionDir));
      recordReviewCore(missionDir, state, featuresDoc, featureId, reportPath, round);
    } catch (e) {
      // A malformed report is the reviewer's problem to fix, not a reason to stop the turn: the rest
      // of the wave is unaffected and the feature simply stays at the gate, which is what it means.
      logEvent(missionDir, 'review_record_refused', {
        featureId, round, reportPath, error: String(e.message || e).slice(0, 300),
      });
    }
  }
}

function readRoundsOffered(missionDir) {
  try {
    return JSON.parse(fs.readFileSync(path.join(missionDir, 'rounds.json'), 'utf8'));
  } catch {
    return {};
  }
}

function writeRoundsOffered(missionDir, rounds) {
  fs.writeFileSync(path.join(missionDir, 'rounds.json'), JSON.stringify(rounds, null, 2) + '\n');
}

function cmdWave(args) {
  const { options } = parseArgs(args);
  const missionDir = resolveMissionDir(options);
  const asJson = options['json'] === true || options['json'] === 'true';
  const maxAttempts = options['max-attempts'] !== undefined ? Number(options['max-attempts']) : 3;
  if (!Number.isInteger(maxAttempts) || maxAttempts < 1) {
    throw new InputError(`--max-attempts must be a positive integer, got '${options['max-attempts']}'`);
  }
  const verificationTimeoutMs = (Number(options['timeout']) || 1800) * 1000;
  const maxFixRounds = options['max-rounds'] !== undefined ? Number(options['max-rounds']) : 2;

  // 0: settle the reviews that came back. A report on disk is what a reviewer returned, exactly as a
  // receipt is what a worker returned, and settling both is this command's job -- rendering the
  // brief and never recording the answer is how a loop dispatches the same reviewer forever.
  //
  // Recording is mechanical and therefore belongs here: `record-review` DERIVES the verdict from the
  // findings and refuses a report whose summary line disagrees with them. (Contrast `record-verdicts`
  // for the assertion ledger, which is a judgement and stays in `yourMove`.)
  //
  // Done BEFORE `settleRound` loads the mission, and through the CLI rather than in-process: the
  // subcommand takes the mission lock and has its own tests, and running it after the load would
  // leave this command holding a stale copy of what it just changed.
  settleReviews(missionDir);

  // Rounds already dispatched for features at the gate. A round is offered once; while it is still
  // owed (no fresh verdict, branch unchanged) a later poll must NOT hand out a second round of the
  // same kind -- that is how a poll loop launched a stream of fix/review agents for one fork.
  const roundsOffered = readRoundsOffered(missionDir);
  let roundsDirty = false;

  // 1-2: settle receipts, run every merge gate, recover a leftover session, charge the round.
  const { out: round, state, featuresDoc } = settleRound(missionDir, {
    maxAttempts, verificationTimeoutMs, startRound: true,
  });

  const spawn = [];
  const escalate = [];

  // 3a: rounds owed to features already at the gate. Rendered BEFORE anything new is claimed, so a
  // fresh claim cannot change what an already-parked branch is owed.
  for (const at of round.awaitingMerge) {
    if (!at.code) continue; // it merged during settle
    const feature = featuresDoc.features.find((f) => f.id === at.id);
    const kind = roundNeeded(missionDir, state, feature, at.code);
    if (!kind) {
      escalate.push({ feature: at.id, code: at.code, why: at.message || 'the merge gate refuses this, and no round addresses it' });
      continue;
    }
    if (kind === 'fix' && fixRoundsSpent(missionDir, at.id) >= maxFixRounds) {
      // A finding that survives its rounds stopped being a worker problem.
      escalate.push({
        feature: at.id, code: at.code,
        why: `still blocking after ${maxFixRounds} fix round(s); this is a decision, not another attempt`,
      });
      continue;
    }
    // The round that WOULD be handed out now: the same number the brief will carry. A round is owed
    // to the gate, but only once -- if we already offered this exact round and no fresh verdict has
    // arrived to consume it, the agent for it is still out, so hand nothing down.
    const roundNumber = kind === 'review'
      ? reviewEvents(missionDir, at.id).length + 1
      : (lastReviewVerdict(missionDir, at.id) ? lastReviewVerdict(missionDir, at.id).round : 0) + 1;
    const offered = roundsOffered[at.id];
    if (offered && offered.kind === kind && roundNumber <= offered.round) continue;
    try {
      const built = kind === 'review'
        ? buildReviewBrief(missionDir, at.id)
        : buildFixBrief(missionDir, at.id);
      const usedRound = built.round != null ? built.round : roundNumber;
      roundsOffered[at.id] = { kind, round: usedRound };
      roundsDirty = true;
      spawn.push({
        role: kind === 'review' ? 'reviewer' : kind,
        feature: at.id,
        round: usedRound,
        brief: writeWaveBrief(missionDir, kind === 'review' ? 'review' : kind, at.id, usedRound, built.brief),
        cwd: kind === 'review' ? state.repoRoot : built.worktree,
        reportPath: built.reportPath || null,
      });
    } catch (e) {
      escalate.push({ feature: at.id, code: at.code, why: `no ${kind} round could be rendered: ${e.message}` });
    }
  }

  // 3b: new work. `preSpawnChecks` runs ONCE, outside the loop -- what it checks (the integration
  // branch, a clean worktree) is mission-wide, and running it per candidate would pause the mission
  // on behalf of whichever feature happened to be first in the list.
  const refused = [];
  const candidates = eligibleFeatures(featuresDoc.features);
  if (candidates.length) {
    const checks = preSpawnChecks(missionDir, state, featuresDoc, candidates[0], verificationTimeoutMs);
    if (checks.ok) {
      for (const feature of candidates) {
        const blocked = claimBlock(state, featuresDoc, feature, missionDir);
        if (blocked) {
          refused.push({ feature: feature.id, code: blocked.code, message: blocked.message });
          continue;
        }
        const claim = openWorkerSession(missionDir, state, featuresDoc, feature, { withBrief: true });
        const role = FEATURE_KIND_ROLE[claim.kind] || 'worker';
        spawn.push({
          role,
          feature: feature.id,
          round: null,
          // No attempt in the path: this brief has to be found again by a later turn that knows
          // only the role and the feature. A retry overwrites it, which is right -- the previous
          // attempt's brief is stale the moment a new session opens.
          brief: writeWaveBrief(missionDir, role, feature.id, null, claim.brief),
          cwd: claim.workingDirectory,
          workerSessionId: claim.workerSessionId,
        });
      }
    } else {
      refused.push({ feature: null, code: checks.pauseReason, message: 'the repository is not in a state anything can be spawned into' });
    }
  }

  // A feature claimed on an earlier turn whose worker has not reported. Normal between invocations
  // under this executor -- the brief was handed over and the subagent is still working, or was never
  // spawned. Re-offer the brief that already exists; never re-claim, never release on a poll.
  const awaiting = featuresDoc.features
    .filter((f) => f.status === 'in_progress' && !spawn.some((sp) => sp.feature === f.id))
    .map((f) => {
      const role = FEATURE_KIND_ROLE[featureKind(f)] || 'worker';
      const brief = waveBriefPath(missionDir, role, f.id);
      return {
        feature: f.id,
        workerSessionId: f.currentWorkerSessionId,
        cwd: f.worktree || state.workingDirectory,
        brief: fs.existsSync(brief) ? brief : null,
        note: fs.existsSync(brief)
          ? 'claimed and awaiting its receipt — spawn it if you have not, or wait for the one running'
          : 'claimed outside this command, so no brief is on disk; a brief cannot be re-rendered for an open session',
      };
    });

  const out = {
    missionState: round.missionState,
    pauseReason: round.pauseReason,
    done: round.done,
    spawn,
    awaiting,
    yourMove: pendingOrchestratorWork(missionDir, state, featuresDoc),
    escalate,
    refused,
    unproven: round.unproven,
    openFindings: round.openFindings,
    salvaged: round.salvaged,
    open: round.open,
  };

  if (roundsDirty) writeRoundsOffered(missionDir, roundsOffered);

  if (asJson) {
    process.stdout.write(JSON.stringify(out, null, 2) + '\n');
    return;
  }
  process.stdout.write(renderWave(out));
}

/** Briefs go to disk under one stable name per role and feature, so re-running points at the same
 * file rather than accumulating a copy of the same brief every turn. */
function waveBriefPath(missionDir, role, featureId, round) {
  const dir = path.join(missionDir, 'briefs');
  const suffix = round ? `-r${round}` : '';
  return path.join(dir, `${role}-${String(featureId).replace(/[^\w.-]/g, '_')}${suffix}.md`);
}
function writeWaveBrief(missionDir, role, featureId, round, text) {
  const p = waveBriefPath(missionDir, role, featureId, round);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, text);
  return p;
}

function renderWave(out) {
  const L = [];
  // `done` is not a reason to stop talking. A mission can complete with a non-blocking finding
  // nobody disposed of and assertions nobody proved -- and returning early on `done` printed
  // "mission completed" over exactly the items that must not be dropped. Say it finished, then say
  // what it finished owing.
  if (out.done) L.push('mission completed');
  if (!out.done && out.spawn.length) {
    L.push(`spawn ${out.spawn.length} agent(s) — independent by construction, so run them together:`);
    L.push('');
    for (const sp of out.spawn) {
      L.push(`  ${sp.role}:${sp.feature}${sp.round ? ` r${sp.round}` : ''}`);
      L.push(`    brief  ${sp.brief}`);
      L.push(`    cwd    ${sp.cwd}`);
    }
    L.push('');
    L.push('Give each the CONTENTS of its brief file as its prompt, and the cwd shown as its working');
    L.push('directory. Append your judgment to the brief; never replace it. Then run `wave` again.');
  } else if (!out.done && !out.awaiting.length && !out.yourMove.length && !out.escalate.length) {
    L.push(`nothing to spawn; mission state=${out.missionState}${out.pauseReason ? ` (${out.pauseReason})` : ''}`);
  }
  if (out.awaiting.length) {
    L.push('');
    L.push(`awaiting a receipt: ${out.awaiting.map((a) => a.feature).join(', ')}`);
  }
  if (out.yourMove.length) {
    L.push('');
    L.push('yours to do — the machine can see these are owed but must not do them:');
    for (const m of out.yourMove) L.push(`  ${m.kind}${m.feature ? ` ${m.feature}` : ''} — ${m.why}`);
  }
  if (out.escalate.length) {
    L.push('');
    L.push('escalate — no round addresses these:');
    for (const e of out.escalate) L.push(`  ${e.feature} (${e.code}) — ${e.why}`);
  }
  if (out.unproven && out.unproven.length) {
    L.push('');
    L.push(`unproven: ${out.unproven.join(', ')}`);
  }
  return L.join('\n') + '\n';
}

// ---------------------------------------------------------------------------
// missions subcommand: what am I in the middle of?
//
// Nothing outside a mission directory has ever known that a mission is in flight. The state is all
// there -- committed plan, ignored runtime, event log -- but only to someone who already knows
// which directory to look in, and that is precisely what a fresh context does not know. The
// observed cost was an operator reading agent session transcripts out of ~/.omp to work out what
// their own repository was doing.
//
// This is the discovery primitive: cheap, read-only, and it never throws. Those three properties
// are not incidental -- a session-start hook is built on top of it (SKILL.md), and a hook that is
// slow, or that writes, or that can fail, damages the thing it exists to help.
// ---------------------------------------------------------------------------

/** Walk up for a `.git`, without spawning git: this runs on the session-start path. */
function repoRootNear(from) {
  let dir = path.resolve(from);
  for (;;) {
    if (fs.existsSync(path.join(dir, '.git'))) return dir;
    const up = path.dirname(dir);
    if (up === dir) return null;
    dir = up;
  }
}

/**
 * Every mission under `root` that has not finished.
 *
 * Returns [] for anything it cannot read rather than throwing -- an unreadable mission is exactly
 * the case where a caller most needs the others, and a discovery command that dies on one bad
 * directory is a discovery command nobody can rely on.
 */
function activeMissions(root) {
  const repoRoot = repoRootNear(root);
  if (!repoRoot) return [];
  const specsRoot = specsRootFor(repoRoot);
  let entries;
  try {
    entries = fs.readdirSync(specsRoot, { withFileTypes: true });
  } catch {
    return [];
  }
  const out = [];
  for (const e of entries) {
    if (!e.isDirectory()) continue;
    const missionDir = path.join(specsRoot, e.name, 'missions');
    const statePath = path.join(missionDir, 'state.json');
    let state;
    try {
      state = JSON.parse(fs.readFileSync(statePath, 'utf8'));
    } catch {
      continue; // no mission here, or one we cannot read
    }
    if (state.state === 'completed' || state.state === 'cancelled') continue;
    // Open work, counted from the runtime rather than the plan: the plan has no statuses by design.
    let open = null;
    try {
      const rt = JSON.parse(fs.readFileSync(path.join(missionDir, 'runtime.json'), 'utf8'));
      open = Object.values(rt.features || {})
        .filter((f) => f.status !== 'completed' && f.status !== 'cancelled').length;
    } catch { /* a mission proposed but never run has no runtime yet */ }
    out.push({
      // The SESSION is the identity, not `missionId`. `missionId` comes from the plan's `mission`
      // field and is reused verbatim across re-proposals: one real repository holds four missions
      // all calling themselves `assistant-ui-migration`, distinguishable only by directory. The
      // session is what `propose --session` made unique, and it is what names the directory.
      session: state.sessionId || e.name,
      mission: state.missionId || null,
      missionDir,
      repoRoot: state.repoRoot || repoRoot,
      state: state.state || 'orchestrator_turn',
      pauseReason: state.state === 'paused' ? state.pauseReason || null : null,
      openFeatures: open,
    });
  }
  return out;
}

function cmdMissions(args) {
  const { options } = parseArgs(args);
  const root = options['root'] ? path.resolve(String(options['root'])) : process.cwd();
  const found = activeMissions(root);
  if (options['json'] === true || options['json'] === 'true') {
    process.stdout.write(JSON.stringify({ missions: found }, null, 2) + '\n');
    return;
  }
  if (!found.length) {
    process.stdout.write('no mission in flight here\n');
    return;
  }
  for (const m of found) {
    // Zero open features and not completed means the work is done and the CLOSING gate is not:
    // unproven assertions, an open finding, a milestone unsealed. Saying "0 open" and stopping
    // there reads as finished, which is the one thing it is not.
    const open = m.openFeatures === null
      ? 'not started'
      : m.openFeatures === 0
        ? 'all features done — waiting at the closing gate'
        : `${m.openFeatures} feature(s) open`;
    const why = m.pauseReason ? ` — paused: ${m.pauseReason}` : '';
    const also = m.mission && m.mission !== m.session ? ` (plan: ${m.mission})` : '';
    process.stdout.write(`${m.session}${also}: ${open}${why}\n  ${m.missionDir}\n`);
  }
}

// ---------------------------------------------------------------------------
// CLI entry point
// ---------------------------------------------------------------------------

/**
 * Subcommands that read the mission's state and write it back.
 *
 * Every one of these is a read-modify-write over state.json and runtime.json with no protection of
 * its own. That was safe for exactly as long as one orchestrator issued one command at a time --
 * and it was never guaranteed, only true. Two things ended it: a wave of work driven from one
 * process (whose only defence was that `spawnSync` blocks the event loop, an invariant so invisible
 * it had to be documented as load-bearing after the fact), and a model host that can issue parallel
 * tool calls, where no event loop is in the way at all. A lost update here is not a crash; it is a
 * feature silently un-claimed, or a receipt settled against a plan that has moved.
 *
 * So the list is the contract: adding a subcommand that writes mission state means adding it here.
 *
 * FOUR subcommands are deliberately absent, and the reason is the same for all of them: a lock may
 * only be held across a bounded transaction.
 *
 * - `end-feature` takes this lock itself, AFTER reading its receipt from stdin. That ordering is
 *   deliberate: acquiring first would hold the lock across a blocking read from a pipe whose writer
 *   may be slow.
 * - `next-feature` and `merge-feature` run the plan's verification steps as subprocesses, with a
 *   default timeout of 1800s. Holding the lock across that would starve a worker's `end-feature`
 *   past its 30s wait and reject it as `mission_busy` -- discarding a receipt that is committed, on
 *   its branch and correct, which is the exact failure this lock was introduced to prevent.
 *
 * Those last two are not "safe as they are"; they need their transaction split, so that verification
 * runs outside the lock and the state transition inside it re-checks what it assumed. That is real
 * design work and it belongs with `wave` (P5/W-D), which is being written fresh and can be shaped
 * that way from the start. Until then the race they carry is the one this file already had.
 */
const MUTATES_MISSION = new Set([
  'claim', 'release', 'record-review', 'review-supersede', 'amend', 'cancel',
  'resolve-pause', 'bind-authority', 'record-verdicts', 'reconcile', 'log',
  'waive-assertion', 'reconcile-forge', 'close-forge-ticket',
]);

/** The mission directory a command will act on, or null when it names none. */
function missionDirOf(cmd, rest) {
  if (!MUTATES_MISSION.has(cmd)) return null;
  try {
    const { options } = parseArgs(rest);
    return resolveMissionDir(options);
  } catch {
    // Unparseable or absent: let the command itself produce the usage error. Refusing to lock is
    // right here -- a command that cannot name its mission cannot corrupt one either.
    return null;
  }
}

async function main() {
  const [, , cmd, ...rest] = process.argv;
  let missionLock = null;
  try {
    const md = missionDirOf(cmd, rest);
    if (md && fs.existsSync(md)) {
      missionLock = acquireMissionLock(md);
      if (!missionLock) {
        throw new RejectError(
          `another devflow transaction still holds this mission's lock after ${MISSION_LOCK_WAIT_MS / 1000}s; ` +
            'it is wedged, not merely busy',
          'mission_busy'
        );
      }
    }
    switch (cmd) {
      case 'validate':
        cmdValidate(rest);
        break;
      case 'plan':
        cmdPlan(rest);
        break;
      case 'propose':
        cmdPropose(rest);
        break;
      case 'receipt-check':
        cmdReceiptCheck(rest);
        break;
      case 'end-feature':
        await cmdEndFeature(rest);
        break;
      case 'compile':
        cmdCompile(rest);
        break;
      case 'next-feature':
        cmdNextFeature(rest);
        break;
      case 'claim':
        cmdClaim(rest);
        break;
      case 'review-supersede':
        cmdReviewSupersede(rest);
        break;
      case 'record-review':
        cmdRecordReview(rest);
        break;
      case 'merge-feature':
        cmdMergeFeature(rest);
        break;
      case 'cancel':
        cmdCancel(rest);
        break;
      case 'bind-authority':
        cmdBindAuthority(rest);
        break;
      case 'missions':
        cmdMissions(rest);
        break;
      case 'amend':
        cmdAmend(rest);
        break;
      case 'resolve-pause':
        cmdResolvePause(rest);
        break;
      case 'release':
        cmdRelease(rest);
        break;
      case 'gate':
        cmdGate(rest);
        break;
      case 'log':
        cmdLog(rest);
        break;
      case 'milestones':
        cmdMilestones(rest);
        break;
      case 'assertions':
        cmdAssertions(rest);
        break;
      case 'record-verdicts':
        cmdRecordVerdicts(rest);
        break;
      case 'waive-assertion':
        cmdWaiveAssertion(rest);
        break;
      case 'scrutiny':
        cmdScrutiny(rest);
        break;
      case 'validation-check':
        cmdValidationCheck(rest);
        break;
      case 'reconcile':
        cmdReconcile(rest);
        break;
      case 'wave':
        cmdWave(rest);
        break;
      case 'reconcile-forge':
        cmdReconcileForge(rest);
        break;
      case 'close-forge-ticket':
        cmdCloseForgeTicket(rest);
        break;
      case 'brief':
        cmdBrief(rest);
        break;
      case 'review-brief':
        cmdReviewBrief(rest);
        break;
      case 'fix-brief':
        cmdFixBrief(rest);
        break;
      case '--help':
      case 'help':
        process.stdout.write(
          'usage: devflow.cjs <brief|review-brief|fix-brief|compile|validate|plan|propose|record-review|review-supersede|merge-feature|amend|cancel|receipt-check|end-feature|next-feature|claim|resolve-pause|release|gate|milestones|scrutiny|assertions|record-verdicts|validation-check|reconcile|log> ...\n' +
            '  compile --spec <spec.md> --tickets <dir> [--out-dir <dir>] [--mission <name>] [--milestone <name>] [--generated-at <date>] [--require-acceptance]\n' +
            '  validate <features.json> --contract <validation-contract.json> [--require-proof-coverage] [--require-acceptance] [--max-concurrency <1-3>]\n' +
            '  plan [--features <features.json>] [--max-concurrency <1-3>] [--json]   (project the plan\'s waves; never runs, has no authority)\n' +
            '  propose --session <id> --working-directory <path> --features <path> --contract <validation-contract.json> [--branch <name>] [--mission-dir <dir>] [--skills-root <dir>] [--artifact-root <dir>] [--validators] [--skip-scrutiny] [--skip-assertions] [--gate-command <id>=<cmd>] [--review-checklist <file>] [--repair-budget <n>] [--max-concurrency <1-3>] [--worktree-root <dir>] [--review-gate] [--forge]\n' +
            '  receipt-check <receipt.json> --mission-dir <dir> [--feature <id>]\n' +
            '  end-feature (stdin receipt JSON) --mission-dir <dir> --feature <id> --worker-session <id>\n' +
            '    (env MISSION_DIR/WORKER_SESSION_ID/FEATURE_ID are the fallback for a host-launched worker)\n' +
            '  next-feature --mission-dir <dir> [--json] [--start-round] [--max-attempts <n>]\n' +
            '  claim --mission-dir <dir> --feature <id> [--json] [--brief]\n' +
            '  review-brief --mission-dir <dir> --feature <id> [--json]\n  fix-brief --mission-dir <dir> --feature <id> [--json]   (the receipt-less round after a refused merge)\n  record-review --mission-dir <dir> --feature <id> --report <review.json> [--round <n>] [--json]\n' +
            '  merge-feature --mission-dir <dir> --feature <id>\n' +
            '  amend --mission-dir <dir> (--add-feature <feature.json> | --add-assertion <assertion.json>) [--json]\n' +
            '  cancel --mission-dir <dir> --feature <id> --reason <why>\n' +
            '  bind-authority --mission-dir <dir> --features-path <path> --contract-path <path> --reason <why>\n' +
            '    (re-point a mission at committed snapshots whose digests equal those locked at propose; digests are never rewritten)\n' +
            '  resolve-pause --mission-dir <dir> --feature <id> --reason <why> (blocked/boundary-violation/partial pauses)\n' +
            '  release --mission-dir <dir> --feature <id> [--reason <no_receipt|user_abort|...>]\n' +
            '  gate --cmd <shell cmd> [--expect fail|pass] [--target <name>] [--json] [--timeout <sec>]\n' +
            '  brief --mission-dir <dir> [--json]   (read-only: where the mission stands)\n  milestones --mission-dir <dir>\n' +
            '  scrutiny --mission-dir <dir>\n' +
            '  reconcile-forge --mission-dir <dir>   (run the crash reconcile/resume probe keyed by prId; idempotent, never re-invokes pr merge)\n' +
            '  close-forge-ticket --mission-dir <dir> --feature <id> [--evidence <pointer>] [--status <status>]\n' +
            '  assertions --mission-dir <dir>\n' +
            '  record-verdicts --mission-dir <dir> --report <report.json> (--assigned <ids> | --feature <id>) [--milestone <name>] [--evidence-root <dir>]\n' +
            '  validation-check <report.json> --contract <validation-contract.json> --assigned <VAL-001,VAL-002> [--evidence-root <dir>] [--require-proven]\n' +
            '  reconcile --mission-dir <dir> --dispositions <dispositions.json>\n' +
            '  log --mission-dir <dir> <event> [--json <payload>]\n'
        );
        break;
      default:
        process.stderr.write(
          'usage: devflow.cjs <brief|review-brief|fix-brief|compile|validate|plan|propose|record-review|review-supersede|merge-feature|amend|cancel|receipt-check|end-feature|next-feature|claim|resolve-pause|release|gate|milestones|scrutiny|assertions|record-verdicts|validation-check|reconcile|log> ...\n' +
            '  compile --spec <spec.md> --tickets <dir> [--out-dir <dir>] [--mission <name>] [--milestone <name>] [--generated-at <date>] [--require-acceptance]\n' +
            '  validate <features.json> --contract <validation-contract.json> [--require-proof-coverage] [--require-acceptance] [--max-concurrency <1-3>]\n' +
            '  plan [--features <features.json>] [--max-concurrency <1-3>] [--json]   (project the plan\'s waves; never runs, has no authority)\n' +
            '  propose --session <id> --working-directory <path> --features <path> --contract <validation-contract.json> [--branch <name>] [--mission-dir <dir>] [--skills-root <dir>] [--artifact-root <dir>] [--validators] [--skip-scrutiny] [--skip-assertions] [--gate-command <id>=<cmd>] [--review-checklist <file>] [--repair-budget <n>] [--max-concurrency <1-3>] [--worktree-root <dir>] [--review-gate] [--forge]\n' +
            '  receipt-check <receipt.json> --mission-dir <dir> [--feature <id>]\n' +
            '  end-feature (stdin receipt JSON) --mission-dir <dir> --feature <id> --worker-session <id>\n' +
            '    (env MISSION_DIR/WORKER_SESSION_ID/FEATURE_ID are the fallback for a host-launched worker)\n' +
            '  next-feature --mission-dir <dir> [--json] [--start-round] [--max-attempts <n>]\n' +
            '  claim --mission-dir <dir> --feature <id> [--json] [--brief]\n' +
            '  review-brief --mission-dir <dir> --feature <id> [--json]\n  fix-brief --mission-dir <dir> --feature <id> [--json]   (the receipt-less round after a refused merge)\n  record-review --mission-dir <dir> --feature <id> --report <review.json> [--round <n>] [--json]\n' +
            '  merge-feature --mission-dir <dir> --feature <id>\n' +
            '  reconcile-forge --mission-dir <dir>   (run the crash reconcile/resume probe keyed by prId; idempotent, never re-invokes pr merge)\n' +
            '  close-forge-ticket --mission-dir <dir> --feature <id> [--evidence <pointer>] [--status <status>]\n' +
            '  amend --mission-dir <dir> (--add-feature <feature.json> | --add-assertion <assertion.json>) [--json]\n' +
            '  cancel --mission-dir <dir> --feature <id> --reason <why>\n' +
            '  bind-authority --mission-dir <dir> --features-path <path> --contract-path <path> --reason <why>\n' +
            '    (re-point a mission at committed snapshots whose digests equal those locked at propose; digests are never rewritten)\n' +
            '  resolve-pause --mission-dir <dir> --feature <id> --reason <why> (blocked/boundary-violation/partial pauses)\n' +
            '  release --mission-dir <dir> --feature <id> [--reason <no_receipt|user_abort|...>]\n' +
            '  gate --cmd <shell cmd> [--expect fail|pass] [--target <name>] [--json] [--timeout <sec>]\n' +
            '  brief --mission-dir <dir> [--json]   (read-only: where the mission stands)\n  milestones --mission-dir <dir>\n' +
            '  scrutiny --mission-dir <dir>\n' +
            '  assertions --mission-dir <dir>\n' +
            '  record-verdicts --mission-dir <dir> --report <report.json> (--assigned <ids> | --feature <id>) [--milestone <name>] [--evidence-root <dir>]\n' +
            '  validation-check <report.json> --contract <validation-contract.json> --assigned <VAL-001,VAL-002> [--evidence-root <dir>] [--require-proven]\n' +
            '  reconcile --mission-dir <dir> --dispositions <dispositions.json>\n' +
            '  log --mission-dir <dir> <event> [--json <payload>]\n'
        );
        process.exit(2);
    }
  } catch (e) {
    // Released here, not only in `finally`: every branch below calls `process.exit`, and
    // `process.exit` does not unwind the stack, so a `finally` after it never runs. Verified, not
    // assumed. Leaving the lock held would make every ordinary refusal cost the next command a
    // 30s wait for a holder that no longer exists.
    releaseMissionLock(missionLock);
    missionLock = null;
    if (e instanceof RejectError) {
      process.stderr.write(`${e.code}: ${e.message}\n`);
      process.exit(1);
    }
    if (e instanceof InputError) {
      process.stderr.write(`input error: ${e.message}\n`);
      process.exit(2);
    }
    if (e instanceof CompileError) {
      process.stderr.write(`${e.message}\n`);
      process.exit(1);
    }
    throw e;
  } finally {
    releaseMissionLock(missionLock);
  }
}

module.exports = {
  MUTATES_MISSION,
  schemaErrors,
  dagErrors,
  validateAll,
  planDigest,
  receiptErrors,
  parseArgs,
  PLANNING_FIELDS,
  META_FIELDS,
  FEATURE_STATUS,
  FAILURE_CLASSES,
};

if (require.main === module) {
  main().catch((e) => {
    process.stderr.write(`fatal: ${e && e.stack ? e.stack : e}\n`);
    process.exit(2);
  });
}
