#!/usr/bin/env node
'use strict';
// Fake `git-forge` binary for the devflow forge-adapter test matrix (t3 / VAL-113..117).
//
// Placed first on PATH it intercepts exactly `git forge ...` invocations (git runs the
// `git-forge` executable from PATH for the `forge` subcommand) while every real `git`
// call passes through untouched. It emulates the git-forge CLI surface the adapter
// uses, persists forge state as JSON, records every invocation in a call log, and
// performs the REAL git side effects an --ff merge has (base ref CAS, worktree
// removal, branch deletion) so dual-path parity tests observe true repository state.
//
// State directory: $FORGE_FAKE_STATE (required). Files:
//   issues.json     { counter, issues: [{id,title,body,labels,open,comments}] }
//   prs.json        { counter, prs: [{id,title,body,labels,source,base,closes,ci,decision,merged,abandoned}] }
//   calllog.jsonl   one JSON line per invocation: { ts, argv, caller, ok, cwd }
//   scenario.json   consume-once failure injections (see below)
//
// Injection scenarios (each consumed on first use, so tests stay deterministic):
//   issueNewFail     issue new exits 1 (bridge failure; filing retry must not duplicate)
//   prCreateFail     pr create exits 1 (bridge failure)
//   prCreateGarbage  pr create exits 0 with unparseable output (parse failure)
//   ciExecuteFail    ci run exits 1 WITHOUT recording a check (failure to execute)
//   staleBase        pr merge refuses with the moved-base message (recreate path)
//   reviewFail       pr review --approve exits 1 (bridge failure)
//   mergeFail        pr merge refuses with a generic bridge error
//   partialRecycle   merge succeeds but the recycle reports partial-leftover and keeps the worktree
//   recycleDirtyToctou merge succeeds but phase-2 recycle refuses with the TOCTOU refused-dirty
//             wording (merge terminal, worktree stays)
//   recycleDirty     the recycled worktree is reported dirty even when clean (refused-dirty)
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const stateDir = process.env.FORGE_FAKE_STATE;
if (!stateDir) {
  process.stderr.write('fake-git-forge: FORGE_FAKE_STATE is required\n');
  process.exit(2);
}
fs.mkdirSync(stateDir, { recursive: true });

function readJson(name, fallback) {
  try {
    return JSON.parse(fs.readFileSync(path.join(stateDir, name), 'utf8'));
  } catch {
    return fallback;
  }
}
function writeJson(name, value) {
  fs.writeFileSync(path.join(stateDir, name), JSON.stringify(value, null, 2) + '\n');
}

// `git forge ...` runs `git-forge` with argv after the subcommand name; tolerate a
// literal 'forge' leading token too (direct invocation during debugging).
let argv = process.argv.slice(2);
if (argv[0] === 'forge') argv = argv.slice(1);
const [group, sub, ...rest] = argv;

// Parse `--flag value` / bare `--flag`; a repeated flag collects into an array
// (--label is repeatable on issue new / pr create, exactly like the real forge).
function parse(args) {
  const flags = {};
  const positionals = [];
  const set = (name, value) => {
    if (!Object.prototype.hasOwnProperty.call(flags, name)) flags[name] = value;
    else if (Array.isArray(flags[name])) flags[name].push(value);
    else flags[name] = [flags[name], value];
  };
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a.startsWith('--')) {
      const eq = a.indexOf('=');
      if (eq >= 0) {
        set(a.slice(2, eq), a.slice(eq + 1));
      } else if (i + 1 < args.length && !args[i + 1].startsWith('--')) {
        set(a.slice(2), args[++i]);
      } else {
        set(a.slice(2), true);
      }
    } else {
      positionals.push(a);
    }
  }
  return { flags, positionals };
}
const list = (v) => (v === undefined ? [] : Array.isArray(v) ? v : [v]);

function git(args, cwd) {
  const r = spawnSync('git', args, { cwd: cwd || process.cwd(), encoding: 'utf8' });
  return { ok: r.status === 0, stdout: (r.stdout || '').trim(), stderr: (r.stderr || '').trim() };
}

const repo = process.cwd();
const gitDir = git(['rev-parse', '--git-dir']);
if (!gitDir.ok) {
  process.stderr.write(`fake-git-forge: not a git repository: ${repo}\n`);
  process.exit(1);
}

const issues = readJson('issues.json', null) || { counter: 0, issues: [] };
const prs = readJson('prs.json', null) || { counter: 0, prs: [] };
const scenario = readJson('scenario.json', {});

const call = { ts: new Date().toISOString(), argv: argv.slice(), caller: process.env.DEVFLOW_FORGE_CALLER || 'external', ok: true, cwd: repo };

function finish(status, out, err) {
  call.ok = status === 0;
  fs.appendFileSync(path.join(stateDir, 'calllog.jsonl'), JSON.stringify(call) + '\n');
  writeJson('issues.json', issues);
  writeJson('prs.json', prs);
  writeJson('scenario.json', scenario);
  if (out) process.stdout.write(out + '\n');
  if (err) process.stderr.write(err + '\n');
  process.exit(status);
}

/** Consume a named scenario once: true when it was armed. */
function take(name) {
  if (scenario[name] === true) {
    delete scenario[name];
    return true;
  }
  return false;
}

function issueShowText(i) {
  let out = `#${i.id} ${i.title} — ${i.open ? 'open' : 'closed'}\n`;
  if (i.body) out += `description: ${i.body}\n`;
  if (i.labels.length) out += `labels: ${i.labels.join(', ')}\n`;
  if (i.comments.length) {
    out += '\ncomments:\n';
    for (const c of i.comments) out += `  - ${c}\n`;
  }
  return out.trimEnd();
}

function prShowText(p) {
  let out = `PR #${p.id} ${p.title} — ${p.decision ? `decision: ${p.decision}` : 'no review yet'}\n`;
  if (p.merged) out += `merged: ${p.merged}\n`;
  if (p.abandoned) out += `abandoned: ${p.abandoned}\n`;
  out += `ci: ${p.ci}\n`;
  out += `base: ${p.base}\n`;
  out += `source: ${p.source}\n`;
  // Real forge contract (cli.rs cmd_pr_show): the immutable snapshot pair.
  // The adapter's CR-06 stale-snapshot check parses the source OID from it.
  out += `diff: ${p.base_head}...${p.source_head}\n`;
  if (p.labels.length) out += `labels: ${p.labels.join(', ')}\n`;
  if (p.closes) out += `closes: #${p.closes}\n`;
  for (const c of p.comments) out += `comment: ${c}\n`;
  return out.trimEnd();
 }

if (group === 'issue') {
  const { flags, positionals } = parse(rest);
  if (sub === 'new') {
    if (take('issueNewFail')) finish(1, '', 'fake-forge: bridge down (issueNewFail)');
    const title = positionals[0] || '';
    const body = positionals[1] || '';
    const labels = list(flags.label);
    const id = ++issues.counter;
    issues.issues.push({ id, title, body, labels, open: true, comments: [] });
    finish(0, `issue #${id} created: ${title}`);
  } else if (sub === 'list') {
    if (!issues.issues.length) finish(0, '(no issues)');
    finish(0, issues.issues.map((i) => `#${i.id} ${i.title} (${i.open ? 'open' : 'closed'})`).join('\n'));
  } else if (sub === 'show') {
    const id = Number(positionals[0]);
    const i = issues.issues.find((x) => x.id === id);
    if (!i) finish(1, '', `issue #${id} does not exist`);
    finish(0, issueShowText(i));
  } else if (sub === 'comment') {
    const id = Number(positionals[0]);
    const body = positionals.slice(1).join(' ').trim();
    const i = issues.issues.find((x) => x.id === id);
    if (!i) finish(1, '', `issue #${id} does not exist`);
    i.comments.push(body);
    finish(0, `comment added to issue #${id}`);
  } else if (sub === 'close' || sub === 'reopen') {
    const id = Number(positionals[0]);
    const i = issues.issues.find((x) => x.id === id);
    if (!i) finish(1, '', `issue #${id} does not exist`);
    if (take('issueCloseFail')) finish(1, '', 'fake-forge: bridge down (issueCloseFail)');
    i.open = sub === 'reopen';
    finish(0, `issue #${id} ${sub === 'close' ? 'closed' : 'reopened'}`);
  }
  finish(2, '', 'usage: git forge issue <new|list|show|comment|close|reopen> ...');
}

if (group === 'pr') {
  const { flags, positionals } = parse(rest);
  if (sub === 'create') {
    if (take('prCreateFail')) finish(1, '', 'fake-forge: bridge down (prCreateFail)');
    const source = flags.source;
    const base = flags.base;
    if (!source || !base || !positionals.length) {
      finish(2, '', 'usage: git forge pr create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]... [--closes <n>]');
    }
    const src = git(['rev-parse', `refs/heads/${source}`]);
    const bas = git(['rev-parse', `refs/heads/${base}`]);
    if (!src.ok || !bas.ok) finish(1, '', `fake-forge: unknown source/base branch (${source} -> ${base})`);
    const mb = git(['merge-base', bas.stdout, src.stdout]);
    const id = ++prs.counter;
    const pr = {
      id,
      title: positionals.join(' '),
      body: flags.body || '',
      labels: list(flags.label),
      source,
      base,
      source_head: src.stdout,
      base_head: bas.stdout,
      merge_base: mb.ok ? mb.stdout : '',
      closes: flags.closes !== undefined && flags.closes !== true ? Number(flags.closes) : null,
      ci: 'pending',
      decision: null,
      merged: null,
      abandoned: null,
      comments: [],
    };
    prs.prs.push(pr);
    if (take('prCreateGarbage')) finish(0, 'forge emitted something unparseable');
    finish(0, `PR #${id} created: ${pr.title} (${source} -> ${base})`);
  } else if (sub === 'list') {
    if (!prs.prs.length) finish(0, '(no PRs)');
    finish(0, prs.prs.map((p) => `#${p.id} ${p.title} (${p.merged ? 'merged' : p.abandoned ? 'abandoned' : 'open'})`).join('\n'));
  } else if (sub === 'show') {
    const id = Number(positionals[0]);
    const p = prs.prs.find((x) => x.id === id);
    if (!p) finish(1, '', `PR #${id} does not exist`);
    // CR-08: `prShowFail` is a PERSISTENT injection for the transient-failure
    // test — a consume-once flag would be eaten by the reconcile probe's own
    // `pr show` before the merge gate re-runs. The test clears it by removing
    // the scenario file.
    if (scenario.prShowFail === true) finish(1, '', 'fake-forge: bridge down (prShowFail)');
    finish(0, prShowText(p));
  } else if (sub === 'review') {
    const id = Number(positionals[0]);
    const p = prs.prs.find((x) => x.id === id);
    if (!p) finish(1, '', `PR #${id} does not exist`);
    if (take('reviewFail')) finish(1, '', 'fake-forge: bridge down (reviewFail)');
    p.decision = flags.approve ? 'approve' : flags.reject ? 'reject' : null;
    finish(0, `review recorded on PR #${id}: ${p.decision}`);
  } else if (sub === 'comment') {
    const id = Number(positionals[0]);
    const p = prs.prs.find((x) => x.id === id);
    if (!p) finish(1, '', `PR #${id} does not exist`);
    p.comments.push(positionals.slice(1).join(' '));
    finish(0, `comment added to PR #${id}`);
  } else if (sub === 'abandon') {
    const id = Number(positionals[0]);
    const p = prs.prs.find((x) => x.id === id);
    if (!p) finish(1, '', `PR #${id} does not exist`);
    if (p.merged) finish(1, '', `PR #${id} is already merged`);
    p.abandoned = flags.reason || 'abandoned';
    finish(0, `PR #${id} abandoned`);
  } else if (sub === 'merge') {
    const id = Number(positionals[0]);
    const p = prs.prs.find((x) => x.id === id);
    if (!p) finish(1, '', `PR #${id} does not exist`);
    if (p.merged) finish(1, '', `PR #${id} is already merged`);
    if (p.abandoned) finish(1, '', `PR #${id} is already abandoned: ${p.abandoned}`);
    if (p.decision !== 'approve') finish(1, '', `PR #${id} requires an approving review before merge`);
    if (p.ci !== 'success') {
      finish(1, '', `PR #${id} is not mergeable: latest CI Check is \`${p.ci}\` (expected \`success\`); run \`git forge ci run ${id}\``);
    }
    if (!flags.ff) finish(2, '', 'fake-forge: only --ff is emulated');
    if (take('staleBase')) {
      const cur = git(['rev-parse', `refs/heads/${p.base}`]);
      finish(1, '', `base branch '${p.base}' has moved since PR #${id} was created (${cur.stdout} != snapshot ${p.base_head}); recreate the PR or merge manually`);
    }
    if (take('casConflict')) {
      finish(1, '', `fake-forge: finalize CAS conflict persists after retry on PR #${id} (update-ref rejected)`);
    }
    if (take('mergeFail')) finish(1, '', 'fake-forge: bridge down (mergeFail)');
    // recycle pre-validation (#4 phase 1): containment + dirty check, before any side effect.
    const recyclePath = flags['recycle-worktree'] && flags['recycle-worktree'] !== true ? flags['recycle-worktree'] : null;
    if (recyclePath) {
      let dirty = false;
      if (take('recycleDirty')) dirty = true;
      else {
        const st = git(['status', '--porcelain'], recyclePath);
        dirty = st.ok && st.stdout.length > 0;
      }
      const wl = git(['worktree', 'list', '--porcelain']);
      const block = wl.stdout.split('\n\n').find((b) => b.startsWith(`worktree ${recyclePath}\n`) || b.trim() === `worktree ${recyclePath}`);
      const branchLine = block ? block.split('\n').find((l) => l.startsWith('branch ')) : null;
      if (!block || branchLine !== `branch refs/heads/${p.source}`) {
        finish(1, '', `recycle refused (refused-not-contained): '${recyclePath}' is not a registered worktree checked out on the PR source branch '${p.source}'; the worktree is untouched, the PR is not merged`);
      }
      if (dirty && !flags.force) {
        finish(1, '', `recycle refused (refused-dirty): worktree '${recyclePath}' has uncommitted changes; commit, stash or clean it, or pass --force to recycle anyway`);
      }
    }
    // ff precondition (real forge pr_merge.rs): the snapshot source must sit
    // DIRECTLY on top of the snapshot base (merge_base == base_head), before
    // any CAS. A source cut from an older base refuses with its own remedy.
    if (p.merge_base && p.merge_base !== p.base_head) {
      finish(1, '', `PR #${id} cannot fast-forward: the source does not sit directly on top of the snapshot base (merge_base != base_head); use \`pr merge --rebase\` to replay the source or recreate the PR`);
    }
    // --ff merge: CAS the base ref from the snapshot base_head to the snapshot source_head.
    const cas = git(['update-ref', `refs/heads/${p.base}`, p.source_head, p.base_head]);
    if (!cas.ok) {
      const cur = git(['rev-parse', `refs/heads/${p.base}`]);
      finish(1, '', `base branch '${p.base}' has moved since PR #${id} was created (${cur.stdout} != snapshot ${p.base_head}); recreate the PR or merge manually`);
    }
    // #3 live-base refresh: any worktree with the base branch checked out is
    // refreshed to the result commit, or the main worktree's index/HEAD go stale
    // against the moved ref (git status would show a phantom 'D <file>').
    const wl = git(['worktree', 'list', '--porcelain']);
    for (const block of wl.stdout.split('\n\n')) {
      if (block.startsWith('worktree ') && block.includes(`branch refs/heads/${p.base}`)) {
        const wtPath = block.split('\n')[0].slice('worktree '.length);
        const refresh = git(['reset', '--hard'], wtPath);
        if (!refresh.ok) finish(1, '', `fake-forge: base worktree refresh failed: ${refresh.stderr}`);
      }
    }
    p.merged = p.source_head;
    if (p.closes) {
      const issue = issues.issues.find((x) => x.id === p.closes);
      if (issue) issue.open = false;
    }
    // recycle phase 2 (#4): remove the worktree, delete the branch per the ancestry rule.
    // Phase-2 TOCTOU (merge_state.rs): dirt appears between the pre-merge
    // validation and the removal. The merge is TERMINAL (the output carries the
    // merged line) but the exit is non-zero and the worktree stays.
    if (recyclePath && take('recycleDirtyToctou')) {
      finish(1, '', `merged PR #${id} into ${p.base} (${p.source_head}); recycle refused (refused-dirty): worktree '${recyclePath}' has uncommitted changes (dirt appeared after the pre-merge validation); the worktree is untouched, commit/stash/clean it or pass --force to recycle anyway`);
    }
    if (recyclePath && take('partialRecycle')) {
      finish(1, '', `merged PR #${id} into ${p.base} (${p.source_head}); recycle outcome: partial-leftover(${recyclePath}): worktree still present after removal`);
    }
    let outcome = null;
    if (recyclePath) {
      const rm = git(['worktree', 'remove', recyclePath]);
      if (!rm.ok) {
        finish(1, '', `merged PR #${id} into ${p.base} (${p.source_head}); recycle outcome: partial-leftover(${recyclePath}): worktree removal failed: ${rm.stderr}`);
      }
      const tip = git(['rev-parse', `refs/heads/${p.source}`]);
      const anc = tip.ok ? git(['merge-base', '--is-ancestor', tip.stdout, p.source_head]) : { ok: false };
      if (tip.ok && tip.stdout === p.source_head && anc.ok) {
        git(['branch', '-D', p.source]);
        outcome = `removed; branch-deleted (${p.source})`;
      } else if (!tip.ok) {
        outcome = `removed (branch ${p.source} absent)`;
      } else {
        outcome = `removed; branch-kept(${p.source}: its tip is not an ancestor of the integrated result commit — the branch holds unmerged work; salvage renaming is pipeline-side)`;
      }
    }
    finish(0, `merged PR #${id} into ${p.base} (${p.source_head})${outcome ? `; recycle outcome: ${outcome}` : ''}`);
  }
  finish(2, '', 'usage: git forge pr <create|list|show|comment|review|diff|merge|abandon> ...');
}

if (group === 'ci') {
  const { positionals } = parse(rest);
  if (sub !== 'run') finish(2, '', 'usage: git forge ci run <n>');
  const id = Number(positionals[0]);
  const p = prs.prs.find((x) => x.id === id);
  if (!p) finish(1, '', `PR #${id} does not exist`);
  if (take('ciFail')) {
    p.ci = 'failed';
    finish(1, '', `ci run recorded: failed on PR #${id}`);
  }
  if (take('ciExecuteFail')) {
    // Failure to EXECUTE (spawn failure / forge crash): no Check is recorded,
    // the PR's CI state is untouched, and the error names no recorded outcome.
    finish(1, '', 'fake-forge: bridge down (ciExecuteFail)');
  }
  p.ci = 'success';
  finish(0, `ci run recorded: success on PR #${id}`);
}

if (!group) finish(0, 'usage: git forge <issue|pr|ci> ... (fake git-forge for the devflow test matrix)');
finish(2, '', `fake-git-forge: unknown command: ${argv.join(' ')}`);
