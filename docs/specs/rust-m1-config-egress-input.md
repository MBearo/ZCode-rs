# Rust M1：配置、网络出口与输入兼容

总设计见 [P0/P1 架构设计](rust-p0-p1-architecture.md) 5.2–5.4。原则：**对齐 Node 现有行为**（2026-09-23 确认，含已知缺陷，例如 D13）。每项单独提交；提交前清理编译产物、以 `CARGO_INCREMENTAL=0` 构建，Rust 单测与 Node 集成测试全部通过。

## 1. 交付项

1. **配置体系**：纯逻辑放 `domain::config`，文件与环境 IO 放 `host::config`；各适配器只使用 composition root 注入的加载器，不再自行读取配置文件。
2. **运行环境与网络出口**：启动时计算子进程环境；新增 `net` crate，负责代理、CA、身份头、Coding Plan 网关和设备 ID；模型、MCP、子进程统一使用。
3. **`sendText` 扩展字段**：`browserAmbientContext`、`toolDisallowlist`、`modelExecution`，以及 automation/off-peak 归属。
4. **`session/close`**：Host 与 UI 仍在调用的旧方法。

## 2. 配置体系

依据 `apps/zcode-cli/packages/adapters/src/config/`。

### 2.1 所有者与流程

```text
host::config::load(cwd, env)            （IO：发现并读取文件）
  ├─ 用户层 ~/.zcode/cli/config.json
  ├─ 项目层：git 根 → cwd，每级 zcode.json、.zcode/config.json（规范路径去重）
  └─ 环境层 ZCODE_*
        ↓ domain::config（纯函数）
  parse_file → normalize → validate → to_patch
  merge_layers(system < user < project < env < cli)
  resolve_mcp(system < project < user < env < cli)
        ↓
  Arc<ConfigSnapshot>（不可变）
```

- 与 Node 一致，没有文件监听：每个入口（启动、MCP 准备、Skill/Agent 发现、插件操作）重新加载。加载器以端口 `ConfigSource` 注入，适配器只拿快照。
- `ConfigSnapshot` 包含：各段的类型化字段（network、storage、permission、modelStream、features、memory、skills、skillOverrides、commandOverrides、logging、ui、toolConcurrency、modelAnomalyGuard、hooks）、按 Node 规则解析后的 MCP servers 及其来源、plugins 段与插件来源、项目 hooks 信任候选、诊断信息、合并后的原始 JSON（透传未知键）。

### 2.2 解析与校验

- 读取失败或 JSON 不合法：该文件 `loaded=false`，产生 `config_file_invalid`（error），整份忽略。
- MCP 旧格式归一化同 `normalizeMcpServerConfigInput`：
  - `environment` 转为 `env`；
  - `enable` 与 `enabled` 冲突时以停用为准；
  - 删除 `timeout`、`startup_timeout_sec`；
  - `remote` 转为 `http`；缺少 type 时按 command/url 推断；
  - `http_headers` 转为 `headers`。
- 单个 MCP server 校验失败只丢弃该项，产生 `config_mcp_server_invalid`（warning）；`mcp.servers` 不是对象时全部忽略。
- 整份文件按 `ZCodeConfigFileSchema` 校验，失败则整份忽略。schema 由 `scripts/generate-zcode-cli-rust-config-schema.mjs` 从 TS 导出（zod 4 `toJSONSchema`），用 M0 的子集校验器执行，`--check` 纳入测试。
- 转换为运行时补丁同 `parsedConfigFileToRuntimePatch`：
  - `skills[绝对路径]={enable}` 映射为 skillOverrides，与 `skill` 段合并；`command` 映射为 commandOverrides；
  - plugins 中 CUA 插件旧 id 规范化为新 id。Node 在加载时还会把迁移写回磁盘，Rust 只在内存中规范化、不写盘，结果相同。

### 2.3 合并

- 规则同 `config-merger.ts`：各段一层浅合并；mcp.servers 按名合并。
- plugins 专门处理：`dirs` 取并集，`enabledPlugins`、`extraKnownMarketplaces`、`options` 按 key 合并；项目层的 `extraKnownMarketplaces` 丢弃。
- hooks：来源 `enabled !== false` 时追加事件，`enabled` 取任一层为真。
- 项目层 hooks 从可执行合并中剔除，只作为信任候选保留，并产生 `config_project_hooks_pending_trust`。项目层 stdio MCP 的相对 `cwd` 按项目基准目录解析（`.zcode/config.json` 的基准目录是其上一级）。
- MCP servers 单独按 system < project < user < env < cli 解析，即用户层覆盖项目层。
- 环境层同 `env-config.adapter.ts`：`ZCODE_STORAGE_DIR`、`ZCODE_SESSION_DB_PATH`/`ZCODE_SESSION_DB`、`ZCODE_HTTP_PROXY`、`ZCODE_NO_PROXY`、`ZCODE_AGENT_CA_CERT`、`ZCODE_HTTP_TIMEOUT`/`ZCODE_TIMEOUT`、`ZCODE_LOG_FORMAT`、`ZCODE_MAX_TOOL_CONCURRENCY`。数值按 JS `Number()` 解析，NaN 记为 0（D13，保持 Node 语义）；按环境变量出现顺序，后出现者覆盖。
- 默认值同 `DefaultRuntimeConfig`。

### 2.4 验收

- 纯函数的表驱动测试，夹具由 `scripts/generate-zcode-cli-rust-fixtures.mjs` 调用 TS 的 `parseConfigFileToRuntimePatchWithDiagnostics`、`mergeConfigs`、`parseEnvConfig` 批量生成。
- MCP、Skill、Agent profile、插件发现改用快照后，现有集成测试保持通过；新增用户层覆盖项目层 MCP 的集成用例。

## 3. 运行环境与网络出口

依据 `packages/shared/src/runtimeEnv.ts`、`adapters/src/network/*`、`bootstrap/src/model-config.ts`、`runtime-platform-headers.ts`、`adapters/src/model/{model-execution,official-coding-plan-gateway,anthropic-request-metadata}.ts`、`adapters/src/device/cli-device-mid.ts`。

### 3.1 运行环境（启动时计算一次）

- **passthrough**：已有的 `ZCODE_TOOL_ENV_PASSTHROUGH_JSON`，加上当前环境中所有可捕获的键（`shouldCaptureZCodeToolEnvPassthroughKey`：排除 `OTEL_*`、`ZCODE_TELEMETRY_*`、`ZCODE_MODEL_TELEMETRY_ENABLED` 与非工具键）。
- **子进程基础环境**：当前环境去掉 `SANITIZED_RUNTIME_ENV_KEYS`，以及匹配 `^(npm_config|yarn|pnpm)_(http_proxy|https_proxy|proxy|all_proxy|no_proxy|cafile|ca)$`（不区分大小写）的键。
- **`Egress::child_env()`** 同 `applyNetworkEgressEnv`：
  1. 在基础环境上删除 passthrough 键；
  2. 还原 passthrough 的值；
  3. 若配置了代理（`network.httpProxy`，其次 `ZCODE_HTTP_PROXY`），写入六个代理键；
  4. 若配置了 no_proxy，写入 `NO_PROXY`、`no_proxy`；
  5. 若配置了 CA，写入五个 CA 键。

  Windows 上键名不区分大小写。

- Bash、MCP stdio、git 上下文命令、将来的 hooks 与插件 git，都以 `env_clear()` 加 `child_env()` 启动；进程自身环境不被修改，因此不存在多线程下修改环境的问题。

### 3.2 HTTP 客户端

- **代理**：只认 `network.httpProxy` → `ZCODE_HTTP_PROXY`；`no_proxy` 取 `network.noProxy` → `ZCODE_NO_PROXY`，按 `http-config.ts` 的规则逐 URL 匹配（逗号分隔、`*`、带端口 token、`*.x` 或 `.x` 或 `x` 匹配自身与子域、带 scheme 的 token、方括号 IPv6）。关闭 reqwest 对 `HTTP(S)_PROXY` 环境变量的自动读取：模型与 MCP 请求与 Node 一样不使用 shell 代理。WebFetch 另用 passthrough 中捕获的 shell 代理（M5）。支持 http、https、socks5 代理；PAC 不支持，配置时明确报错。
- **CA**：`network.caCertFile` → `ZCODE_AGENT_CA_CERT`，读 PEM 后**替换**系统根证书（与 Node 一致）。
- **超时**：`network.timeout` 不作用于模型请求（与 Node 一致），模型请求沿用 `modelStream.idleTimeoutMs`。
- 模型与 MCP HTTP 共用同一个客户端构建函数；按用途各建一个客户端并惰性初始化。

### 3.3 身份请求头（仅模型请求）

- **默认头**，进程内计算一次：
  - `HTTP-Referer`：ZCode origin，取 `ZCODE_BASE_URL`，其次 `ZCODE_ENDPOINT_ORIGIN`，默认 `https://zcode.z.ai`；
  - `User-Agent`：`ZCode/<version>`，version 取 `ZCODE_APP_VERSION`，否则取 Rust 包版本；
  - `X-ZCode-App-Version`；
  - `X-Title`：app-server 时为 `Z Code@electron`；
  - `X-Release-Channel`：`ZCODE_ENV=test` 时为 test，否则 production；
  - `X-Client-Language`：由 LC_ALL/LC_MESSAGES/LANG 推出 BCP47；
  - `X-Client-Timezone`：IANA 时区；
  - `X-ZCode-Agent: glm`；
  - `X-Platform`：Node 命名的 `darwin|win32|linux-arm64|x64`；
  - `X-Os-Category`：macos/windows/linux；
  - `X-Os-Version`：内核版本。
  - 值必须是可打印 ASCII，否则省略。
- **合并顺序**（不区分大小写，后者覆盖）：默认头 < OpenRouter 归属头（`X-OpenRouter-Title`、`X-OpenRouter-Categories`，仅 `*.openrouter.ai`） < provider `api.headers` < 每次尝试的 `requestAuth.headers` < 请求归属头（`x-request-id`、`x-zcode-session-type`（main/subagent/other）、`x-zcode-trace-id`、`x-query-id`（去掉 `query_` 前缀）、`x-session-id`（去掉 `sess_`/`subagent_agent_` 前缀））。
- **Anthropic 鉴权**：没有显式 Authorization 时补 `Bearer <apiKey>`；已有的 `x-api-key` 行为保持。
- **Coding Plan 网关**：`https://open.bigmodel.cn/api/anthropic/v1/messages` 改写为 `{origin}/api/v1/ultra/anthropic/v1/messages`，`https://api.z.ai/api/anthropic/v1/messages` 改写为 `{origin}/api/v1/ultra-zai/anthropic/v1/messages`。匹配 https、小写 host、端口（默认 443）、去掉尾部斜杠的路径；保留 query，删除 Host 头。改写在选择代理之前。
- **Anthropic `metadata.user_id`**：`{"device_id":…,"account_uuid":"","session_id":…}` 的 JSON 字符串，session id 去掉前缀。
- **设备 ID**：`${ZCODE_DATA_BASE_DIR 或 home}/.zcode/v2/telemetry-state.json` 的 `deviceMid`，与 Desktop 共用。
  - 读写经 `telemetry-state.lock` 独占锁：最多 200 次、每次 10 ms，锁超过 5 分钟或持有进程已死即视为过期；
  - 原子写入；进程内缓存；任何失败都退回仅本进程有效的 UUID。

### 3.4 reqwest 版本

模型当前使用 reqwest 0.12，MCP（rmcp）使用 0.13。统一到 0.13，共用同一套代理、CA 与构建逻辑；实施时若 0.13 的 TLS 根证书 API 无法实现"替换系统根"，则保留两个版本，由同一个配置生成两种客户端，并在本节记录原因。

## 4. `sendText` 扩展字段

依据 `core/src/runtime/helpers/conversation.ts:141-181`、`bootstrap/src/zcode-protocol-v4/commands/prompt-turn.ts:189-256`、`packages/shared/src/model-execution.ts`。M0 的 admission 已按 TS schema 校验字段结构与跨字段规则。

- **`browserAmbientContext`**：`tabCount` 为正整数且文本非空时，把本轮用户文本改写为 Node 的 `<in-app-browser-context source="ambient-ui-state">…## My request for ZCode:\n<text>` 格式（逐字一致）。
  - 改写只存在于内存中的规范消息（私有字段 `_zcode_request_content`，请求投影与压缩读取它）；持久化与 rows 使用原文。重启后改写消失，与 Node 一致。
- **禁用工具**：本轮冻结的禁用集合 = payload 的 `toolDisallowlist`，加上 automation 时的 `CronCreate/CronUpdate/CronDelete`，加上 off-peak 时的 `OffPeakCreate/SendMessage/Workflow`。
  - automation/off-peak 由显式 `automationId`/`offPeakTaskId` 判定，或由 inputId 前缀 `automation-`/`offpeak-` 推出（取 `:` 之前的部分，长度必须超过前缀）。
  - 只从提供给模型的工具定义中移除；模型仍调用被隐藏工具时照常执行（D1，保持 Node 语义）。
- **`modelExecution`**：
  - 会话忙时拒绝；
  - 选型只用于本轮，不写入会话、不改会话选择；
  - `requestAuth`（apiKey/headers）冻结在本轮，覆盖该轮每次请求的鉴权且不向 Host 请求，从不持久化、不写日志；
  - `subagents.foregroundModel=submission` 时前台子代理沿用本轮选型与鉴权，`background=deny` 时拒绝后台子代理；
  - `memoryExtraction=skip` 只记录（Rust 尚无记忆提取）。
- **`automationId`、`offPeakTaskId`、`offPeakRunType`**：记录为本轮归属，参与上面的禁用集合计算。

## 5. `session/close`

依据 `bootstrap/src/zcode-protocol/server-operations.ts:2708-2733`。

- 参数 `{sessionId, workspace?, expectedPersistence?}`。会话不在活动集合时返回 `-32004 "Session is not active: <id>"`。
- 当前 persistence：草稿为 `deferred`，其余为 `immediate`。带了 `expectedPersistence` 且不一致时返回 `{closed:false}`，不做任何改动。
- 否则走与 `deleteSession` 相同的 actor 收口：取消前后台工作、释放等待者、关闭上传与订阅、发布 index 移除、回收无历史的草稿，已持久化的历史保留。返回 `{closed:true}`。
- 为此把现有 `deleteSession` 的收口逻辑提取为共享方法，V4 命令与旧方法只是两个入口。

## 6. 验收

- 各项的 Rust 单测与集成测试：
  - 代理与 no_proxy 匹配（Node 夹具）、CA 替换、默认头与合并顺序、网关改写、设备 ID 锁与回退；
  - 子进程环境（Bash 看到的代理与 CA 变量）；
  - 浏览器上下文改写只进入请求、不进入持久化；
  - 禁用集合过滤工具定义；
  - `modelExecution` 本轮选型与鉴权不写入会话，忙时拒绝；
  - `session/close` 的三种结果。
- 每项提交前：`cargo clean`、`pnpm test:zcode-cli-rust`、`pnpm check:zcode-cli-rust`；改动 TS 文件时加跑 `pnpm typecheck`、`pnpm lint`。
