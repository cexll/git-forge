# Forge P2: self-bootstrap via the forge pipeline (#7 machine interface, #8 actor, #1 pr.update, #2 live-base merge)

## Goal

Switchover 后的第一批自举:git-forge 仓库自己的剩余 L2 issues 全部走**新 forge PR 流程**(`propose --forge` → worktree → merge-gate `pr create --closes` → `ci run` → `review` → `pr merge --ff --recycle-worktree`)。本批按依赖序落地:#7(机器接口,adapter 的 prose 解析先替换为 JSON/version-gate)→ #8(actor 区分)→ #1(pr.update,每代快照)→ #2(live-base merge,重绑 gate 到 merge 结果)。每个 feature 自己通过新流程合并,PR 关闭其 issue——自举。

## Scope

### In scope

- **#7 machine interface**:`--json` on `issue new/list/show`、`pr create/list/show`、`ci run`(机器可读结果,含分配 id/status;`pr create` JSON 含分配 PR 号);`git forge version` 打印 semver 退出 0;capabilities discovery DEFERRED(adapter 改为 semver range version-gate);未知 flag 保持 exit-1;裸 `git forge --help` exit-1 quirk 保留并文档化(help 在 `git forge help`);每个 JSON-enabled 命令一个 byte-stable schema fixture。
- **#8 actor distinguishability**:event actor 从单一 `user.email` 提升为可区分角色(worker vs reviewer vs orchestrator)。wire contract 声明;identity 解析支持 per-worktree/环境覆盖(`extensions.worktreeConfig`、`GIT_CONFIG_COUNT` 等),成为 promised interface。
- **#1 pr.update**:`pr update` 事件把 PR 重新 pin 到分支 tip 作为新快照代;每代不可变(refs/forge/prs/<n>/source/g<k> 布局或等价);事件 body 记录 old/new source_head、base_head、merge_base;fold 暴露最新代;diff = merge_base..latest source_head;review 与 CI Check 绑定代;只有最新代的 approve+check 满足 Merge Gate(旧代 stale 拒绝);测试 red-first。
- **#2 live-base merge**:base 已前移时不拒绝,而是对当前 base tip 重算 merge_base、执行策略、materialize result、把 CI/review 绑定到 merge 结果(跑 CI plan against result tree,或证明 result tree == CI 检查的 tree);finalize CAS 当前 gate 验证的 base tip(F-007/F-008 gate_head 绑定重设计,实现中命名);确定性验收:两个 PR 同一 base,先合并后第二个 base 已前移——除非 re-validated 否则拒绝,re-validated 后 fresh CI + fresh review 通过。

### Out of scope

- hooks(L2 未建)、pr.update 之外的 snapshot 布局迁移、capabilities 发现机制(version-gate 取代)、非本批的 CLI 散文格式改动。
- #3/#4/#9/#10 已完成,不改动其行为(除非 #2 的 gate 重设计显式命名触及)。

## Relevant Context

- Issue spec amendments(验收权威,逐条照录):#7(1)–(5)、#8(identity 覆盖面)、#1 per-generation snapshot contract(1)–(5)、#2 result-bound review/CI(1)–(3)。
- 决策记录:`pipeline-switchover-at-guard-and-recycle.md`(#1/#2 走新流程;stale-base recreate 是 normal adapter action)、`forge-pipeline-adapter-contract.md`(八 clauses;#7 落地后 parse 失败面缩小)。
- Adapter 现状:devflow.cjs 解析 `issue #<n> created:` / `PR #<n> created:` 散文;#7 落地后 adapter 切到 --json + version-gate。
- Wire contract:`pr.merge` body `{strategy, result_commit}`;event actor = repo user.email(#8 改)。

## Terms / Assumptions

- **代(generation)**:pr.update 每次调用产生新不可变快照代;L1 快照 refs 保持 generation 0 不可变。
- **result-bound gate**:approve 与 CI Check 绑定其验证的代/结果;只有最新代且绑定当前 merge 结果的 gate 输入有效。
- **version-gate**:adapter 依赖 `git forge version` 的 semver 判定可用旗标(替代能力探测)。
- **JSON 是 additive**:散文输出保留(--json 是并行面,非替换);byte-stable schema fixture 是契约。

## Affected Surfaces

- Code:`src/cli.rs`(--json/version/help)、`src/issue.rs`、`src/pr_merge.rs`(pr update、live-base)、`src/merge_state.rs`(result-bound CAS/gate)、`src/event.rs`/`src/fold.rs`(代绑定、actor)、`src/identity.rs`(actor 区分)、`src/store.rs`(代 refs 布局)。
- Data / schema:`pr.update` 新 kind;`pr.merge` gate 输入绑定代;event actor 字段扩展。
- Adapter:`devflow.cjs`(--json parse + version-gate + 可能的 actor 传参)。
- Tests:`tests/t3_merge.rs`、`tests/t2_pr.rs`、`scripts/gf-dogfood.sh`、adapter matrix。
- Docs:`docs/architecture/git-forge.md`、`~/.agents/skills/git-forge/references/wire-contract.md` 镜像。

## Technical Direction

1. **#7**:`--json` 在 create/list/show 上输出 stable schema(分配 id 从 JSON 取,不再 sed 散文);`git forge version` 输 semver;未知 flag exit-1 不变;schema fixture 每个命令一个(byte-stable)。
2. **#8**:identity 解析从单一 user.email 变为可角色化;wire contract 声明 actor 字段的 role 语义;per-worktree/environment 覆盖成为 promised interface。
3. **#1**:pr.update 建新代 ref;事件 body old/new 快照;fold 暴露最新代;review/CI 绑定代;gate 只吃最新代。
4. **#2**:merge_base 对当前 base 重算;result materialize 后 CI 绑定 result;review 绑定 result;finalize CAS 当前 gate 验证的 base tip;F-007/F-008 绑定重设计。
5. **自举顺序**:#7 → #8 → #1 → #2,每 feature 通过新流程 merge(PR closes issue);#1/#2 的 acceptance 按 pipeline 术语(与并行 wave、fix round 交互)书写。

## Validation Plan

- VAL-201: `--json` on issue new/list/show + pr create/list/show + ci run 输出 byte-stable schema,含分配 id;`git forge version` 输 semver 退出 0, Surface: cli, Evidence: 每个命令 schema fixture 逐字节匹配;version 输出格式稳定
- VAL-202: 未知 flag 仍 exit-1;裸 `git forge --help` exit-1 quirk 文档化(help 在 `git forge help`), Surface: cli, Evidence: 注入未知 flag exit 非零;docs 声明 quirk
- VAL-203: adapter 在 #7 落地后切到 --json 解析 + version-gate(不再 sed 散文), Surface: pipeline, Evidence: adapter matrix 用 --json fixture 全绿;version-gate 逻辑测试
- VAL-204: #8 actor 区分:同一 repo 中 worker/reviewer/orchestrator 的事件 actor 可区分;wire contract 声明 role 语义, Surface: cli+docs, Evidence: 三种角色事件链 actor 互异;docs 更新
- VAL-205: #1 pr.update 把 PR 重新 pin 到分支 tip 为新代;事件 body 记录 old/new source_head/base_head/merge_base, Surface: cli, Evidence: update 后 show 显示新快照;事件 body 含四字段
- VAL-206: #1 代绑定 gate:approve-before-update 不能 merge post-update 内容;CI-green-before-update 同样 stale;仅最新代 approve+check 满足 gate, Surface: cli, Evidence: 先 approve 再 update 再 merge → 拒绝;update 后重新 approve+CI → 通过
- VAL-207: #2 live-base merge:base 前移后 merge 对当前 base 重算并绑定 result-bound CI+review 通过;未 re-validate 拒绝, Surface: cli, Evidence: 双 PR 场景第二个 PR base 前移 → 无 fresh CI 拒绝;fresh CI+review 后通过
- VAL-208: #2 的 finalize CAS 绑定当前 gate 验证的 base tip(F-007/F-008 重设计命名);并发 CAS 冲突无部分写入, Surface: cli, Evidence: CAS 冲突注入 exit 非零 refs 不变
- VAL-209: 自举契约:#7/#8/#1/#2 全部通过新 forge 流程合并,PR closes issue;adapter 全程使用 --json/version-gate, Surface: pipeline, Evidence: 每个 feature 的 PR merged + issue closed;adapter matrix 绿

## Risks / Open Questions

- **#1/#2 交互**:live-base merge 与 per-generation snapshot 的 gate 绑定语义叠加(update 后 base 又前移);本批按各自 amendment 实现,#2 的 result-bound gate 覆盖。
- **#7 的散文兼容**:现有 adapter 散文解析在切 JSON 前的过渡窗口;版本判定先行,避免 flag 探测猜测。
- **#8 的 identity 覆盖**:per-worktree 配置在现有 repo 布局的验证成本;以 wire contract 声明 + 最小实现验证。

## Mission Handoff

- Suggested milestones: machine-interface, identity, pr-update, live-base, acceptance
- Required evidence: 每 feature 测试绿 + dogfood 绿;adapter matrix 绿(#7/#8 切换后);自举 PR merged + issue closed 记录。
- Human gates: `cli.rs`/`merge_state.rs`/`event.rs` 白盒人审;switchover 后首个自举 PR 的人工确认。
