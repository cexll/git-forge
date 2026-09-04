# Forge P1: merge-core (ff / live-base refresh / recycle) + forge adapter

## Goal

Forge-pipeline P1 enabling set:让 `git forge pr merge` 第一次能在本仓库的真实工作流里完成集成并自己清理现场——`--ff` 策略(#9,所验即所发)、base 被检出的工作树刷新(#3)、源 worktree/branch 回收(#4),再由嵌入式 devflow adapter(#10)把 propose→PR→merge→recycle 全链接管。P0(issue 闭环 + abandon 终态)已落地,是本批的直接前置。完成后按 `pipeline-switchover-at-guard-and-recycle` 的次序,#1/#2 走新流程。

## Scope

### In scope

- `pr merge --ff`(#9):快进策略。前置 `merge_base == base_head`(快照源严格在快照 base 之上);结果 `result_commit == source_head`(不构造新提交,单亲链保持)。#2(live-base)落地前:当前 base ≠ 快照 base_head → 拒绝,消息命名 recreate-or-rebase-and-recheck;#2 落地后:ff 门按 #2 的契约对 live-base 结果重新校验(result-bound CI + result-bound approve)。
- base 工作树刷新(#3):允许 base_ref 被检出时合并。clean = base 工作树 `git status --porcelain`(默认旗标)为空;UNTRACKED 计为脏;ignored 不计;sparse-checkout 与 submodules 在 L1 范围外(docs 声明)。base CAS 成功后刷新;刷新失败 → exit 非零、refs 不变(不部分前移)、报告 worktree 为 leftover。
- 源 worktree/branch 回收(#4):`git forge pr merge <n> --recycle-worktree <path>`。path 必须是已注册 worktree 且其检出分支为 PR source_ref;错误/未注册路径拒绝、refs 不变;脏树无 `--force` 拒绝。仅当源 tip 是集成后 base tip 的祖先才删分支:`--ff`/`--merge` 删;`--squash`/`--rebase` 保留并报告原因。结构化结果行供 orchestrator:`removed | refused-dirty | refused-not-contained | partial-leftover(<path>) | branch-deleted | branch-kept(<reason>)`。时序:在 base 刷新(#3)与 finalize 之后;永不触碰 base 工作树;未合并分支的 salvage 改名属于 pipeline 侧、发生在本调用之前。
- forge adapter(#10):嵌入式 `devflow.cjs propose --forge`。八条 scope(adapter contract ADR 原文语义):owner(prId/merge 结果住 runtime.json,PR 创建在 merge-gate 时刻)、filing 幂等(`session:<s>` label + body 首行 `Ticket: <fid>`,retry 复用不重开)、proof 关闭(单条 issue comment + close)、失败分类(ci red→`gate_failed`;stale/superseded→adapter 重建,绝不 pause;bridge/parse 失败→RECOVERABLE `forge_bridge_failed`;finalize CAS 持续冲突→`forge_bridge_failed`;recycle refused/partial→`worktree_partial_removal`)、crash reconcile/resume probe(按 prId,merged-not-recorded→mark completed+mirror `forge_merged`;merged+open issue→closing comment+close;merged+remaining worktree→recycle;永不重调 `pr merge`)、recycle ownership(消费 #4 结构化结果;salvage-before-recycle)、progress-log 镜像(`forge_pr_created`/`forge_pr_superseded`/`forge_merged`)、测试矩阵(fake `git-forge` binary fixture + 双路径 parity)。
- 切换次序(#9→#3→#4 按现行方式落地,不开 forge PR;adapter 随后上线;然后 #1/#2 走新流程)。

### Out of scope

- #1(pr.update)、#2(live-base merge)、#7(machine interface)、#8(actor)——各自独立批次。
- `--squash`/`--rebase`/`--merge` 策略行为的改变(只按 #4 规则参与分支删除判定)。
- hooks(L2,未建)、worktree prune-merged 子命令形态(#4 采纳 merge 旗标,不建 follow-up 命令)。
- salvage 改名逻辑本身(pipeline 侧/devflow 已有,adapter 只保证先于 recycle)。
- sparse-checkout/submodule 工作树的刷新正确性(L1 只做拒绝面之外的文档声明)。

## Relevant Context

- 决策记录(本批变更的权威,语义逐条照录):
  - `docs/decisions/implemented/process/pipeline-switchover-at-guard-and-recycle.md`(切换次序;#9/#3/#4 旧路径落地;adapter 上线后 #1/#2 走新流程;stale-base 重建是 normal adapter action)
  - `docs/decisions/implemented/architecture/forge-pipeline-adapter-contract.md`(八 clauses:owner/filing/idempotence/labels/attribution/failure-taxonomy/crash-probe/recycle-ownership)
- Forge issues(#9/#3/#4/#10 的 spec amendment comments 是各自验收权威,上文 Scope 已照录)。
- P0 已交付并生效:`merge_state.rs` 拥有 finalize 事务;`pr abandon` 终态存在;`pr create --closes` 存在。P1 建于其上。
- Wire contract 现状:`pr.merge` body `{strategy, result_commit}`;无 `--ff` strategy;checked-out base guard 在 `docs/architecture/git-forge.md`('Checked-out base guard' + Known Limitations);merge 只清理自己的 disposable worktree(`src/pr_merge.rs` removal/verify path)。
- L1 单用户:actor 均同一 `user.email`;无权限/通知语义。

## Terms / Assumptions

- **ff 前置是快照级**:`merge_base == base_head` 比较的是 PR 快照所记 base_head 与 merge_base;moved-base 的运行时拒绝在 #2 落地前基于当前 base ref 值。
- **clean 定义**:仅 `git status --porcelain`(默认旗标)空;UNTRACKED=脏,ignored≠脏;`--force` 只解锁 #4 的脏源树,不解锁 #3 的脏 base 树。
- **recycle 只动源**:recycle 永不触碰 base 工作树;`partial-leftover` 指删除部分成功的路径级结果。
- **adapter 单 owner**:只有 orchestrator/scheduler 写 forge 状态;worker brief 禁止 `git forge` 写命令(validator 亦然,per `report-only-work-closes-issue-without-pr`)。
- **parse 失败即 pause**:#7 未落地,adapter 解析 `issue #<n> created:` / `PR #<n> created:` 散文;解析失败=RECOVERABLE pause,绝不猜。
- **标签词表不变**:`type:`/`milestone:`/`session:` 三个前缀,ticket id 住 body 首行,不进 label。
- **dogfood 是验收 oracle**:每个行为变更 ticket 同步扩展 `scripts/gf-dogfood.sh`;断言反转仅限 --ff 相关(旧 oracle 对 guard 的期望随 #3/#9 翻转,属本批明示范围)。

## Affected Surfaces

- Code:`src/pr_merge.rs`(merge 门 + 策略分派)、`src/merge_state.rs`(finalize 事务)、`src/store.rs`(worktree 刷新与回收)、`src/cli.rs`(`--ff`、`--recycle-worktree` 参数面)、`src/git.rs`(merge --ff-only / worktree 操作)、`src/issue.rs`(展示不变)。
- Data / schema:`pr.merge` body 增加 strategy `ff`(v: 1 不变);无新 kind。
- Adapter / pipeline:`devflow.cjs`(`propose --forge` + runtime.json prId/issueId 字段 + forge 子命令 plumbing + reconcile probe + 失败分类映射);implement-skill 测试矩阵增加 fake `git-forge` binary fixture。
- Tests:`tests/t3_merge.rs`、`tests/t2_pr.rs`、`scripts/gf-dogfood.sh`、implement-skill 测试矩阵(双路径 parity)。
- Docs / ops:`docs/architecture/git-forge.md`(guard 放宽 + Known Limitations:sparse-checkout/submodules 范围声明)、`~/.agents/skills/git-forge/references/wire-contract.md` 镜像 re-sync。

## Technical Direction

以下为决策-rich 细节,编码进 code 时须精确匹配(权威为各 ADR/issue 原文,此处只列落点):

1. **`--ff` 策略**(switchover ADR 命名的目标命令面 `pr merge --ff --recycle-worktree`):复用 `merge_state.rs` finalize 事务,`result_commit = source_head`;CAS 期望值 = PR 快照 base_head(#9 acceptance: exact-base CAS)。门序:ff 可行性 → moved-base 检查(#2 前)→ gate 检查(最小副作用原则,P0 同款)。
2. **base 刷新**(switchover 后主路径):base CAS 成功后,若 base_ref 检出在非临时 worktree,对该 worktree 做 clean 检查并刷新(等价 `git status --porcelain` 空 + 工作树文件前移到 result commit)。失败姿态:exit 非零、refs 保持 CAS 后的一致态并回滚不可行时整体拒绝前移——以 "refs UNCHANGED / no partial move" 注释为准:#3 的 chosen posture 是刷新失败=不产生任何 ref 前移,报告 worktree 为 leftover。
3. **recycle**(adapter clause 8 的 forge 侧):校验 registered + 检出分支 == source_ref → 脏检查(--force 例外)→ 执行移除 → 分支删除判定(ancestor-of-integrated-base)→ 输出结构化结果行。挂在 base 刷新与 finalize 之后。
4. **adapter**(clause 1–8 全量):`propose --forge` 是 mission-scoped flag,未带 flag 的 mission 保持旧路径语义到完成;失败分类按 clause 6 映射;reconcile probe 按 clause 7 的四步序,precedence = forge refs 权威、progress log 记录使命、runtime.json 由 probe 重建绝不手编。
5. **切换边界**:switchover 是一个 commit 边界,在 #4 的 OLD-PATH 集成提交消息及其 Issue comment 中宣布(无 pre-switch `--recycle-worktree` PR 可宣告)。

## Validation Plan

- VAL-101: `pr merge --ff` 在源严格位于 base 之上时合并成功且 result commit 即验证过的 source_head(无新提交、单亲链保持), Surface: cli, Evidence: merge exit 0;`pr show` 展示 result_commit==source_head;目标分支 tip 的 parent 即原 base tip
- VAL-102: 非 ff 的 PR(merge_base ≠ base_head)用 `--ff` 拒绝且错误命名 rebase/recreate 补救,refs 不变, Surface: cli, Evidence: merge exit 非零;stderr 含补救词;`pr show` 仍为未合并
- VAL-103: #2 落地前,当前 base ref 已前移(≠ 快照 base_head)时 `--ff` 拒绝并命名 recreate-or-rebase-and-recheck, Surface: cli, Evidence: 前移 base 后 merge exit 非零;stderr 含 recreate-or-rebase-and-recheck;base/PR refs 不变
- VAL-104: `--ff` 的 finalize 走 exact-base CAS:base 在 CAS 期望值之后被并发前移时合并失败、无部分写入, Surface: cli, Evidence: 注入 CAS 冲突后 merge exit 非零;base/PR/issue refs 均不变
- VAL-105: base_ref 检出在主工作树且 clean 时 `pr merge` 成功,合并后工作树文件与 result commit 一致, Surface: cli, Evidence: merge exit 0;工作树 `git status --porcelain` 为空且内容匹配 result commit
- VAL-106: base 工作树脏(含 UNTRACKED 文件)时 `pr merge` 拒绝且 refs 不变, Surface: cli, Evidence: 构造 untracked 文件后 merge exit 非零;base ref 值不变;ignored-only 文件不触发拒绝
- VAL-107: 刷新失败注入下合并 exit 非零、refs 与工作树状态一致(never split),报告 worktree 为 leftover, Surface: cli, Evidence: 注入刷新失败后 merge exit 非零;base ref 与工作树 HEAD 一致(未部分前移);输出含 leftover
- VAL-108: sparse-checkout/submodule 的 base 工作树在 L1 的行为有文档声明(不静默), Surface: docs, Evidence: `docs/architecture/git-forge.md` Known Limitations 命名 sparse-checkout 与 submodules 为 L1 范围外
- VAL-109: `--recycle-worktree` 对已注册且检出分支==source_ref 的 clean worktree 执行回收并输出 `removed`(+分支包含时 `branch-deleted`), Surface: cli, Evidence: merge exit 0;输出行含 removed;worktree 目录不存在;`git worktree list` 无该项
- VAL-110: 回收拒绝面:未注册/错误分支的 path 拒绝且 refs 不变;脏树无 `--force` 拒绝(`refused-dirty`), Surface: cli, Evidence: 三次调用 exit 非零;输出行分别为 refused-not-contained/refused-dirty;PR 未合并、base ref 不变
- VAL-111: 分支删除仅当源 tip 是集成后 base tip 的祖先:`--ff`/`--merge` 删分支,`--squash`/`--rebase` 保留并报告原因(`branch-kept`), Surface: cli, Evidence: --ff 后 `git rev-parse <source_ref>` 失败;--squash 后分支仍在且输出 branch-kept(<reason>)
- VAL-112: 回收结果行覆盖六态枚举且 `partial-leftover(<path>)` 在移除部分失败时报告路径;recycle 永不触碰 base 工作树, Surface: cli, Evidence: 注入移除失败输出含 partial-leftover;合并后 base 工作树 `git status --porcelain` 为空且 HEAD==result commit
- VAL-113: `propose --forge` 在 merge-gate 时创建 PR:runtime.json 获 prId/issueId;重复 propose 不重开 Issue(`session:<s>` label + body 首行 `Ticket: <fid>` 命中即复用), Surface: cli, Evidence: fake git-forge fixture 下 propose exit 0;第二次 propose 输出复用(forge 侧 issue 数不变)
- VAL-114: 失败分类按 clause 6:ci red→gate_failed;stale/superseded→adapter 重建(forge_pr_superseded,新快照,非 pause);bridge/parse 失败→RECOVERABLE forge_bridge_failed; Surface: cli, Evidence: fixture 注入三类失败,devflow 停点/pause reason 逐类匹配,绝不 blanket gate_failed
- VAL-115: proof ticket 经 adapter 以单条 issue comment + close 关闭;validator/orchestrator 不运行 git forge 写命令, Surface: cli, Evidence: fixture 记录 write 调用仅来自 adapter;close 前 comment 恰一条
- VAL-116: crash reconcile/resume probe:merge 成功后崩溃恢复,按 prId 探测 forge refs 权威重建——merged-not-recorded→completed+forge_merged;merged+open issue→comment+close;merged+剩余 worktree→recycle;任何分支不重调 `pr merge`, Surface: cli, Evidence: 三种中断态注入后 probe 收敛;fixture 中 `pr merge` 调用次数不增
- VAL-117: 双路径 parity:同一 feature 在旧路径(无 --forge)与新路径(--forge)下走完,progress-log 镜像事件(forge_pr_created/forge_pr_superseded/forge_merged)可重建 recreate 窗口;fake git-forge fixture 支撑 implement-skill 测试矩阵, Surface: cli, Evidence: 双路径各跑一遍 exit 0;progress log 含三种镜像事件;测试矩阵绿
- VAL-118: 切换次序契约:#9→#3→#4 以旧路径落地(无 forge PR),adapter(#10)先于 #1/#2 的新流程 PR 存在, Surface: docs, Evidence: switchover ADR 声明 + 验收时 #1/#2 的 PR 走 `pr create --closes`→merge 新流程(recreate 行为属 #10 已证路径)

## Risks / Open Questions

- **风险:guard 翻转的 oracle 面**——#3 放宽 checked-out base guard 后,旧 dogfood 断言中"检出即拒绝"的期望必须随本批反转;这是 P0 spec 预告的"断言反转仅发生在 #3/#9 批次"的落点,需在 t1 同 PR 内完成,避免 oracle 与代码脱节窗口。
- **风险:刷新失败姿态与 CAS 的交界**——#3 chosen posture 是"刷新失败=refs UNCHANGED",但 base CAS 已成功;实现必须把刷新放进 CAS 后的同一致态语义(docs 用词以 issue #3 原文为准),t2 需带注入测试证明 never split。
- **风险:adapter 是跨仓库改动**(devflow.cjs 住 implement skill,不在 git-forge 仓库)——t3 的 Touches 声明与验收都发生在 implement-skill 测试矩阵面;dogfood oracle 对 adapter 只能以 fixture parity 证明。
- **开放**:`--recycle-worktree` 是否也接受 `--squash` 之外的 keep 场景列表扩展(#1/#2 落地后 live-base 结果会改变 ancestor 判定的输入)——本批按 #4 amendment 原文四策略硬表实现。
- **开放**:probe 的 runtime.json 重建精度(哪些字段必重建)由 t3 实现 + fixture 断言敲定,spec 层只固定四步序与"绝不手编"。

## Mission Handoff

- Suggested milestones: merge-core, adapter, acceptance
- Required evidence:
  - 每 feature:对应测试绿 + `scripts/gf-dogfood.sh` 全绿(基线扩展含 --ff/recycle 断言;guard 翻转断言随 #3/#9 更新)。
  - t3:implement-skill 测试矩阵(含 fake git-forge fixture)绿 + 双路径 parity 证明。
  - 收口:验收票逐断言报告 + 断言账本全 proven。
- Human gates:
  - `pr_merge.rs`/`merge_state.rs`/`store.rs` 白盒人审(每 feature 合并前,由 orchestrator 提交人审)。
  - switchover 宣布提交(#4 的 OLD-PATH 集成提交消息 + Issue comment)是人工边界。