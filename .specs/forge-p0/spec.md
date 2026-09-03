# Forge P0: issue closure in finalize transaction + PR abandon terminal state

## Goal

给 forge 补上 PR→Issue 的生命周期闭环:合并时同事务关闭声明的 Issue(#5),以及 PR 的 abandon 终态(#6)。两者是 forge-pipeline 集成(P0+P1 批次)中唯二改变状态机的特性,也是 devflow adapter(`--forge`)的前置。

## Scope

### In scope

- `pr create --closes <issue>`:声明 PR 与 Issue 的关闭关系,`pr show` 展示。
- 合并成功时,在同一 `update-ref --stdin` 事务中写入 `issue.close`(该 Issue 声明被此 PR closes)。
- 合并失败(gate red、冲突、CAS 重试耗尽)时,任何 Issue 状态不变——无部分写入。
- `pr abandon <id> --reason <text>` 新命令:abandoned 是 PR 终态,merge/review/ci run 全部拒绝,不提供 reopen。
- 终态与幂等规则:已 merge 的 PR 拒绝 abandon;已 abandon 的 PR 拒绝二次 abandon;abandon 拒绝时 refs 不变。
- size-gate 前置:模块拆分腾出 800-code-line 余量(prefactor,行为不变)。

### Out of scope

- issues #1(pr.update)、#2(live-base 深优)、#3(guard 放宽)、#4(recycle)、#7(machine interface)、#8(actor)、#9(--ff)、#10(adapter)——各自独立批次。
- Issue 的 abandon 终态(non-goal,触发器:需要按终态过滤 issue 视图)。
- PR 的 approve/reject 与 closes 的联动(关闭只随 merge,不随 review)。
- `pr created` 快照语义的改变(仍不可变;新提交=新 PR)。
- GitHub/Web UI/里程碑/assignees。

## Relevant Context

- 决策记录(本批变更的权威):
  - `docs/decisions/implemented/feature/issue-close-in-finalize-transaction.md`
  - `docs/decisions/implemented/feature/pr-abandon-terminal-state.md`
  - `docs/decisions/implemented/architecture/forge-pipeline-adapter-contract.md`(clause 6/7 消费这两个状态)
- Wire contract 现状:`pr.created` body 无 closes 字段;`issue.close` body `{}`;`pr.merge` body `{strategy, result_commit}`;无 `pr.abandoned` kind。`store.finalize_pr_merge` 单事务 = delete `/result` + CAS `refs/heads/<base>` + CAS PR `/head`(store.rs L941-973)。
- L1 单用户:actor 都是同一个 `user.email`;无权限/无通知语义。
- 现有测试面:`tests/t0_core.rs`(event/fold)、`t1a_store.rs`、`t1b_issue.rs`、`t2_pr.rs`、`t3_merge.rs`;`scripts/gf-dogfood.sh` 45/45 断言是可执行 oracle。

## Terms / Assumptions

- **closes 基数**:一个 PR 至多关闭一个 Issue(`--closes <n>` 单数,可重复参数留待需求);devflow 投影是 1 票 1 issue,1:1。
- **closes 目标校验**:`--closes` 的 id 必须已存在(未知 id 拒绝,不创建);目标 Issue 的当前状态(open/closed)不影响 create。
- **close 事件 schema 不变**:`issue.close` body 保持 `{}`,只改变"何时由谁写"——写入点是 finalize 事务而非 `git forge issue close` 独立命令。两者并存:手工 close 仍可用。
- **reopen 不受影响**:merge 事务关闭后,用户仍可 `issue reopen`(现行行为,不做限制)。
- **abandon reason 必填**:空/whitespace-only reason 拒绝。
- **abandon 的 CI 交互**:abandon 拒绝 `ci run` 时**不追加任何 ci.check**(与 `.forge/ci.sh` 非 regular file 的拒绝同语义);此前的 pending/success/failed check 保留展示。
- **dogfood 是验收 oracle**:每个行为变更的 ticket 必须同步更新 `scripts/gf-dogfood.sh`(新增断言),45/45 基线随之扩展;断言反转仅发生在本 spec 之外(#3/#9 批次)。

## Affected Surfaces

- Code:`src/cli.rs`(pr 命令面)、`src/pr_merge.rs`(合并门)、`src/store.rs`(finalize 事务)、`src/event.rs`(kind/fold)、`src/fold.rs`(PrState)、`src/issue.rs`(展示)。
- Data / schema:事件 schema v1 扩展——`pr.created.body` 可选 `closes`;新 kind `pr.abandoned`;两者均保持 `v: 1`(向后兼容,可选字段不破坏旧 reader)。
- API / CLI / UI:`git forge pr create --closes <n>`;`git forge pr abandon <n> --reason <text>`;`pr show` 增加 closes/abandoned 展示。
- Tests:`tests/t0_core.rs`、`t1a_store.rs`、`t1b_issue.rs`、`t2_pr.rs`、`t3_merge.rs`、`scripts/gf-dogfood.sh`。
- Docs / ops:`docs/architecture/git-forge.md`(wire-contract 表 + 已知限制)、`~/.agents/skills/git-forge/references/wire-contract.md` 镜像 re-sync、`CONTEXT.md` glossary(Issue Projection 不涉及本批;abandon 语义词条)。

## Technical Direction

以下为决策-rich 细节,编码进 code 时须精确匹配:

1. **`pr.created.body` 扩展**:可选字段 `"closes": <issue-id>`(整数)。仅当 `--closes` 提供时存在;`v` 保持 `1`。fold 时 PrState 增加 `closes: Option<u64>` 透出。
2. **新 kind `pr.abandoned`**:`entity: "pr"`,body `{"reason": "<text>"}`。fold:PrState 增加 `abandoned: Option<String>`(取链上最后一个 abandoned 事件的 reason);abandoned 为终态。
3. **拒绝检查顺序(所有读态命令之外)**:abandoned → 拒绝(消息命名 reason)。检查先于任何事件写入;`pr merge` 的 checked-out/stale/gate 检查保持现状,abandoned 检查先于 gate 检查(最小副作用)。
4. **finalize 事务扩展(store 层)**:存在 closes 目标时,同一 `update-ref --stdin` 批次追加第三步:按当前 issue 链 tip 为 parent 构造 `issue.close` 提交(树 = `.forge/event.json`,消息 `forge:issue.close:<id>`),CAS `refs/forge/issues/<n>` 期望 tip。事务失败=整体不应用=issue 保持原状。CAS 重试(现有 MAX_FINALIZE_RETRIES 环)复用同一批构造。
5. **幂等 no-op**:merge 时目标 Issue 已 closed(state.open == false)→ 不在批次中追加 close 提交(仅 base/PR 两个动作),merge 照常成功;不产生第二个 close 事件。
6. **`pr.abandoned` 写入**:单 append 到 PR `/head` 链,复用现有 CAS 重试环(同一 UUID 保留)。
7. **命令面**:
   - `git forge pr create --source <b> --base <b> <title> [--body <t>] [--label <x>]... [--closes <n>]`。
   - `git forge pr abandon <n> --reason <text>`(新命令,`run_pr` 分发增加分支)。
   - `pr show <n>`:已有字段之外,展示 `closes: #<n>`(若有)与 `abandoned: <reason>`(若有)。

## Validation Plan

- VAL-001: PR create 声明 closes 后,pr show 展示 closes 目标, Surface: cli, Evidence: `git forge pr show <n>` 输出含 `closes: #<issue>`
- VAL-002: closes 目标 Issue 在 PR 合并成功后变为 closed, Surface: cli, Evidence: `git forge issue show <n>` 输出 closed;merge exit 0
- VAL-003: 目标 Issue 已 closed 时合并仍成功且不写第二个 close 事件, Surface: cli, Evidence: merge exit 0;issue show 仍 closed(事件链长度不变,fold 值不变)
- VAL-004: 合并失败时(门未过/冲突)目标 Issue 保持 open, Surface: cli, Evidence: 构造 merge 失败后 `issue show` 仍 open;merge exit 非零
- VAL-005: abandon 后 pr show 展示 abandoned 与 reason, Surface: cli, Evidence: `git forge pr show <n>` 输出含 `abandoned: <reason>`
- VAL-006: abandon 后 merge 拒绝、退出非零、错误消息命名 reason, Surface: cli, Evidence: `git forge pr merge <n>` exit 非零,stderr 含 reason
- VAL-007: abandon 后 review 拒绝, Surface: cli, Evidence: `git forge pr review <n> --approve` exit 非零
- VAL-008: abandon 后 ci run 拒绝且不追加 ci.check, Surface: cli, Evidence: `git forge ci run <n>` exit 非零;`pr show` 的 CI 状态不变(仍为 create 时的 pending)
- VAL-009: 二次 abandon 拒绝,不写第二个 abandoned 事件, Surface: cli, Evidence: 第二次 `pr abandon` exit 非零;`pr show` 展示同一(首个)reason
- VAL-010: 已 merge 的 PR 拒绝 abandon, Surface: cli, Evidence: merge 成功后 `pr abandon` exit 非零

## Risks / Open Questions

- **风险:事务基座交互**(store.rs finalize 是白盒人审面)。t2 必须带 store 层单元测试(构造事务失败探针),不以 CLI 冒烟代替白盒断言。
- **风险:dogfood 基线扩展方向**——本批只增断言不改断言语义(翻转留给 #3/#9)。
- **开放**:`--closes` 是否需要支持 PR 列表(一个 PR 多个 Issue)由 #10 adapter 的投影形态验证后决定;本批单数。
- **开放**:abandon 后 `pr list` 的展示(是否标注)未纳入本批断言,实现可按 show 同步 list 展示(不列为断言)。

## Mission Handoff

- Suggested milestones: p0-core, acceptance
- Required evidence:
  - 每 feature:对应测试绿 + `scripts/gf-dogfood.sh` 全绿(45+新增断言)。
  - 收口:验收票逐断言报告 + 断言账本全 proven。
- Human gates:
  - store.rs / pr_merge.rs 白盒人审(每 feature 合并前,由 orchestrator 提交人审)。
  - `docs/architecture/git-forge.md` wire-contract 更新必须与代码同 PR(否则 Known Limitations 失读)。
