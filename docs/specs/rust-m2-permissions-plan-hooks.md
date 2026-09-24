# Rust M2：执行模式、权限、plan、Bash 只读判定、hooks 与工作区信任

总设计见 [P0/P1 架构设计](rust-p0-p1-architecture.md) 4.5、5.5、5.12。

原则：**逐项对齐 Node 现有行为**，包括第 6 节列出的已知缺陷（2026-09-23 确认）。本文引用的 Node 路径以 `apps/zcode-cli/packages/` 为根，`shared/` 指 `packages/shared/src/`。

凡是需要逐字一致的文本、表格与判定结果，都由生成脚本调用 Node 实现导出为 JSON 夹具，再由 Rust 表驱动测试比对，不在 Rust 中手抄：

- 提示词与模型可见文本；
- 工具能力元数据；
- 权限判定矩阵；
- Bash 策略表与命令注册表；
- hook digest 向量。

## 1. 交付顺序与能力声明

| 子项 | 内容                                                                                                  | 完成后声明                                   |
| ---- | ----------------------------------------------------------------------------------------------------- | -------------------------------------------- |
| M2.1 | 执行状态：模式与 plan 开关的唯一变更路径、投影、持久化                                                | —                                            |
| M2.2 | 权限策略、规则、交互选项、全权限                                                                      | —                                            |
| M2.3 | Bash 只读判定、"始终允许"前缀建议、Bash 规则匹配、git 安全检查                                        | `permissionModes: [build, edit, yolo, auto]` |
| M2.4 | Plan 工具、plan 审批、计划文件、提醒、`v4/conversation/plans`                                         | `independentPlanState: true`                 |
| M2.5 | Hooks：7 种事件、执行、输出合并、模型可见结果                                                         | —                                            |
| M2.6 | 项目 hooks 的工作区信任：digest、信任存储、准入、审查流程、4 个 V4 命令、`workspace/hooks/trustGrant` | —                                            |

- M2.2 完成、M2.3 未完成期间，Bash 一律按非只读处理（build 下每条命令都询问）。这是更保守的方向，能力也尚未声明，App 仍按 yolo-only 门禁。
- M2.3 完成后，`runtime/capabilities.independentPlanState` 与执行能力的声明同 Node 等价：
  - Node 不声明执行能力，App 因而视全部模式可用；
  - Rust 声明全部四种模式，效果相同；
  - `rust-app-execution-modes.md` 的"Rust 只声明 yolo"随之更新。

## 2. M2.1 执行状态

依据 `shared/execution-state.ts`、`core/src/runtime/execution-state.ts`、`core/src/runtime/methods/{config,resume,turn-model,subagent}.ts`、`bootstrap/src/zcode-protocol-v4/commands/handlers/{model-config,goal-compact,session-flow}.ts`。

### 2.1 状态与归一化

- `ExecutionState { mode: build | edit | yolo | auto, plan_enabled: bool }`。`plan` 不是存储的模式，只是输入别名。
- `resolve(input, current)` 与 Node `resolveExecutionState` 相同：
  - `mode`：输入 mode 属于四种有效模式时取之，否则保留当前；
  - `plan_enabled` 依次取：
    1. 输入显式给出的值；
    2. 输入 mode 为 `plan` 时为 `true`；
    3. 输入 mode 为有效模式时为 `false`，即只发 `{mode:"edit"}` 会关闭 plan；
    4. 其余情况保留当前值。

### 2.2 唯一变更路径

所有变更都经 Engine 的 `apply_execution_state(session, input, cause)`，`cause` 为 `command` 或 `tool{tool_call_id}`。顺序与 Node `applyRuntimeExecutionState` 一致：

1. 全权限事务进行中时拒绝，错误文本为 `Permission update is busy; retry mode change`。
2. 计算 `next`。若 mode 与 plan 都未变，直接返回，不投影也不持久化。
3. 开启 plan 且会话有 `active` 目标时拒绝，错误文本为 `Plan and Goal cannot be active at the same time.`。暂停的目标允许。
4. 写会话状态。会话元数据本身随持久化事务一起提交，失败时内存不变。
5. plan 发生变化时设置 `needs_plan_exit_reminder = !next.plan_enabled`，供下一次模型请求使用，不落盘。
6. 投影 V4 `config.mode`、`config.planEnabled`；来源为工具时同时写 `config.planTransition = {toolCallId, planEnabled}`。

调用方：

| 路径                                                                     | 行为                                                                                                                                                                                                                                                                        |
| ------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `switchCollaborationMode {mode: build\|edit\|plan\|yolo}`                | CAS 命令。`mode` 等于当前 mode 且 plan 关闭时，ACK 为 `noop / config.unchanged`。否则调用 `set_mode`：应用 `{mode}`，再写项目模式偏好（保存的是应用后的权限模式，永不为 `plan`，写失败只记 warn）。异常时 ACK 为 `failed / fault.command.executionFailed`，并带 `message`。 |
| `createSession.config.mode/planEnabled`                                  | 仅当 mode ∈ {build, edit, plan, yolo} 或给出了 planEnabled 时应用；不写项目偏好。                                                                                                                                                                                           |
| 输入 intent（`sendText`、`sendGoalCommand`、`createSession.firstInput`） | admission 时把 payload 的 mode/planEnabled 与当前状态合成冻结值（`auto` 视为 `build`）写入 intent；run 开始或引导消息排空时以 `command` 来源应用。                                                                                                                          |
| EnterPlanMode / ExitPlanMode                                             | 见第 5 节。                                                                                                                                                                                                                                                                 |
| 全权限                                                                   | 见 3.6。                                                                                                                                                                                                                                                                    |
| 目标命令                                                                 | intent 带 `planEnabled:true` 的 `sendGoalCommand` 返回 `rejected / guard.planGoalMutuallyExclusive`。其余目标命令先应用 `{mode, planEnabled:false}`。plan 开启时 `resumeGoal` 被拒绝；plan 开启期间跳过目标续跑。                                                           |

### 2.3 持久化与恢复

- 会话的 `mode` 与 `plan_enabled` 与 Node 一样存为会话行 `permission` 与 `runtime/execution_state` entry（spec rust-m11-node-storage §6.1）。
- **项目模式偏好**：Node 的 `local_setting(scope="project", scope_id=<projectID>, namespace="permission", key="mode")`，值 `{"mode": ...}`；
  `projectID` 按 Node `projectIdFromDirectory(workspacePath)` 计算（小写后把 `[^a-z0-9._-]+` 替换为 `-`，去掉首尾 `-`，空时为 `session`，截取 80 字符）。
- 新会话的初始模式为：会话创建参数 → 项目偏好 → 配置 `permission.mode` → `build`，结果经 `resolve` 归一化。

### 2.4 子代理

- 父模式取 `plan`（父会话 plan 开启时）或父会话的 mode。
- 子代理模式按 profile 的 `permissionMode` 决定：
  - `auto` 或 `plan` 时取对应值；
  - 未指定时，内置 Explore 为 `yolo`，其他子代理继承父模式。
- 子会话的状态为 `mode = (子模式为 plan ? 父 mode : 子模式)`，`plan_enabled = (子模式为 plan)`。
- 项目来源的 profile 忽略 `permissionMode`。
- 状态在创建时确定，父会话之后的变化不传递给子代理。

## 3. M2.2 权限策略

依据 `core/src/permission/{service,plan-mode-policy,rule-matching,workflow-draft-path}.ts`、`core/src/tool/executor/permission-*.ts`、`bootstrap/src/permission-options.ts`、`bootstrap/src/zcode-protocol/interaction-broker.ts`、`bootstrap/src/zcode-protocol-v4/{interaction-registry,product-projection}.ts`。

### 3.1 工具能力元数据

- 每个工具的能力元数据由脚本从 Node `builtInTools` 导出为 `crates/domain/fixtures/tool-capabilities.json`，Rust 只加载已实现的工具。字段：
  - `readOnly`、`destructive`、`allowedInPlanMode`、`requiresUserInteraction`、`alwaysAsk`
  - `sideEffectScope`、`riskLevel`、`needsApproval`
  - `permission`（类别名）
- 缺字段时按 `service.ts:583-611` 的默认值推断，与 Node 相同。
- MCP 工具：`readOnly = readOnlyHint === true`，`destructive = destructiveHint === true`，scope 为 `network`，`needsApproval = true`，类别为 `mcp`。风险：destructive 为 high，readOnly 为 low，其余为 medium。
- Bash 的只读命令在运行时改写为 `{readOnly: true, destructive: false, needsApproval: false, riskLevel: low, sideEffectScope: none}`（第 4 节）。

### 3.2 PolicyEngine（domain 纯函数）

输入为一份快照 `{mode, plan_enabled, project_rules, session_rules, allowed_tools, disallowed_tools, auto_approve_high_risk, working_directory}` 和调用视图 `{tool, input, capability}`。输出 `{decision: allow|ask|deny, rule_id, reason, risk_level, side_effect_scope, always_ask?}`。

判定顺序与 Node `checkPermission` 一致：

1. plan 过渡工具：
   - EnterPlanMode 一律 allow；
   - plan 关闭时 ExitPlanMode 为 deny。
2. `requiresUserInteraction`：在 `disallowedTools` 中则 deny，否则 ask。
3. `alwaysAsk` 分支（`checkAlwaysAsk`）：auto 模式、disallowed、项目 deny 判 deny；会话规则判 allow；其余 ask。
4. yolo 且 plan 关闭时 allow。
5. auto 模式 deny，理由为 `Auto mode is reserved but not implemented yet`。
6. `disallowedTools` 精确匹配工具名则 deny。
7. 项目 deny 规则。
8. 项目 ask 规则。
9. plan 开启时进入 plan 阶梯，后续步骤均不再执行：
   - 只读且非破坏性，allow；
   - 非破坏性 MCP，allow；
   - 显式声明的会话能力，allow；
   - 其余 deny。
10. 项目 allow 规则。
11. WebFetch 预批准的 URL（主机表由脚本导出）。
12. 工作流草稿写入。
13. `allowedTools`。
14. edit 模式下，`permission == "edit"` 且 scope 为 `workspace` 的工具 allow；其余落入 build。
15. build 阶梯：
    1. 只读且无需审批，allow；
    2. critical 风险 ask；
    3. high 风险且未开启 `autoApproveHighRisk` 时 ask；
    4. session 范围的低风险状态更新 allow；
    5. 有副作用 ask；
    6. 其余 allow。

策略判定之后的调整：

- PreToolUse hook（M2.5）：`allow` 把 ask 升为 allow（`alwaysAsk` 除外），`ask` 把 allow 降为 ask。
- 记忆目录下 Markdown 文件的写入放行（记忆功能不在 M2 范围，保留入口）。

每一步的 `ruleId` 与 `reason` 文本由脚本从 Node 导出的判定矩阵夹具校验。

### 3.3 规则

- **结构**：`{toolName, ruleContent?}`。项目规则集为 `{version: 1, allow?, deny?, ask?}`，按 `toolName + "\0" + ruleContent` 去重，保留首个。
- **来源**：
  - 项目规则：Node 的 `local_setting ... key="ruleset"`（旧 `permission` 表只读兼容）；
  - 会话规则：只在内存中，只参与 `alwaysAsk` 分支；
  - 配置的 `permission.allowedTools` / `disallowedTools`：按工具名精确匹配。
- **匹配**（`service.ts:233-320`、`rule-matching.ts`）：
  - `toolName` 必须相等，唯一的别名是 `Edit` 规则也匹配 `Write`；
  - 没有 `ruleContent` 的规则直接命中；
  - 匹配对象：
    - 输入本身是字符串时取整个输入；
    - WebFetch 取 `domain:<host 小写且去掉末尾的点>`；
    - 其余取 `command, url, file_path, path, pattern, patch_text` 中第一个字符串字段；
  - 以 `:*` 结尾的规则做前缀匹配，前缀后必须是结尾、空格或制表符；
  - 含 `*` 的规则转为锚定的通配正则；
  - 其余精确相等；
  - Bash 使用专用的规则求值（第 4 节）。
- **写入**：所有规则写入都经 Engine 串行完成，与交互解决在同一持久化事务中提交。这不改变 Node 的判定结果，只是不会像 Node 那样丢失更新。

### 3.4 管线与所有者

```mermaid
sequenceDiagram
    participant L as run task
    participant W as watch<PermissionSnapshot>
    participant P as PolicyEngine（domain）
    participant E as Engine（actor）
    participant T as ToolPort
    L->>L: 1 查找工具、校验输入（失败不进入权限）
    L->>W: 2 读取本会话快照（Arc 共享，无往返）
    L->>P: 3 check(snapshot, call)
    alt deny
        L->>E: PermissionDenied{call, reason}
        E->>E: 行状态 cancelled；模型收到 deny 文本
    else ask
        L->>E: Permission{call, decision, suggestions}
        E->>E: 注册交互（waiters）+ 行 pendingApproval + pending interaction
        E-->>L: 决定（allow / deny{reason} / 修改后的输入）
        E->>E: allowAlways → 写项目规则；allowSession → 会话规则；fullAccess → 3.6
    end
    L->>T: 4 以获批输入执行
```

- 快照的所有者是 Engine：每个会话一个 `watch::Sender<Arc<PermissionSnapshot>>`。
- 模式变更、规则写入、配置重载时，Engine 发布新快照；run task 每次工具调用只读一次。

### 3.5 交互选项与投影

- **pending interaction**：
  - `kind: "permission"`；
  - `payload` 为 `{kind, toolCallId, toolName, summary: 判定理由, detail: 输入, freeText: true, options, fullAccessOption?, origin?, display?}`。
- **选项**（按顺序）：
  - `allowOnce`（kind `allowOnce`，文案 `Allow once`）；
  - `allowAlways`（kind `allowAlways`，文案 `Always allow in this project`）；
  - `allowSession`（kind `allowAlways`，只在 `askOptions.allowAlways == "session"` 时替代 allowAlways）；
  - `deny`（kind `deny`，文案 `Deny`）。
- **`fullAccessOption`** 为 `{optionId: fullAccess, label: "Full access", kind: custom}`，只在非子代理请求且无选项策略时提供。
- **工具行**：询问时为 `pendingApproval` 并带 `approvalInteractionId`；解决后 allow 为 `running`，deny 为 `cancelled`；策略直接 deny 时也为 `cancelled`。deny 不产生工具错误事件，但模型会收到 deny 文本作为工具结果。
- **`resolveInteraction` 的答案映射**（`interaction-broker.ts:147-193`）：
  - `allowOnce` 为 allow；
  - `allowAlways` 为 allow，并按建议写入项目规则。写入失败时与 Node `permission-flow.ts` 一致：交互仍按允许解决（行回到 `running`），但工具不执行，模型收到 `Failed to persist project permission update`，行为 `error`；内存中的项目规则保持旧值；
  - `allowSession` 为 allow，并写会话规则（整个工具）；
  - `deny` 或无法识别的答案为 deny，理由为 `PERMISSION_DENIED_BY_USER_CONTENT`；非空的 freeText 追加为 ` To tell you how to proceed, the user said:\n<feedback>`。
- **ACK**：一律 `accepted` 且不带 `result`；重复或未知的 id 幂等成功（`snoozeInteractionAutoResolution` 同理）。Rust 保留会话归属检查：发往非宿主会话的应答同样返回 `accepted`，但不产生效果。
- **模型可见的 deny 文本**：
  - 用户拒绝时使用上面的文本，逐字保留；
  - 策略拒绝时使用规则理由；
  - 没有理由时为 `Permission denied for <tool>`；
  - 其余文本折叠空白，超过 500 字符截为 497 个字符加 `...`。
- **建议规则**：
  - Bash 按第 4 节；
  - 其余工具为整个输入字符串，或 `command, url, file_path, path, pattern` 中第一个非空字段；
  - WebFetch 保存完整 URL，因而永不命中（D5，保持 Node）。

### 3.6 全权限

`resolveInteraction{optionId: "fullAccess"}` 由 Engine 在一个持久化事务内完成：

1. 会话 mode 设为 `yolo`，`plan_enabled` 保持不变；
2. 当前排队输入的 `mode` 改写为 `yolo`；
3. 投影 `config.permissionGrant = {interactionId}`；
4. 被选中的交互按 allowOnce 解决。

- 前置条件：没有进行中的全权限、没有队列提升或引导排空；否则 ACK 为 `failed`，错误文本为 `Queue mutation is busy; retry approval`。
- 同一交互重复提交视为幂等。
- 不写项目偏好。
- 与 Node 一致，只放行当前这一条（D6）。

### 3.7 子代理

- Explore 使用独立的空策略：没有会话规则，没有 `allowed` / `disallowed` 列表，模式为 yolo。
- 其余子代理共享父会话的会话规则，模式按 2.4 确定。
- 子代理的询问路由到根会话，payload 带 `origin`，锚定根会话中对应的 Agent 工具行；不提供 `fullAccessOption`。
- 根会话的 run 结束时只清理自己的待决交互；仍由子会话持有的请求保留在根会话上，直到子会话应答或释放（`release_waiters` 同时撤下宿主上的条目）。
- 待决条目先写入宿主会话，再发布工具行与状态，保证同一帧的 `state.updated` 已含该请求。

## 4. M2.3 Bash 只读判定

依据 `core/src/tool/handlers/bash-*.ts`、`unbash@4.0.1`、`generated/bash-command-registry.ts`。

- **解析**：移植 unbash 4.0.1 的词法与语法分析，覆盖以下部分，并逐一复现 Node 的取值与容错行为：
  - 词值：反斜杠保留、ANSI-C 转义；
  - 动态部分识别；
  - 重定向与 heredoc；
  - 语句、`&&` / `||`、管道、`!` 与 `time`；
  - 出错时的恢复。

  复合结构（子 shell、`if`、`for`、函数、`[[ ]]`、`(( ))`、后台 `&` 等）只需识别为不支持，一律判非只读。

- **判定**：`is_read_only(command, cwd)` 与 `isRuntimeReadOnlyBashCommand` 一致：
  - 命令需满足可判定的前提：无解析错误、无不支持语法、无动态词；
  - 同一行里 git 与 `cd`/`pushd`/`popd` 共存时判非只读；
  - 调用 git 时，当前目录的 git 运行环境需安全（`bash-git-runtime-safety.ts`，同步文件检查）；
  - 每个调用都需通过写选项检查与只读策略。
- **策略表**：由脚本从 TS 导出，导出的是解析后的对象：
  - 60 条命令策略、46 条任意参数命令、24 条 git 子命令、24 条多词命令；
  - 1029 项安全参数；
  - fig 命令注册表（707 个根命令）。

  输出为 JSON 并以 `--check` 防止漂移；约 20 个危险回调按名称手工移植，启动时断言每个名称都有实现。

- **"始终允许"建议**（`bash-command-permission-policy.ts`）：
  - 为每个非只读调用计算稳定前缀，形如 `<prefix>:*`，最多 5 条；
  - 无法计算前缀时退回精确命令；
  - 包装命令最多展开两层；
  - 高危根命令不生成前缀；
  - 按深度覆盖表处理子命令深度，其余按命令注册表遍历。
- **规则求值**（`bash-command-rule-evaluator.ts`）：
  - 精确命令或无内容的规则直接命中；
  - 不可判定的命令不匹配任何前缀规则；
  - allow 要求每个非只读调用都被覆盖；
  - deny / ask 只要任一调用命中。
- **保留的 Node 行为**：
  - `sed s/…/w file` 判只读；
  - 带重定向或 `$` 的命令不匹配前缀规则；
  - `git status:*` 放行 `git -C <任意目录> status`；
  - 当前目录含 `refs/` 时，git 命令判非只读；
  - 任意参数命令表遮蔽同名命令的逐条策略；
  - `hostname` 的正则针对完整命令文本。
- **原型链键**：JS 对象查找会命中原型链，例如 `constructor foo` 在 Node 中抛异常。Rust 使用自有键查找，不模拟这个异常，该命令按普通未知命令处理（非只读，使用精确规则）。这一处是唯一的有意差异，原因是异常属于实现崩溃，不是产品行为。
- **夹具**：生成脚本经 tsx 直接调用 Node 的解析、判定与策略模块作为对照源，生成约 300 条命令的 `{analysis, readOnly, suggestions, ruleResults}`，并在临时目录中构造 git 场景。

### 4.1 模块与所有者

- `crates/bash-parse`（`zcode-cli-bash-parse`）：unbash 4.0.1 词法与语法的移植，以及 `bash-command-parser.ts` 的遍历，对外只暴露 `analyze(command) -> Analysis` 与 `is_permission_safe`。纯函数，无 IO。
  - 约束：Node 判为可判定（无解析错误、无不支持语法、无动态词）时，`Analysis` 全部字段逐一相同；Node 判为不可判定时，Rust 也必须判为不可判定，其余字段不参与任何决策。
  - 长度上限与 `trim` 按 JS 语义：10 000 个 UTF-16 单元，`String.prototype.trim` 的空白集合。
- `crates/bash`（`zcode-cli-bash`）：只读策略、策略表、危险参数回调、始终允许建议、fig 命令注册表与 Bash 规则求值。纯函数，无 IO；git 运行环境是否安全作为输入传入。
  - 策略表与注册表由生成脚本从 Node 导出为 JSON（解析后的对象，保留顺序），以 `include_str!` 嵌入并在首次使用时解析；回调按名称绑定，启动测试断言每个名称都有实现。
- `crates/tools`：Bash 工具的权限入口。按会话当前 Bash 工作目录做 git 运行环境检查（异步文件 IO，只在命令调用 git 时执行，同一次判定只查一次），然后调用 `zcode-cli-bash` 得到能力与规则策略。
- `ToolPort::permission(session, name, args)`（异步）返回 `{capability, rules, suggestions}`。默认实现由同步的静态能力 `capability()` 与通用建议组成；工具端口只为 Bash 覆盖它。Core 的 `authorize` 把 `rules` 传给 `Snapshot::check`，替换临时的精确匹配。
- Rust 的 Bash 始终在工作区目录执行（尚未实现 Node 的 `cd` 持久化），git 运行环境检查因此使用工作区目录。

```mermaid
sequenceDiagram
    participant R as run task (authorize)
    participant T as tools::ToolPort
    participant G as git safety (async fs)
    participant B as zcode-cli-bash
    participant P as domain Policy
    R->>T: permission(session, "Bash", args)
    T->>B: analyze(command)
    alt 某个调用是 git
        T->>G: unsafe_context(cwd)
        G-->>T: bool
    end
    T->>B: read_only(analysis, git_unsafe) / rule_policy(...)
    B-->>T: capability, BashRules, suggestions
    T-->>R: ToolPermission
    R->>P: Snapshot::check(tool, input, capability, BashRules)
    P-->>R: Decision
```

- 声明：M2.3 通过全部夹具与集成测试后，`permissionModes` 改为 `[build, edit, yolo, auto]`。

## 5. M2.4 Plan

依据 `contracts/src/tools/plan-mode.ts`、`core/src/tool/handlers/{plan-mode,plan-mode-prompts}.ts`、`core/src/runtime/helpers/{runtime-reminders,plan-file-continuity}.ts`、`core/src/tool/executor/turn-control.ts`、`bootstrap/src/zcode-protocol/interaction-broker.ts:269-446`。

- **工具**：主会话始终注册，子代理中始终移除。
  - EnterPlanMode：schema 为 `{}`（strict）。
  - ExitPlanMode：`{plan: 1..20000 且非空白, allowedPrompts?}`，允许额外字段。`allowedPrompts` 只回显，不授予任何权限。
  - 描述、模型可见结果与提醒文本全部由脚本从 TS 导出。
- **EnterPlanMode**：权限一律 allow；以 `tool` 来源应用 `{planEnabled: true}`，mode 不变；有进行中的目标时工具失败。
- **ExitPlanMode**：
  - plan 关闭时 deny。
  - plan 开启时询问，pending interaction 为 `kind: "userInput"`，`schema: {interaction: "plan_approval", toolName: "ExitPlanMode"}`，问题文本为权限理由 `Tool ExitPlanMode requires user interaction`，唯一选项为 `approve`，并允许自由文本。
  - 答案映射：
    - `action: accept`：内容为 `approve` 时批准，为其他非空文本时视为反馈；
    - `allowOnce` / `allowAlways`：批准；
    - 非空 freeText：视为反馈；
    - 其余（包括 `optionId: "approve"`）：拒绝。
  - 批准后依次：
    1. 写计划文件 `<workspace>/.zcode/plans/plan-<清理后的会话 id>.md`，原子写，写入失败静默忽略；
    2. 以 `tool` 来源应用 `{planEnabled: false}`，mode 不变；
    3. 返回 `User has approved your plan…## Approved Plan:\n<plan>`。
  - 拒绝且带反馈：工具结果为 `The plan was not approved by the user.`，反馈作为引导消息进入同一轮，plan 保持开启。
  - 拒绝且无反馈：结果为 `Permission denied for ExitPlanMode`，本轮在提交工具结果后结束，同批后续工具以 `Tool cancelled because a previous tool result requested a turn stop.` 取消。
- **提醒**（只在内存中，不落盘）：
  - `runtime_mode`：plan 开启时每次模型请求检查。当没有先前提醒，或自上次提醒后已有至少 5 条真实用户消息时注入：第 1、6、11… 次用完整文本，其余用精简文本。
  - `plan_mode_exit`：plan 关闭后的下一次模型请求注入一次。
- **压缩后**：无论 plan 是否开启，只要计划文件存在且非空，就注入计划文件提醒。读取上限 81024 字节；除文件不存在外的读取错误都会使压缩失败。
- **`v4/conversation/plans`**：返回 `{plans, atSeq, atLogEpoch}`：
  - 取状态为 success / error / cancelled 且 `plan` 非空的 ExitPlanMode 工具行；
  - 按 `rowId` 降序；
  - 不分页。
- **目标**：互斥规则见 2.2。

### 5.1 Rust 结构与所有者

- **文本**：工具描述、schema、结果文本、三类提醒正文与提醒节奏，由生成脚本从 Node 导出到 `crates/domain/schema/plan-mode.json`；`domain::plan_mode` 提供纯函数（提醒节奏、计划审批答案映射、计划文件名、结果文本）。
- **工具定义**：tools crate 在主会话的工具列表中加入两个工具；子代理（带 profile 的会话）在 agent loop 中移除它们。
- **执行**：两个工具由 core 在 run task 中处理，状态变更经事件交给 Engine：
  - EnterPlanMode：`Event::PlanMode {enable: true}`，Engine 以 `tool` 来源调用 `apply_execution_state`，失败（目标进行中）时工具失败并返回错误文本；
  - ExitPlanMode：先由权限层走审批；批准后 run 检查权限快照中的 plan 开关（已关闭则返回 Node 的 `You are not in plan mode…` 错误），调用 `ToolPort::write_plan_file`（原子写，失败静默），再发送 `Event::PlanMode {enable: false}`。
- **审批**：Engine 登记权限请求时，ExitPlanMode 投影为 `kind: "userInput"` 的待决交互（不带 `options` 与 `fullAccessOption`）；`resolveInteraction` 按 Node 映射为批准、带反馈拒绝或无反馈拒绝（`PermissionAnswer::PlanRejected`）。
  - 带反馈：工具结果为 `The plan was not approved by the user.`，行为 `cancelled`；反馈记在会话运行态，下一个步骤边界由 Engine 作为引导输入（userInput 行 + 用户消息）交给 run，与 Node `steerTurn(guide)` 等价。
  - 无反馈：工具结果为 `Permission denied for ExitPlanMode`，`ToolOutput.stop_turn` 置位；同一步其余工具以 `Tool cancelled because a previous tool result requested a turn stop.` 取消，本轮在提交结果后结束。
- **提醒**：run 在每次模型请求前按权限快照判断：
  - plan 开启时按 Node 节奏插入 `runtime_mode` 提醒（完整 / 精简）；
  - 快照中 `plan_exit_pending` 为真时插入一次退出提醒；
  - 插入位置是当时历史末尾，作为运行期临时消息参与后续请求，不写入会话消息；run 通过 `Event::PlanReminder` 告知 Engine，Engine 在会话运行态记住锚点（跨轮次保留，进程重启后清空）并清除退出标记。
- **压缩后计划文件提醒**：压缩前经 `ToolPort::read_plan_file` 读取（上限 81024 字节，不存在或空白则无；其他错误使压缩失败），压缩成功后作为持久化的提醒消息追加在保留消息之后。
- **能力**：`runtime/capabilities` 与工作区执行能力的 `independentPlanState` 改为 `true`。

```mermaid
sequenceDiagram
    participant M as Model
    participant R as run task
    participant E as Engine
    participant T as ToolPort
    participant U as Client
    M->>R: ExitPlanMode {plan}
    R->>E: Event::Permission (ask)
    E-->>U: pendingInteractions += userInput(plan_approval)
    U->>E: resolveInteraction {answer}
    alt 批准
        E-->>R: Allow
        R->>T: write_plan_file(session, plan)
        R->>E: Event::PlanMode {enable:false}
        E-->>R: ok（planTransition, needs exit reminder）
        R->>E: ToolDone(approved text)
    else 带反馈拒绝
        E->>E: runtime.plan_feedback = text
        E-->>R: PlanRejected(feedback)
        R->>E: ToolDone(not approved, denied)
        R->>E: StepBoundary
        E-->>R: Guide(feedback user message)
    else 无反馈拒绝
        E-->>R: PlanRejected(none)
        R->>E: ToolDone(denied, stop_turn) 并取消同步其余工具
        R->>E: Finished
    end
```

- **与 Node 的差异（属于 M7 的请求构建）**：Node 把 `runtime_mode` 与 `plan_mode_exit` 提醒投影为对话中途的 system 消息（MCS）；Rust 目前与 todo 提醒一致，按 `<system-reminder>` 包装的 user 消息发送，正文逐字相同。
- **真实用户消息的判定**：role 为 user，且不是 `_zcode_source` 标记的合成消息、不以 `<system-reminder>` 或 `<task-notification>` 开头。

## 6. M2.5 Hooks

依据 `core/src/hooks/**`、`core/src/tool/executor/{hook-flow,call-runner}.ts`、`core/src/runtime/methods/{hooks,turn,turn-stop}.ts`、`adapters/src/exec/*`、`bootstrap/src/app/runtime-config.ts`。

- **配置与来源**：
  - 用户配置的 `hooks` 已由 M1 合并；插件 hooks 已由 M10.1 接入（见 rust-m10-plugins §3.5）；
  - 插件提供 hooks 时强制启用 hooks（保持 Node 行为）；
  - 注册顺序：用户 → 项目（需信任，M2.6）→ 插件 → 内部；
  - `enabled: false` 的 hook 跳过；
  - 子代理中不触发任何 hook。
- **7 种事件**：SessionStart、UserPromptSubmit、PreToolUse、PermissionRequest、PostToolUse、PostToolUseFailure、Stop。
  - 各事件的触发时机、输入字段与 stdin 兼容字段同 Node，包括：
    - PostToolUseFailure 中 `error` 被覆盖为字符串；
    - 从不发送 `permission_suggestions`。
  - 同一事件的 hooks 按注册顺序依次执行，不因某个 hook 拦截而中止；`async: true` 的 hook 在后台执行，不合并其输出。
- **匹配**：使用 `regress`（ECMAScript 语义），不加 flag、不锚定：
  - 空 matcher 或 `*` 匹配；
  - 只含 `[A-Za-z0-9_|]` 时按 `|` 分割后精确比较；
  - 非法正则不匹配；
  - UserPromptSubmit 与 Stop 没有匹配值，任何 matcher 都匹配；
  - 工具别名：`Agent` 与 `Task` 互为别名，`ApplyPatch` 对应 `[Write, Edit]`。
- **执行**：
  - command hook 使用 `/bin/sh -c` 或配置的 shell，process hook 按 argv 执行；
  - cwd 为运行时工作目录；
  - 环境变量为 M1 工具环境，加上会话与项目变量，有插件时再加插件变量；
  - `${VAR}` 展开规则同 Node：没有插件时保留 `${CLAUDE_PLUGIN_ROOT}` 原文；skill 变量报 `configuration_error`；
  - stdout 与 stderr 各自截在 `maxOutputBytes`，超出部分丢弃，不杀进程；
  - 单一计时器超时时结果为 `timed_out`，文本为 `Hook timed out after <n>ms`（D10 保持 Node 实际表现）；
  - 终止顺序：先 SIGTERM 进程组，750 ms 后 SIGKILL；根进程退出后 1 s 仍占用输出的子进程会被回收。
- **退出码与输出**：
  - 退出码 0：解析 stdout JSON，截断导致 JSON 无效时静默忽略（D8）；
  - 退出码 2：按事件生成拦截输出；
  - 其余退出码：hook 失败，不拦截。
- **合并**（`output.ts`）：逐字移植，包括：
  - `permissionRequestResult` 后者覆盖前者（D9a）；
  - `preventContinuation` 用空值覆盖 `stopReason`（D9b）；
  - `permissionBehavior` 按 deny > ask > 最后一个 allow 合并。
- **模型可见结果**：
  - PreToolUse 拦截：返回 permission_denied 错误；
  - 附加上下文：追加 `\n\n[Hook additional context]\n#1\n…`；
  - SessionStart、UserPromptSubmit、Stop 的上下文：以 system-reminder 形式注入，截断为 24000 个 UTF-16 单元；
  - UserPromptSubmit 阻止：不写入历史，本轮以阻止理由结束；
  - Stop 续跑：需要 `continue` 且带上下文，每轮最多 3 次。
- **PermissionRequest**：与用户审批竞速，先到者胜，失败方被取消（保持 Node 行为）。
- **投影**：
  - `hookInvocation` 行，lane 为 toolBefore / toolAfter / assistantWork；
  - HookRun 事件；
  - UserPromptSubmit 拦截时写 `lastError`，代码为 `fault.runtime.hookBlocked`。

### 6.1 Rust 结构与所有者

- **domain `hooks`（纯函数，Node 生成夹具比对）**：
  - `registrations(hooks, user_path)`：把 M1 合并后的 `hooks` 转成有序注册表。Rust 目前只有用户配置文件会带 hooks，因此来源一律为 `user`，`sourcePath` 为用户配置路径；插件 hooks（M10）与项目 hooks（M2.6）按 Node 的插入规则追加。
  - 匹配：`regress` 实现，规则同 §6；工具别名 `Agent↔Task`、`ApplyPatch→[Write, Edit]`。
  - 输入：7 种事件的输入对象按 Node 构造顺序生成键；另提供 stdin 兼容字段、transcript 行、`${VAR}` 展开和环境变量覆盖。
  - 输出：stdout 解析（按 zod3 规则校验并丢弃未知键）、退出码 2 的拦截输出、单个 hook 的处理、多个 hook 的合并（含 D9a/D9b）、拦截判定与 blockReason。
  - 文本：工具结果追加的 `[Hook additional context]`，以及生命周期上下文正文（按 UTF-16 单元截断到 24000 并加 `...`）。
  - 展示：`sanitize` 使用 Node 的正则与 flag（`regress`）；descriptor 与 `displayName` 同 Node。
  - 投影：`hookInvocation` 行的合并规则同 Node `onHookRunLifecycle`，包括执行项按 `hookIndex` 排序、行状态、lane 与 UserPromptSubmit 的 `lastError`。
- **进程执行**：新增 `ToolPort::run_hook(request, cancel)`，由 tools crate 实现：
  - 在临时目录写 transcript，按 domain 规则生成 stdin；
  - command hook 用 `/bin/sh -c`（或配置的 shell，Windows 为 `cmd.exe /d /s /c`），process hook 按 argv 执行；
  - cwd 与环境变量同 M1 工具环境，再加 hook 覆盖变量；
  - 进程单独成组；stdout 与 stderr 分别只保留前 `maxOutputBytes` 字节，超出部分读取后丢弃；
  - 超时由此处的唯一截止时间负责，结果为 `timed_out`；取消结果为 `cancelled`；两种情况都复用 `process_tree::terminate` 回收整个进程树；
  - 根进程退出后最多再等 1 s 管道关闭，超时则回收进程树；临时目录在任何结果下都会删除。
- **运行器（core `hook_runner`）**：
  - 每个 run 持有一个 `HookRunner`，其中有：注册表、`Arc<dyn ToolPort>`、事件 sink、工作目录，以及 M2.6 的准入回调。
  - 执行顺序：按注册顺序逐个执行；
    - 执行前先重新检查准入；
    - 生命周期事件以 `Event::Hook(payload)` 交给 Engine，payload 键序同 Node；
    - `async` hook 用 `tokio::spawn` 在后台执行，受本轮取消信号约束，输出不参与合并。
  - 结果映射：
    - `timed_out` → `Hook timed out after <n>ms`（`TOOL_TIMEOUT`）；
    - `cancelled` → `Hook execution cancelled`（`TOOL_CANCELLED`）；
    - 其他非零退出（退出码 2 除外） → `Hook process failed`，错误文本取原因（`TOOL_EXECUTION_FAILED`）。
  - 子代理的 run 不创建运行器。
- **调用点**：
  - **工具**（`tool_execution`）：
    1. 参数校验通过后执行 PreToolUse；
    2. 拦截时返回拒绝结果并附带上下文；
    3. `updatedInput` 替换参数；
    4. 权限判定应用 allow/ask 覆盖（`alwaysAsk` 不可被 allow 覆盖）；
    5. 执行成功后运行 PostToolUse，失败后运行 PostToolUseFailure；
    6. 上下文按 Node 规则追加在结果末尾。
  - **PermissionRequest**：
    - run 先发 `Event::Permission` 登记交互，再并发运行 hook 链。
    - hook 先给出决定时，发 `Event::PermissionHook {call_id, answer}`；Engine 只在交互仍待决时按同一收口（`settle_permission`、`permissionUpdates` 写项目规则）解决它，并把答案经原 `reply` 返回。
    - 用户先答复时，run 取消 hook 链，但不等待它结束。
    - `modify` 按 Node 复核改写后的输入：拒绝则拒绝；命中 `rule.project.ask` 时保持交互待决，等用户答复；否则以改写后的输入放行。
  - **轮次**（`agent_loop` 开头）：
    - 本进程内该会话第一次开轮时运行 SessionStart（`startup`），由 Engine 的运行态标记保证只运行一次；
    - 用户输入开启的轮次运行 UserPromptSubmit。
    - 被阻止时，run 发 `Event::PromptBlocked {reason}`：Engine 把本轮写入的模型消息截回输入边界并持久化，run 随即正常结束。模型不会看到该输入，也不会发出请求。
  - **Stop**：纯文本步骤收口、且没有引导输入时运行。满足续跑条件时，上下文作为提醒注入并继续请求，每轮最多 3 次。
  - **上下文注入**：SessionStart、UserPromptSubmit、Stop 的上下文作为运行期临时提醒插在当时的历史末尾。与 plan 提醒使用同一套 `Event::Reminder {anchor, kind, message}` 与会话运行态锚点：跨轮次保留，进程重启后清空。
- **投影**：Engine 的 `hook_events` 把生命周期合并进 `hookInvocation` 行（`row.appended` / `row.upserted`），并在 UserPromptSubmit 实际执行后被拦截时设置 `lastError`。
  - 没有 turn 的事件（`async` hook 在轮次结束后完成）只更新已存在的行。
  - 行随会话持久化；冷恢复时仍为 `running` 的行按 Node `onSessionResumed` 收口为 `failed`，执行项的结果为 `cancelled`。

```mermaid
sequenceDiagram
    participant M as Model
    participant R as run task
    participant H as HookRunner
    participant T as ToolPort
    participant E as Engine
    participant U as Client
    M->>R: tool call
    R->>H: PreToolUse(input)
    H->>E: Event::Hook(started)
    H->>T: run_hook(stdin)
    T-->>H: exit/stdout/stderr
    H->>E: Event::Hook(completed|blocked|failed)
    H-->>R: merged result
    alt 需要审批
        R->>E: Event::Permission
        E-->>U: pendingInteractions += permission
        par 用户
            U->>E: resolveInteraction
            E-->>R: answer（run 取消 hook 链）
        and hook
            R->>H: PermissionRequest
            H-->>R: decision
            R->>E: Event::PermissionHook
            E->>E: 仍待决才 settle
            E-->>R: answer
        end
    end
    R->>T: execute tool
    R->>H: PostToolUse / PostToolUseFailure
    R->>E: ToolDone(result + hook context)
```

- **顺带修正（对齐 Node）**：plan 开启时，ExitPlanMode 的任何非反馈拒绝都停轮（Node `withPlanExitDeniedTurnStop`），包括规则拒绝、PreToolUse 拦截和 PermissionRequest 拒绝。此前 Rust 只覆盖了用户拒绝。
- **与 Node 的差异**（不影响 hook 决策）：
  - Rust 的 JSON 对象按键排序（serde_json 未开 `preserve_order`）：
    - stdin 顶层键序与 Node 一致，嵌套对象（如 `tool_input`）按键排序；
    - 不同事件的注册先后与配置文件书写顺序无关。每次只运行一个事件的 hooks，同一事件内保持数组顺序。
  - `toolResponse` 为工具的结构化结果（`ToolOutput.data`），没有时为结果文本。
  - PostToolUseFailure 的 `error.type` 为 `tool_execution_failed` 或 `tool_cancelled`。
  - 工具结果追加 hook 上下文时不按各工具的 `maxModelBytes` 重新分配预算（属于 M5 的结果序列化）。
  - SessionStart `resume` 在本进程第一次开轮时运行，Node 在恢复会话时立即运行。上下文位置相同（本轮输入之前）。
  - hook 上下文与 plan 提醒一样，以 `<system-reminder>` 包装的 user 消息发送（M7 的 MCS 投影）。

## 7. M2.6 工作区信任

依据 `shared/{workspace-hook-config,workspace-hook-digest,workspace-hook-trust-store-file,workspace-hook-mutation}.ts`、`adapters/src/storage/workspace-hook-trust-store.ts`、`core/src/hooks/workspace-hook-*.ts`、`bootstrap/src/app/workspace-hook-*.ts`、`bootstrap/src/zcode-protocol/workspace-hook-trust.ts`。

- **发现**：沿用 M1 的项目配置发现顺序。一个项目 hook 条目带：
  - `reviewItemId`，形如 `workspace-hook-<源文件序号>-<事件>-<matcher 序号>-<hook 序号>`；
  - 相对路径与发现序号；
  - 解析后的超时与输出上限；
  - 四个开关：源根启用、声明启用、运行时启用、配置启用。
- **digest**：
  - `sha256(JSON.stringify(payload))`，payload 为声明数组或 bundle 数组；
  - 数字与字符串的序列化与 JS 一致；
  - 用 Node 生成的向量校验，含调研得到的 4 组向量。
- **信任存储**：与 Desktop 共用 `<storageRoot>/security/workspace-hook-trust-v1.json`：
  - 格式、字段顺序与去空白规则同 Node；
  - 锁协议：`wx` 创建锁文件，写入 pid、启动时间与 token；每 10 ms 重试，5 s 超时；锁文件超过 30 s 且持有者已死或启动时间不符时视为过期；
  - 原子写：临时文件 → fsync → rename，文件权限 0600；
  - 文件损坏时改名为 `.corrupt-<ms>`。
- **判定与准入**：
  - 信任状态依次为：
    - `blocked_policy`
    - `blocked_untrusted`（存储损坏）
    - `trusted_persistent`
    - `revoked`
    - `stale_digest`
    - `pending_trust`
  - 策略默认为 `user_decides`；
  - 每次执行项目 hook 前都重新评估准入；
  - 未获准的 hook 只产生一条 blocked 生命周期事件。
- **审查流程**：
  - 每个会话同时只有一个审查流程；流程的状态为 pending、resolved、superseded、cancelled、timed_out，超时 600 s；
  - 请求 payload 与 Node `workspace-hook-review-request.ts` 相同。
- **命令**（ACK 与 reasonCode 同 Node）：
  - `respondWorkspaceHookReview`
  - `toggleWorkspaceHookReviewItem`（改写 `<cwd>/.zcode/config.json` 中对应 hook 的 `enabled`）
  - `revokeWorkspaceHookTrust`
  - `requestWorkspaceHookReview`
- **RPC**：`workspace/hooks/trustGrant`（无会话的预先信任），结果为 `{accepted, reasonCode?}`；成功后同工作区的会话重新加载信任。
- **投影**：
  - pending interaction，`kind: "workspaceHookReview"`；
  - 快照字段 `workspaceHookAdmission`：`{pendingCount, bundleDigest, workspaceIdentity?}`，数量为 0 时为 `null`。
- **范围**：信任只在 app-server 中启用；纯 CLI 下项目 hooks 以 `workspace_hooks_feature_disabled` 阻止，与 Node 相同。

### 7.1 Rust 结构与所有者

- **domain `hooks::workspace`（纯函数）**：
  - 输入为 M1 发现的项目 hook 候选：已加载的配置文件，带发现序号，序号含未能加载的文件。
  - 生成条目、声明 digest、bundle digest 与快照。数字按 JS `Number#toString` 格式写入 payload。
  - 用项目条目生成注册，`source` 为 `project.<reviewItemId>`；每个事件的项目 hooks 插在该事件第一个非用户 hook 之前。
- **domain `hooks::trust`（纯函数）**：
  - 信任记录的字段顺序、去空白与严格校验；存储文件解析，任何不合规都判为损坏。
  - 逐条评估信任状态、准入类别与 reasonCode；汇总 pendingCount。
  - 生成授权记录与审查请求 payload。
  - 策略固定为 `user_decides`；Rust 没有策略提供方，`deny` / `allow_trusted_only` 只在纯函数中实现。
- **信任存储（host `trust_store`）**：
  - 经 core-api 端口 `TrustStorePort` 提供 `load` / `grant` / `revoke`。
  - 进程内串行执行；跨进程使用 Node 的锁文件协议。
  - 路径取用户配置文件的 `storage.dir`。
- **所有者（Engine）**：
  - 每个根会话持有一份 `WorkspaceTrust`：快照、持久记录、存储状态、撤销集合、安全修订号、激活与失败标记，以及当前审查流程（generation、交互、截止时间）。
  - Engine 把准入视图（逐条 `effectiveRunnable` / reasonCode / `configuredEnabled`，以及整体标记）经 `watch` 发布给该会话的 run。
  - run 中 `Driver::admission` 读取最新视图，派发每个项目 hook 前都重新判定。
  - 快照在会话第一次开轮时构建并激活：读取信任存储并评估，写 `workspaceHookAdmission`。之后授权、撤销、切换开关或 `trustGrant` 重载都会重新发布视图。
- **审查流程**：
  - 待决交互为 `{interactionId, kind: "workspaceHookReview", anchorRowId: null, createdAt, payload}`，同一流程只允许更高 generation 替换。
  - 截止时间到达时，由 Engine 的计时器收口为 `timed_out`（reasonCode `workspace_hooks_interaction_timeout`），并移除交互。
- **命令**：失败 ACK 为 `{status: "failed", reasonCode, message: "Workspace Hook review command rejected: <code>"}`。
  - 没有项目 hooks 的会话返回 `workspace_hooks_require_trust_capable_host`；
  - 信封 `sessionId` 与 payload 不一致时返回 `workspace_hooks_snapshot_mismatch`。
- **切换开关**：改写 `<cwd>/.zcode/config.json` 中该声明的 `enabled`（原子写）。
  - 写入后重新发现并替换快照，用新 generation 替换当前审查流程。
  - 写入后重建失败时，流程以 `configuration_error` 收口。
- **`workspace/hooks/trustGrant`**：
  - 由 app-server 路由到 Engine；按当前配置重新发现快照，再核对 bundle 与声明，然后授权并复核。
  - 成功后，同一工作区的所有会话重载信任并重发准入状态。

```mermaid
sequenceDiagram
    participant R as run task
    participant E as Engine
    participant S as TrustStorePort
    participant U as Client
    R->>E: 首轮开始（构建快照、激活）
    E->>S: load()
    S-->>E: records / missing / corrupt
    E->>E: 评估 → 发布准入视图
    E-->>U: workspaceHookAdmission{pendingCount, bundleDigest}
    R->>R: 项目 hook 派发前读视图（pending → blocked 事件）
    U->>E: requestWorkspaceHookReview
    E-->>U: pendingInteractions += workspaceHookReview(gen 1)
    U->>E: respondWorkspaceHookReview(trust_selected)
    E->>S: grant(records)
    S-->>E: 文件内容
    E->>E: 替换记录（修订号 +1）→ 重新发布视图
    E-->>U: 交互移除；workspaceHookAdmission 更新
    E-->>U: 仍有待审项 → gen 2 审查
```

- **缺陷 D15（保持 Node 行为）**：Node 锁文件的 `startTime` 取 `Date.now() - os.uptime()*1000`，即系统开机时间，而不是进程启动时间。因此，由存活进程持有且超过 30 s 的锁，在 macOS / Linux 上总会被判为过期并回收。锁通常只持有数毫秒，只有持有者卡死时才会出现这种情况。Rust 按相同方式写入与判定。
- **与 Node 的差异**：
  - 会话的快照与激活在本进程第一次开轮时进行。Node 在冷恢复会话时即激活，因此恢复的会话在第一轮之前不会显示横幅。
  - `trustGrant` 与会话使用同一套发现结果。Node 的 `trustGrant` 会单独解析未通过完整配置校验的文件里的 hooks。

## 8. 保持的 Node 缺陷（本期涉及）

- D1：被隐藏的工具被调用时照常执行。
- D2：用户答复与 PermissionRequest hook 竞速。
- D3：模式与 plan 各自读取。Rust 每次调用读取同一份快照，这是 Node 无竞争时的行为，不构成差异。
- D4：规则的读-合并-写。Rust 串行写入，判定结果不变。
- D5：WebFetch 的"始终允许"保存完整 URL，因而永不命中。
- D6：全权限只放行当前这一条。
- D7：重启后待决权限的收口。按 Node 当前行为：交互随进程丢失，冷恢复时工具行按中断收口。
- D8、D9、D10：见第 6 节。
- D15：信任存储锁的 `startTime` 取系统开机时间，见第 7.1 节。
- yolo 跳过项目 deny 规则与 `disallowedTools`。
- plan 允许非破坏性 MCP 工具。

**移植中新发现、待用户决定（当前保持 Node 行为）**：

- **D14（权限绕过）**：unbash 把带文件描述符前缀的进程替换当作普通参数，例如 `cat 2<(touch x)` 的 argv 为 `["cat", "2<(touch x)"]`，没有动态词，`cat` 又允许任意参数，因此判为只读，build / edit 模式下免审批执行。bash 实际会执行其中的命令（已在本机验证 `touch` 生效）。建议修复：解析层把 `\d+[<>]\(` 形式的词视为动态词（不可判定），并同步修 Node。
- 其余几处 unbash 怪癖不会导致免审批执行，保持 Node 行为：`> $(cmd)` 丢弃重定向但 argv 为空（非只读）；`a=($(cmd)) ls` 的数组元素不检查（赋值名不在白名单，非只读）；`toString; cmd` 因原型链保留字得到零条命令（非只读）；`echo x;; cmd` 丢弃 `;;` 之后的内容（bash 本身报语法错误，不执行）。

## 9. 验收

- **夹具**：
  - 工具能力表；
  - 权限判定矩阵：模式 × 工具 × 规则 × plan；
  - 规则匹配；
  - Bash 解析、只读判定、建议与规则求值，约 300 条命令加 git 场景；
  - plan 文本；
  - hook stdin、合并与 digest 向量。

  全部由 Node 实现生成，Rust 表驱动比对；生成器以 `--check` 纳入 `pnpm test:zcode-cli-rust`。

- **集成测试**（`packages/services/tests/zcode-cli-rust-*.test.ts`），覆盖以下场景：
  - build、edit、yolo 下的询问、允许与拒绝；
  - allowAlways 写项目规则并在重启后生效；
  - allowSession；
  - 用户拒绝的反馈文本；
  - 全权限改写排队输入；
  - plan 开关、EnterPlanMode / ExitPlanMode 的批准、带反馈拒绝与无反馈拒绝，以及计划文件与 plans RPC；
  - 目标互斥；
  - 各事件的 hooks；
  - PreToolUse 拦截与改写输入；
  - Stop 续跑上限；
  - 项目 hooks 从 pending 到信任，再到执行与撤销。

  以上场景在 desktop-continuous 与 web-remote-replayable 两种订阅下都要验证。

- **门禁**：
  - 每个子项提交前执行 `cargo clean` 与 `CARGO_INCREMENTAL=0`；
  - 运行 `pnpm test:zcode-cli-rust`、`cargo test --workspace`、`pnpm check:zcode-cli-rust`；
  - 改动 TS 时加跑 `pnpm typecheck`、`pnpm lint`。
