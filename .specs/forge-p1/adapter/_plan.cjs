'use strict';
// The plan and its runtime live in two files now: the committed source the mission was proposed
// with, and missionDir/runtime.json. Tests that used to read one snapshot read them joined here,
// so the join lives in one place rather than in every test that needs a status.
const fs = require('node:fs');
const path = require('node:path');

function planWithRuntime(missionDir) {
  const state = JSON.parse(fs.readFileSync(path.join(missionDir, 'state.json'), 'utf8'));
  const doc = JSON.parse(fs.readFileSync(state.featuresSourcePath, 'utf8'));
  const rtPath = path.join(missionDir, 'runtime.json');
  const rt = fs.existsSync(rtPath)
    ? JSON.parse(fs.readFileSync(rtPath, 'utf8'))
    : { features: {}, injected: [] };
  doc.features = [...(rt.injected || []), ...doc.features];
  for (const f of doc.features) Object.assign(f, (rt.features || {})[f.id] || {});
  return doc;
}

/** featureId -> status, the shape most assertions actually want. */
function statusesOf(missionDir) {
  return planWithRuntime(missionDir).features
    .reduce((acc, f) => Object.assign(acc, { [f.id]: f.status }), {});
}

/**
 * Persist a features doc the way the scheduler does: runtime state to runtime.json, planning
 * fields left alone. Tests that used to hand-write the snapshot to fake an in-flight worker write
 * through this instead -- the snapshot is not the source of anything any more.
 */
function writeRuntimeFrom(missionDir, doc) {
  const RUNTIME = ['status', 'workerSessionIds', 'currentWorkerSessionId', 'branch', 'worktree', 'prId', 'issueId'];
  const rtPath = path.join(missionDir, 'runtime.json');
  const prev = fs.existsSync(rtPath)
    ? JSON.parse(fs.readFileSync(rtPath, 'utf8'))
    : { features: {}, injected: [] };
  const state = JSON.parse(fs.readFileSync(path.join(missionDir, 'state.json'), 'utf8'));
  const planned = new Set(JSON.parse(fs.readFileSync(state.featuresSourcePath, 'utf8')).features.map((f) => f.id));
  const out = { features: {}, injected: [] };
  for (const f of doc.features) {
    const rt = {};
    for (const k of RUNTIME) if (f[k] !== undefined) rt[k] = f[k];
    out.features[f.id] = rt;
    if (!planned.has(f.id)) {
      const rec = {};
      for (const k of Object.keys(f)) if (!RUNTIME.includes(k)) rec[k] = f[k];
      out.injected.push(rec);
    }
  }
  out.injected = out.injected.length ? out.injected : (prev.injected || []);
  fs.writeFileSync(rtPath, JSON.stringify(out, null, 2) + '\n');
}

/**
 * Edit a planning field where the plan actually lives now — the committed source. There is no
 * snapshot in missionDir to tamper any more, which is the point: the plan is one file, and
 * planDigest is what says it has not moved since propose.
 */
function tamperPlan(missionDir, mutate) {
  const state = JSON.parse(fs.readFileSync(path.join(missionDir, 'state.json'), 'utf8'));
  // A mission owns its plan file now -- there is no snapshot standing between them. A test that
  // proposed against a shared fixture would therefore rewrite that fixture for every other test.
  // Refuse loudly rather than corrupting the suite's inputs, which is what happened once.
  if (state.featuresSourcePath.includes(`${path.sep}fixtures${path.sep}`)) {
    throw new Error(
      `tamperPlan would rewrite a shared fixture (${state.featuresSourcePath}). ` +
      'Propose against a per-test copy: a mission owns its plan file.'
    );
  }
  const doc = JSON.parse(fs.readFileSync(state.featuresSourcePath, 'utf8'));
  mutate(doc);
  fs.writeFileSync(state.featuresSourcePath, JSON.stringify(doc, null, 2) + '\n');
  return doc;
}

/**
 * A private copy of a plan file. A mission owns its plan for its lifetime -- there is no snapshot
 * standing between them any more -- so a test that will tamper with the plan must not propose
 * against a fixture every other test reads.
 */
function copyPlan(srcPath, intoDir, name) {
  fs.mkdirSync(intoDir, { recursive: true });
  const dest = path.join(intoDir, name || `plan-${path.basename(srcPath)}`);
  fs.copyFileSync(srcPath, dest);
  return dest;
}

module.exports = { planWithRuntime, statusesOf, writeRuntimeFrom, tamperPlan, copyPlan };
