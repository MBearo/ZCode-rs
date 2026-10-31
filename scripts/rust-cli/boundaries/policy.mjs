// packages/rust-cli 的边界规则表（docs/specs/rust-cli/README.md §3.1、§3.2、§4）。
//
// 这里是唯一来源：README 的依赖表、IO 类别表、第三方归属表与 tokio 特性表由 check.mjs 逐行比对。
// 新增 crate、依赖、IO 入口或第三方库时先改这里与 README，并写一行理由。

// ZCode App 协议的方法名表（PROTOCOLS.zcode，app-protocol.md §9.2）：由 scripts/rust-cli/app-protocol/inventory.mjs --write
// 从 App 的协议源码生成，--check 查漂移。
import ZCODE_METHODS from "./zcode-methods.json" with { type: "json" };

/// crate 所在的层（README §3.1 规则 1）。
export const LAYERS = {
  base: "foundation",
  paths: "foundation",
  fs: "foundation",
  net: "foundation",
  proc: "foundation",
  prompts: "foundation",
  // 远端执行的协议与传输（docs/specs/rust-cli/remote-exec.md §15.4）：不认识 kernel-api。
  "exec-wire": "foundation",
  "exec-transport": "foundation",
  "exec-client": "foundation",
  "kernel-api": "kernel",
  policy: "kernel",
  hooks: "kernel",
  context: "kernel",
  tools: "kernel",
  compact: "kernel",
  memory: "kernel",
  agents: "kernel",
  ext: "kernel",
  query: "kernel",
  session: "kernel",
  runtime: "kernel",
  "node-store": "adapter",
  model: "adapter",
  "env-local": "adapter",
  // 远端机器的执行端口（客户端）与执行器侧：wire ⇄ kernel-api 的换算只在这两个 crate（remote-exec.md §15.4）。
  "env-remote": "adapter",
  "exec-server": "adapter",
  // SSH 的连接与部署（remote-exec.md §15.4、§19.9）：只认 exec-transport 的 Connector / Provisioner。
  "ssh-connect": "adapter",
  // 远端机器（remote-exec.md §15.4、§19.10、§20.3）：机器清单 machines.json（R3）与一台机器的连接状态机（R2），只认
  // Connector / Provisioner 与 exec-client。
  machines: "adapter",
  peers: "adapter",
  platform: "platform",
  tui: "frontend",
  headless: "frontend",
  // 协议适配层（docs/specs/rust-cli/app-protocol.md §3.2）：协议框架只被前端与入口依赖；每种协议一个前端 crate。
  "protocol-core": "frontend-shared",
  "protocol-zcode": "frontend",
  cli: "entry",
  // `zcode executor` 的组装（remote-exec.md §15.4）。
  "executor-host": "entry",
};

/// 每层能依赖的层与 crate。改依赖表也不能越过它。
export const LAYER_RULES = {
  foundation: { layers: ["foundation"], crates: [] },
  kernel: { layers: ["kernel"], crates: ["base", "prompts"] },
  adapter: { layers: ["foundation"], crates: ["kernel-api"] },
  platform: { layers: ["foundation"], crates: ["kernel-api"] },
  // frontend-shared：协议适配层的共用框架（docs/specs/rust-cli/app-protocol.md §3）。只依赖 base 与 kernel-api，只被前端与入口
  // 依赖；不认识任何一种协议。
  "frontend-shared": { layers: [], crates: ["base", "kernel-api"] },
  frontend: {
    layers: ["frontend-shared"],
    crates: ["base", "prompts", "kernel-api", "runtime", "platform"],
  },
  entry: "*",
};

/// 依赖表（README §3.2）：表 = 实际依赖（普通、构建与测试依赖都算），死项与没登记的依赖都报错。
export const ALLOWED = {
  base: [],
  paths: ["base"],
  fs: ["base"],
  // base：文件权限位、`/proc/<pid>/stat` 的字段与单位（F122，constants.md §2）。
  proc: ["base"],
  net: ["base"],
  // base：`include_lf!`（F56）与产品目录名 `.zcode`（loop.md 的位置，README §4.1，F55）。
  prompts: ["base"],
  "kernel-api": ["base"],
  // base：握手里不透明的凭据用 RedactedString（remote-exec.md §18.4）。
  "exec-wire": ["base"],
  // base：stderr 转发任务经 owned 启动；proc：stdio 传输的子进程（remote-exec.md §18.6）。
  "exec-transport": ["base", "proc"],
  // 会话层：任务经 base 的 TaskScope；凭据是 base 的 RedactedString（remote-exec.md §18.5）。
  "exec-client": ["base", "exec-transport", "exec-wire"],
  "node-store": ["base", "fs", "kernel-api", "paths"],
  model: ["base", "kernel-api", "net"],
  "env-local": ["base", "fs", "kernel-api", "net", "proc"],
  peers: ["base", "fs", "kernel-api", "net"],
  // MachineEnv 的 8 个端口经 exec-client 发请求（remote-exec.md §18）。§15.4 的表多写了 base：没有用到，不列。
  // F96（§23.6）：远端调用失败时给模型的说明在 prompts（模型看的文字）。
  "env-remote": ["exec-client", "exec-wire", "kernel-api", "prompts"],
  // 派发到注入的 MachineEnv；任务经 base 的 TaskScope，执行器实例名经 base 的 random（remote-exec.md §18）。
  "exec-server": ["base", "exec-transport", "exec-wire", "kernel-api"],
  // SSH 的 Connector 与 Provisioner（remote-exec.md §19.9）：ssh 进程经 proc；本机产物（ELF 头、sha256、上传的内容）经 fs；
  // 远端目录与可执行文件的名字、任务取 base。不认识 exec-wire：要的版本与协议由组装根给。
  "ssh-connect": ["base", "exec-transport", "fs", "proc"],
  // 状态机与驱动（remote-exec.md §19.10）：任务经 base 的 TaskScope；exec-wire 只在测试里（假执行器的帧）。机器清单
  // machines.json（R3，§20.3.1）：文件经 fs，位置由 paths 给。
  machines: ["base", "exec-client", "exec-transport", "exec-wire", "fs", "paths"],
  // base：`include_lf!`（F56）与产品目录与文件的名字（README §4.1，F55）。
  policy: ["base", "kernel-api"],
  hooks: ["base", "kernel-api", "prompts"],
  context: ["base", "kernel-api", "memory", "prompts"],
  memory: ["kernel-api"],
  tools: ["base", "kernel-api", "policy", "prompts"],
  // base：测试的模型常量（dev-dependency，工作线 F121）。
  compact: ["base", "kernel-api", "prompts"],
  agents: ["base", "kernel-api", "prompts"],
  ext: ["base", "kernel-api", "policy", "prompts"],
  query: ["base", "kernel-api", "prompts"],
  session: ["base", "kernel-api", "prompts", "query"],
  runtime: [
    "agents",
    "base",
    "compact",
    "context",
    "ext",
    "hooks",
    "kernel-api",
    "policy",
    "prompts",
    "query",
    "session",
    "tools",
  ],
  platform: ["base", "fs", "kernel-api", "net", "paths"],
  tui: ["base", "kernel-api", "platform", "prompts", "runtime"],
  headless: ["base", "kernel-api", "runtime"],
  // 协议框架：连接、会话桥、投递与 schema 校验只用内核会话 API；任务经 base 的 TaskScope（app-protocol.md §3.3）。
  "protocol-core": ["base", "kernel-api"],
  // App 协议：platform 给账号凭据的注入来源（G8）；P1 没有 App 形态的提示段，不依赖 prompts（§13 第 4 项）。
  "protocol-zcode": ["base", "kernel-api", "platform", "protocol-core"],
  cli: "*",
  // 入口：组装 exec-server 与注入的 env-local；测试依赖里有合同测试用到的 env-local、env-remote、exec-client。
  "executor-host": "*",
};

/// 波及范围（docs/specs/rust-cli/remote-exec.md §15.6 第 2 条）：远端 crate 的反向依赖闭包（谁直接或间接依赖它，测试依赖也算），
/// 实际必须与这里相等。“把 SSH 换成云只改两处”由此可查：新传输只被组装根依赖。
export const BLAST_RADIUS = {
  // machines：只在测试里（假执行器的帧）。
  "exec-wire": ["cli", "env-remote", "exec-client", "exec-server", "executor-host", "machines"],
  "exec-transport": [
    "cli",
    "env-remote",
    "exec-client",
    "exec-server",
    "executor-host",
    "machines",
    "ssh-connect",
  ],
  "exec-client": ["cli", "env-remote", "executor-host", "machines"],
  "env-remote": ["cli", "executor-host"],
  "exec-server": ["cli", "executor-host"],
  "executor-host": ["cli"],
  // cli：`executor status --json` 的输出与部署探测的解析互相核对（测试依赖）。
  "ssh-connect": ["cli"],
  machines: ["cli"],
};

/// 上面任何一个闭包里都不能有这些层的 crate：内核、前端与平台层不认识远端（远端对它们只是另一台机器的 MachineEnv）。
export const BLAST_FORBIDDEN_LAYERS = ["kernel", "frontend", "platform"];

/// 稳定依赖原则的例外（remote-exec.md §15.6 第 4 条）：键 `A->B`，值是理由。
export const SDP_EXCEPTIONS = {
  "headless->runtime":
    "runtime 只在 headless 的测试里用来组装内核（测试依赖）；headless 的产品代码只认 kernel-api 的会话 API",
};

/// IO 类别（README §4）：clippy.toml 的每条禁令按路径前缀归入一类；`entries` 是可以豁免这一类的 crate。
export const IO_CATEGORIES = [
  {
    name: "file",
    label: "文件",
    prefixes: [
      "std::fs::",
      "tokio::fs::",
      "std::path::Path::",
      "std::os::unix::fs::",
      "std::os::windows::fs::",
    ],
    entries: ["fs", "env-local", "proc", "cli"],
  },
  {
    name: "process",
    label: "进程",
    prefixes: ["std::process::", "tokio::process::"],
    entries: ["proc"],
  },
  { name: "env", label: "环境", prefixes: ["std::env::"], entries: ["cli"] },
  {
    name: "net",
    label: "网络",
    prefixes: ["reqwest::", "std::net::", "tokio::net::", "std::os::unix::net::"],
    entries: ["net"],
  },
  { name: "clock", label: "时间", prefixes: ["std::time::"], entries: ["node-store", "fs"] },
  { name: "ids", label: "ID", prefixes: ["uuid::"], entries: ["node-store"] },
  // 随机（F58）：`RandomState` 的种子经 base 的 `random`；加密强度的随机（ring）只在 platform。
  {
    name: "random",
    label: "随机",
    prefixes: ["std::hash::RandomState::", "ring::rand::"],
    entries: ["base", "platform"],
  },
];

/// crate 级放开的类别：这些 crate 有自己的 clippy.toml，内容等于根配置去掉这些类别的禁令。
export const CRATE_LEVEL_OPEN = { fs: ["file"] };

/// `allow-invalid = true` 只给按平台或按 tokio 特性才存在的路径。
export const ALLOW_INVALID_PREFIXES = [
  "std::os::unix::",
  "std::os::windows::",
  "tokio::fs::",
  "tokio::net::",
  "tokio::process::",
];

/// IO 类第三方库的归属（普通与构建依赖；表 = 实际）。
export const THIRD_PARTY_OWNERS = {
  reqwest: ["net"],
  rustls: ["net"],
  // 内置的 Mozilla 根证书（docs/specs/rust-cli/config.md §2.4，F44）：信任锚只在出口构造客户端时用。
  "webpki-root-certs": ["net"],
  rusqlite: ["node-store"],
  uuid: ["node-store"],
  ignore: ["env-local"],
  "grep-matcher": ["env-local"],
  "grep-regex": ["env-local"],
  "grep-searcher": ["env-local"],
  notify: ["env-local"],
  // cli：系统时区（docs/specs/rust-cli/time-zone.md §1，F8）——Unix 的 localtime_r、Windows 的时区 API。
  libc: ["paths", "fs", "proc", "cli"],
  // tui：Windows 控制台——stdin 是不是控制台（mintty 的判断）与背景色探测读控制台输入（工作线 F71），同 Unix 的 rustix。
  "windows-sys": ["fs", "proc", "cli", "tui"],
  ring: ["platform"],
  crossterm: ["tui"],
  ratatui: ["tui"],
  rustix: ["tui"],
  // 全局分配器（带 C 代码）只由组装根设置，且只在 *-linux-musl（docs/specs/rust-cli/linux.md §8.3，F44）。
  mimalloc: ["cli"],
  // 内嵌的搜索工具（第三方源码，非 workspace 成员，docs/specs/rust-cli/embedded-search.md，F53）：整个进程变成
  // 那个工具（读参数与环境、读写文件、起子进程），只有入口按 argv0 分派时调用。
  "rust-cli-vendor-rg": ["cli"],
  "rust-cli-vendor-bfs": ["cli"],
  "rust-cli-vendor-ugrep": ["cli"],
};

/// 只能出现在测试依赖里的库。
/// rcgen：真实进程与 net 的单测现生成 CA 与服务器证书（docs/specs/rust-cli/config.md §5，F21）。
export const TEST_ONLY = ["tempfile", "rcgen"];

/// 纯计算的库：谁都能用；列出的每个都要有人用（死项报错）。
export const PURE = [
  "anyhow",
  "arc-swap",
  "async-trait",
  "base64",
  "bytes",
  // cli：解开执行器的 npm 平台包（gzip，remote-exec.md §26.4，F100）。
  "flate2",
  "futures-util",
  "httpdate",
  // kernel-api `env_path::nfc`：Unicode NFC（工具路径参数，tools-files.md §3.2，F14）。
  "icu_normalizer",
  "pulldown-cmark",
  "regex",
  "regress",
  "serde",
  "serde_json",
  "sha2",
  "syntect",
  "thiserror",
  "tokio",
  "tokio-util",
  "tracing",
  "two-face",
  // tui：搜索按字素匹配（docs/specs/rust-cli/tui-input.md §2.3.3，F114）。
  "unicode-segmentation",
  "unicode-width",
  "url",
];

/// tokio 做 IO 的特性由哪些 crate 声明（普通与构建依赖；表 = 实际；测试依赖不限）。
export const TOKIO_IO_FEATURES = {
  fs: ["fs", "env-local", "proc"],
  process: ["proc"],
  // 跨会话的收件箱：Unix 域套接字只在 net（docs/specs/rust-cli/cross-session.md §2.1，F28）。
  net: ["net"],
  "io-std": ["cli"],
  // 入口把 SIGINT / SIGTERM / SIGHUP 变成前端的取消令牌（session-lifecycle.md §3.5，F5）。
  signal: ["cli"],
  full: [],
};

/// 单个 .rs 文件的行数上限（2026-10-18 用户把上限从 400 调到 800）。
export const MAX_LINES = 800;

/// 同一个名字只定义一处（README §4.1，F55）：产品代码（不含测试文件）的字符串字面量里出现 `pattern` 时，文件必须是
/// `owners` 之一；每个 owner 自己要有这个字面量（死项报错）。注释不算；`packages/rust-cli/prompts/` 下的提示词原文是
/// 数据文件，不查。
export const SINGLE_DEFINITIONS = [
  {
    name: "the product directory name .zcode",
    // `.zcode` 作为路径段：前面不是名字字符，后面不是名字字符、`-` 或 `.`（`.zcode-plugin`、`.zcode.json` 是别的名字）。
    pattern: /(?<![A-Za-z0-9_])\.zcode(?![A-Za-z0-9_.-])/,
    owners: ["crates/base/src/names.rs"],
    hint: "rust_cli_base::names::DOT_DIR (or rust_cli_base::dot_dir!() in a const)",
  },
  // F58：文件名只抓路径用途——字面量里作为整段出现（前面是开头或 `/` `\`，后面是结尾或 `/` `\`）。给用户或模型看的
  // 文字（前后有空格、标点）不算。
  {
    name: "the rules file name AGENTS.md",
    pattern: pathSegment("AGENTS.md"),
    owners: ["crates/base/src/names.rs"],
    hint: "rust_cli_base::names::RULES_FILE",
  },
  {
    name: "the local rules file name AGENTS.local.md",
    pattern: pathSegment("AGENTS.local.md"),
    owners: ["crates/base/src/names.rs"],
    hint: "rust_cli_base::names::LOCAL_RULES_FILE",
  },
  {
    name: "the project config file name zcode.json",
    pattern: pathSegment("zcode.json"),
    owners: ["crates/base/src/names.rs"],
    hint: "rust_cli_base::names::PROJECT_CONFIG_FILE",
  },
  {
    name: "the scheduled tasks file name scheduled_tasks.json",
    pattern: pathSegment("scheduled_tasks.json"),
    owners: ["crates/base/src/names.rs"],
    hint: "rust_cli_base::names::SCHEDULED_TASKS_FILE",
  },
  {
    name: "the memory index file name MEMORY.md",
    pattern: pathSegment("MEMORY.md"),
    owners: ["crates/memory/src/entrypoint.rs"],
    hint: "rust_cli_memory::entrypoint::ENTRYPOINT",
  },
  // F122（用户决定第 9 项）：通用的请求头名只在 base 的 `proto::http::header`。`accept` 不登记：hooks 的 elicitation 结果
  // 里有同名的取值（另一个意思）。
  ...["content-type", "retry-after", "user-agent"].map((header) => ({
    name: `the HTTP header name ${header}`,
    pattern: new RegExp(`^${header}$`, "i"),
    owners: ["crates/base/src/proto/http.rs"],
    hint: "rust_cli_base::proto::http::header (net::header re-exports it)",
  })),
  // F122 第二阶段（用户决定第 9 项，docs/specs/rust-cli/constants.md §5.2）：路径用途的 `.git`（整段，前后是开头、结尾或
  // 路径分隔符；`--exclude-dir=.git` 这类参数文字不算）、空设备、MIME、`x-api-key`。
  {
    name: "the git directory name .git",
    pattern: /(?:^|[/\\])\.git(?:$|[/\\])/,
    owners: ["crates/kernel-api/src/env_files.rs"],
    hint: "rust_cli_kernel_api::env_files::GIT_DIR (GIT_HOOKS_DIR, GIT_CONFIG_FILE)",
  },
  {
    name: "the null device /dev/null",
    pattern: /^\/dev\/null$/,
    owners: ["crates/base/src/path.rs"],
    hint: "rust_cli_base::path::DEV_NULL",
  },
  ...[
    "application/json",
    "application/pdf",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
    "text/event-stream",
    "text/html",
    "text/markdown",
    "text/plain",
  ].map((type) => ({
    name: `the MIME type ${type}`,
    pattern: new RegExp(`^${type}$`, "i"),
    owners: ["crates/base/src/proto/mime.rs"],
    hint: "rust_cli_base::proto::mime (net::header re-exports JSON and EVENT_STREAM)",
  })),
  {
    name: "the HTTP header name x-api-key",
    pattern: /^x-api-key$/i,
    owners: ["crates/net/src/header.rs"],
    hint: "rust_cli_net::header::X_API_KEY",
  },
];

// ── 容易变的字符串只在常量模块里（工作线 F121，README §4.1 第 6 条；检查在 boundaries/literals.mjs）──

/// 规则：字符串字面量里的写法（`pattern`）只许出现在 `owners`；`tests: true` 时测试也查。
export const VOLATILE_LITERALS = [
  {
    name: "model id",
    // 模型 ID 样子的写法：厂商前缀后接版本或系列（`glm-5.3`、`deepseek-v4-pro`、`claude-opus-5-5`、
    // `us.anthropic.claude-sonnet-4-5-…`、`gpt-5.4`、`kimi-k3`）。前面不是字母、数字或 `_`。Anthropic 的前缀逐个写出
    // （导出扫描只放过完整的模型 ID 写法）。
    pattern: new RegExp(
      `(?<![A-Za-z0-9_])(?:${[
        "claude-opus",
        "claude-sonnet",
        "claude-haiku",
        "claude-fable",
        "claude-mythos",
        "claude-1",
        "claude-2",
        "claude-3",
        "claude-4",
        "claude-5",
        "claude-6",
        "claude-7",
        "claude-8",
        "claude-9",
        "glm-[a-z0-9]",
        "deepseek-(?:v\\d|chat|reasoner|coder)",
        "gpt-\\d",
        "gemini-\\d",
        "kimi-k\\d",
        "qwen\\d",
      ].join("|")})`,
      "i",
    ),
    owners: ["crates/base/src/models.rs", "scripts/rust-cli/fixture/model.mjs"],
    tests: true,
    hint: "use rust_cli_base::models (scripts: fixture/model.mjs)",
  },
  {
    name: "host name",
    // 已知主机名（小写；品牌的写法 `Z.AI` 不算）：模型端点、账号服务、注册表、测试环境。测试环境的根域名拆开写（导出
    // 门禁的 internal-host 拦它的原文）。
    pattern: new RegExp(
      `(?<![A-Za-z0-9-])(?:${[
        "api\\.anthropic\\.com",
        "(?:[a-z0-9-]+\\.)*z\\.ai",
        "(?:[a-z0-9-]+\\.)*bigmodel\\.cn",
        "api\\.deepseek\\.com",
        "registry\\.npmjs\\.org",
        "api\\.openai\\.com",
        "(?:[a-z0-9-]+\\.)*openrouter\\.ai",
        "(?:[a-z0-9-]+\\.)*opencode\\.ai",
        `(?:[a-z0-9-]+\\.)*${["chat", "glm"].join("")}\\.site`,
      ].join("|")})(?![A-Za-z0-9-])`,
    ),
    owners: ["crates/base/src/hosts.rs", "scripts/rust-cli/fixture/model.mjs"],
    tests: true,
    hint: "use rust_cli_base::hosts (scripts: fixture/model.mjs)",
  },
  {
    name: "env var name",
    // 整个字面量就是环境变量名的样子（大写字母、数字与 `_`，至少一个 `_`）：业务代码读写的环境变量名只在 base 的
    // `names`（`names::env`、给用户程序的 `names::vars`）。只查产品代码；单个词的名字（`HOME`、`PATH`）样子上认不出，
    // 收进 `names::env` 之后由“同一个名字只定义一处”的约定管。
    pattern: /^_?[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+$/,
    owners: ["crates/base/src/names.rs"],
    tests: false,
    hint: "use rust_cli_base::names::env",
  },
];

/// 不查的文件：只在本仓库的对齐材料（parity 测试、轨迹比对与提示词提取的脚本等，导出时删掉）。vendor 与 JSON 数据不是
/// Rust / JS 源码，本来不收。
export const VOLATILE_UNCHECKED = [
  /_parity_tests\.rs$/,
  /^crates\/[^/]+\/tests\/parity(?:_[^/]+\.rs|\/)/,
  new RegExp(
    `^scripts/rust-cli/(?:${["or", "acle"].join("")}|prompts|bench|export|repo-only|app-protocol)/`,
  ),
  /^scripts\/rust-cli\/bench\.mjs$/,
];

/// 例外（按文件，每项写类别与理由）：`{ files, rules, reason }`，`rules` 是 VOLATILE_LITERALS 的 `name`。按 crate 分组
/// 登记（前端与入口一组，适配器、平台、内核与脚本一组），环境变量名另一组。
const FRONT_EXCEPTIONS = [];
const CORE_EXCEPTIONS = [
  {
    files: ["crates/prompts/src/models_tests.rs"],
    rules: ["model id"],
    reason:
      "解析器的输入样本：按 ID 认旧模型（chars_per_token）的各种写法（日期、Bedrock / Vertex 的后缀、下划线、长得像的 ID）",
  },
  {
    files: ["crates/base/src/rule_url_tests.rs"],
    rules: ["host name"],
    reason: "解析器的输入样本：URL 规范化与端点判定（大小写、端口、空白、路径里与长得像的域名）",
  },
  {
    files: ["crates/model/src/profiles_select_tests.rs"],
    rules: ["model id", "host name"],
    reason: "解析器的输入样本：请求方案表对各种 URL 与模型 ID 写法的命中",
  },
  {
    files: ["crates/platform/src/model_name_tests.rs"],
    rules: ["model id"],
    reason: "解析器的输入样本：`--model` 的各种写法（provider 前缀、团队项目的后缀）",
  },
  {
    files: ["scripts/rust-cli/boundaries/literals.test.mjs"],
    rules: ["model id", "host name"],
    reason: "这条检查自己的测试：合成的违规样本",
  },
  {
    files: ["scripts/rust-cli/boundaries/policy.mjs"],
    rules: ["model id"],
    reason: "这条检查自己的规则表：模型 ID 的前缀",
  },
  {
    files: ["scripts/rust-cli/login-golden.mts", "scripts/rust-cli/team-plan-golden.mts"],
    rules: ["model id"],
    reason: "黄金文件的生成脚本：给 Node 喂固定的账号模型，产物是登录与团队套餐的黄金文件",
  },
];
const ENV_NAME_EXCEPTIONS = [
  {
    files: ["crates/cli/build.rs", "crates/cli/src/build_number.rs"],
    rules: ["env var name"],
    reason:
      "构建脚本读 Cargo 与正式发版给的变量：build.rs（及它用 #[path] 引进的 build_number.rs）在编译期跑，不依赖 base",
  },
  {
    files: ["crates/proc/src/identity.rs", "crates/proc/src/tree/table.rs"],
    rules: ["env var name"],
    reason: "proc 不依赖 base（依赖表里 proc 没有依赖）：给子进程 ps 的固定区域设置 LC_ALL=C",
  },
  {
    files: ["crates/cli/src/signals.rs"],
    rules: ["env var name"],
    reason: "Windows 控制台事件的名字（CTRL_C 等），不是环境变量",
  },
  {
    files: ["crates/platform/src/team/list.rs"],
    rules: ["env var name"],
    reason: "服务返回的枚举值（订阅方式 ONE_TIME），不是环境变量",
  },
  {
    files: ["crates/policy/src/bash_compound.rs"],
    rules: ["env var name"],
    reason: "shell 的内置变量表（判定复合命令用），不是本进程读写的变量",
  },
  {
    files: ["crates/policy/src/removal.rs", "crates/policy/src/safe_env.rs"],
    rules: ["env var name"],
    reason:
      "权限判定的数据表：命令里出现的变量名（删除保护的家目录变量、可以安全前置的变量），不是本进程读写的变量",
  },
];
export const VOLATILE_EXCEPTIONS = [
  ...FRONT_EXCEPTIONS,
  ...CORE_EXCEPTIONS,
  ...ENV_NAME_EXCEPTIONS,
];

/** 文件名作为路径的一整段（F58）：`name` 里的正则字符转义。 */
function pathSegment(name) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?:^|[\\\\/])${escaped}(?=$|[\\\\/])`);
}

// ── 协议适配层与解耦（docs/specs/rust-cli/app-protocol.md §3、§9；用户 2026-10-25：“兼容 host 的代码一定要解耦，不能入侵
// 核心逻辑……它应该和 tui 是同一个层级的”）。检查在 boundaries/decoupling.mjs 与 boundaries/kernel-surface.mjs。──

/// 规划中的 crate：层与依赖在设计期就过 LAYER_RULES。建 crate 时把它挪到 LAYERS 与 ALLOWED（README §3.2 的表同步），
/// 建好了还留在这里报错（protocol-core 与 protocol-zcode 在 P1 建好、已挪走）。
export const PLANNED_CRATES = {
  // 以后的 ACP（Agent Client Protocol）：只经框架与内核会话 API。
  "protocol-acp": { layer: "frontend", allowed: ["base", "kernel-api", "protocol-core"] },
};

/// 这些层的 crate 里不能出现任何一种协议的词汇（协议是前端的事）。
export const PROTOCOL_NEUTRAL_LAYERS = [
  "foundation",
  "kernel",
  "adapter",
  "platform",
  "frontend-shared",
];

/// 每种协议：属于它的 crate、它的词汇（`re` 在源码文字上匹配）与方法名（字符串字面量与它完全相同才算）。
/// `comments`：注释里出现也算（ZCode 与 ACP 是；stream-json 是对齐对象自己的概念，内核注释里说明“-p / stream-json 输入
/// 时怎样”是照对齐对象，只查代码与字面量）。词汇只能出现在它自己的 crate 与入口（组装根解析子命令）里。
export const PROTOCOLS = {
  zcode: {
    crates: ["protocol-zcode"],
    comments: true,
    terms: [
      { id: "zcode-protocol", re: /zcode[_-]?protocol|ZCode ?Protocol/i },
      { id: "app-server", re: /app[-_]server|AppServer|appServer/ },
      // 协议的名字 V4（大写、独立的词）；`Ipv4Addr`、`new_v4`、“uuid v4” 不算。
      { id: "v4-name", re: /(?<![A-Za-z0-9_])V4(?![A-Za-z0-9_])/ },
      // 方法名的前缀 `v4/`；URL 里的 `/v4/`（模型端点的路径）不算。
      { id: "v4-method", re: /(?<![A-Za-z0-9_/.:-])v4\/[A-Za-z]/ },
      { id: "log-epoch", re: /log[_-]?epoch/i },
      // 两种投递与它们的选择键。
      {
        id: "client-mode",
        re: /desktop[-_]continuous|web[-_]remote[-_]replayable|client[_-]?mode/i,
      },
      { id: "delivery-kind", re: /delivery[_-]?kind|wire[_-]?version/i },
      // 命令裁决的字段（CAS 与幂等表）：revision 由会话 actor 给出，裁决在前端（app-protocol.md §6）。
      {
        id: "command-cas",
        re: /revision[_-]?at[_-]?decision|base[_-]?revision|base[_-]?log[_-]?epoch|command[_-]?inbox/i,
      },
      { id: "topic", re: /sessions-index\/|workspace-config\// },
      {
        id: "snapshot-field",
        re: /pendingInteractions|inputRouting|pendingCommands|workspaceHookAdmission/,
      },
      // Host 的身份键（workspaceIdentity 是与 Node 共用的存储字段，不在这里）。
      { id: "workspace-key", re: /workspace[_-]?key/i },
      // Host 给 agent 的环境变量与启动参数：由入口读，换成内核的通用参数。
      // （`ZCODE_AGENT_CA_CERT` 是代理与根证书的通用变量，TUI 也认，不在这里。）
      {
        id: "host-env",
        re: /ZCODE_WORKSPACE_IDENTITY|GEN_UI_OUTPUT_ROOT|ZCODE_AGENT_SERVER_[A-Z_]+|ZCODE_AGENT_(?:IMPL|RUST_BIN)\b/,
      },
      { id: "surface-flag", re: /--surface\b/ },
      { id: "electron", re: /ELECTRON_RUN_AS_NODE|utilityProcess|Electron/ },
    ],
    methods: ZCODE_METHODS.methods,
    // Host 与 agent 之间的方法（旧方法、V4、反向请求、通知）。
    methodId: "host-method",
  },
  acp: {
    crates: ["protocol-acp"],
    comments: true,
    terms: [
      {
        id: "acp-name",
        re: /(?<![A-Za-z0-9_])ACP(?![A-Za-z0-9_])|agent[-_ ]?client[-_ ]?protocol/i,
      },
      {
        id: "acp-field",
        re: /agentCapabilities|clientCapabilities|sessionUpdate|agent_message_chunk|agent_thought_chunk|tool_call_update|available_commands_update|current_mode_update/,
      },
    ],
    // `initialize`、`authenticate` 与 MCP 同名，不列。
    methods: [
      "session/new",
      "session/load",
      "session/prompt",
      "session/cancel",
      "session/update",
      "session/request_permission",
      "session/set_mode",
      "session/set_model",
      "fs/read_text_file",
      "fs/write_text_file",
      "terminal/create",
      "terminal/output",
      "terminal/release",
      "terminal/wait_for_exit",
      "terminal/kill",
    ],
  },
  "stream-json": {
    crates: ["headless"],
    comments: false,
    terms: [{ id: "stream-json", re: /stream[-_]?json/i }],
    methods: [],
  },
};

/// 词汇的例外：只按文件与精确片段（照 export/scan.mjs 的写法），没用到的是死项。
export const PROTOCOL_TERM_EXCEPTIONS = [
  // A0 盘点时已有（2026-10-25）：会话 API 的模块说明提到第二阶段的协议前端。只是注释，不改行为；改不改由用户定
  // （app-protocol.md §9.6）。
  {
    file: "crates/kernel-api/src/session_api.rs",
    fragment: "第二阶段的 ZCode Protocol 前端也一样",
    why: "已有的模块说明（A0 盘点），待用户决定",
  },
  {
    file: "crates/kernel-api/src/session_api.rs",
    fragment: "列出的 V4 适配层输入",
    why: "已有的模块说明（A0 盘点），待用户决定",
  },
];

/// 按“当前前端是谁”分支的写法（只看代码；注释与字符串不算）。内核要的是与前端无关的能力位，不是前端的名字。
export const FRONTEND_BRANCH_PATTERNS = [
  { id: "frontend", re: /\b(?:frontend|Frontend|FrontendKind|front_end|FrontEnd)\b/ },
  {
    id: "app-mode",
    re: /\b(?:app_mode|AppMode|is_app|in_app|desktop_mode|DesktopMode|is_desktop|is_mobile|is_tui|is_headless|headless_mode|tui_mode|host_mode|HostMode|ui_kind|UiKind|client_kind|ClientKind)\b/,
  },
  { id: "surface", re: /\b(?:surface|Surface|presentation_surface|PresentationSurface)\b/ },
  {
    id: "cfg-frontend",
    re: /\bcfg!?\s*\(\s*(?:not\s*\(\s*)?(?:app|frontend|desktop|mobile|host|tui|headless)\b/,
  },
];

/// 按前端分支的例外（按文件与精确片段，只在代码里找；测试文件不查这一条）。
/// A0 盘点时已有（2026-10-25）：context 的 `Surface`（`-p` 与终端界面的提示词差别，照对齐对象的交互 / 非交互会话），
/// runtime 按它选后台命令的期限。登记成例外并冻结变体（FROZEN_ENUMS）；App 不加第三个变体，要的差别做成与前端无关的
/// 能力位（app-protocol.md §5.3、§9.6）。
export const FRONTEND_BRANCH_EXCEPTIONS = [
  ...["pub enum Surface", "pub surface: Surface", "fn new(surface: Surface)", "surface,"].map(
    (fragment) => ({
      file: "crates/context/src/config.rs",
      fragment,
      why: "已有的 Surface（A0 盘点）",
    }),
  ),
  {
    file: "crates/context/src/analysis.rs",
    fragment: "surface: self.config.surface",
    why: "已有的 Surface（A0 盘点）",
  },
  ...[
    "OutputStyle, Surface}",
    "self.config.surface == Surface::Interactive",
    "surface: self.config.surface",
  ].map((fragment) => ({
    file: "crates/context/src/lib.rs",
    fragment,
    why: "已有的 Surface（A0 盘点）",
  })),
  ...["OutputStyle, Surface}", "pub surface: Surface", "facts.surface == Surface::Interactive"].map(
    (fragment) => ({
      file: "crates/context/src/system.rs",
      fragment,
      why: "已有的 Surface（A0 盘点）",
    }),
  ),
  ...[
    "OutputStyle, Surface}",
    "ContextConfig::new(Surface::Interactive)",
    "matches!(context.surface, Surface::Print)",
  ].map((fragment) => ({
    file: "crates/runtime/src/lib.rs",
    fragment,
    why: "已有的 Surface（A0 盘点）",
  })),
];

/// 已有的“形态”枚举：变体固定。加一个变体就是给内核加一种前端分支，要先改设计（app-protocol.md §9.3）。
export const FROZEN_ENUMS = [
  {
    file: "crates/context/src/config.rs",
    name: "Surface",
    variants: ["Print", "Interactive"],
    why: "A0 盘点时已有：照对齐对象的交互 / 非交互会话；App 不加变体",
  },
];

/// 中立层的 crate 只能声明这些 cargo feature（按前端开关的 feature 不许有）。值是理由。
export const NEUTRAL_FEATURES = {
  testing: "kernel-api 的测试替身（README §11 M0）",
  parity: "读对齐基线的 parity 测试（export.md §5）",
};

/// kernel-api 只加不改（boundaries/kernel-surface.json，app-protocol.md §9.4）：快照只追加；已有项的改动或删除逐项登记在这里，
/// `was` 是快照里的值，`now` 是现在的值（删除写 null），`reason` 写用户的决定。
export const KERNEL_API_CHANGES = [
  {
    key: "machine::Commands::start_command",
    was: "async fn start_command(&self,workspace:&WorkspaceRef,context:OperationContext,request:StartCommand,) -> Result<(),EnvError>",
    now: "async fn start_command(&self,workspace:&WorkspaceRef,context:OperationContext,request:StartCommand,) -> Result<CommandEvents,EnvError>",
    reason:
      "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：执行环境不按 operationId 重放；start_command 直接返回这条命令的事件流，登记只放还在跑的命令",
  },
  {
    key: "machine::Commands::operation_status",
    was: "async fn operation_status(&self,operation:OperationId) -> Result<Outcome,EnvError>",
    now: null,
    reason:
      "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：执行环境不按 operationId 重放；start_command 直接返回这条命令的事件流，登记只放还在跑的命令；产品代码没有调用方",
  },
  {
    key: "orchestration::Interactions::arm_monitor",
    was: "async fn arm_monitor(&self,_session:&SessionId,_monitor:crate::monitor::MonitorArmed,) -> Result<(),OrchestrationError> {default}",
    now: "async fn arm_monitor(&self,_session:&SessionId,_monitor:crate::monitor::MonitorArmed,_events:crate::machine::CommandEvents,) -> Result<(),OrchestrationError> {default}",
    reason:
      "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：执行环境不按 operationId 重放；start_command 直接返回这条命令的事件流，登记只放还在跑的命令；monitor 读 Monitor 工具交来的那条流",
  },
  {
    key: "machine::Checkpoints::rewind_status",
    was: "async fn rewind_status(&self,_token:&OperationId) -> Result<RewindStatus,EnvError> {default}",
    now: null,
    reason: "用户决定 2026-10-30（工作线 F119）：执行环境不按 token 重放，rewind_status 没有调用方",
  },
  {
    key: "orchestration::RunSink::commit_intent",
    was: "async fn commit_intent(&self,operation:OperationId,fingerprint:String,) -> Result<(),StoreError>",
    now: null,
    reason: "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：操作意图没有读者，不再落库",
  },
  {
    key: "store::Fact::OperationIntent",
    was: "OperationIntent{operation:OperationId,fingerprint:String,}",
    now: null,
    reason: "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：操作意图没有读者，不再落库",
  },
  {
    key: "store::LoadedSession.open_operations",
    was: '#[serde(default,skip_serializing_if = "Vec::is_empty")] open_operations:Vec<OpenOperation>',
    now: null,
    reason: "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：操作意图没有读者，不再落库",
  },
  {
    key: "store::OpenOperation",
    was: "#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]",
    now: null,
    reason: "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：操作意图没有读者，不再落库",
  },
  {
    key: "store::OpenOperation.fingerprint",
    was: "fingerprint:String",
    now: null,
    reason: "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：操作意图没有读者，不再落库",
  },
  {
    key: "store::OpenOperation.operation",
    was: "operation:OperationId",
    now: null,
    reason: "用户决定 2026-10-30（工作线 F119，README §6 规则 3）：操作意图没有读者，不再落库",
  },
];

// ── 厂商错误的解耦（docs/specs/rust-cli/provider-errors.md §2.1、§2.5、§11）。厂商码与厂商专有的字符串只在第 1 层（model 的
// `errors/` 目录，厂商分类器）；第 2 层（`errors/dispose.rs`、transport）、query、session 与前端只认中性分类。检查在
// boundaries/vendor.mjs。──

/// 厂商与协议的错误词。`owners` 是能出现的位置：以 `/` 结尾的是目录（含其中的测试文件）；`.rs` 文件连同它的测试文件
/// （同名加 `_` 开头、只在测试里编译的文件，如 `zhipu_tests.rs`）。
/// 测试文件（isTestFile 与 `_tests_*.rs`、`test_support.rs`、`test_more.rs`，含 crate 的 `tests/` 目录）不查 `values`、`codes`
/// 与 `protocol`：测试要把厂商的原始正文端到端喂进去。`selector` 连测试文件也查。
export const VENDOR_ERROR_TERMS = {
  vendors: [
    {
      // 智谱（z.ai、bigmodel.cn，含 ZCode 套餐网关）：码表与结束原因只在它自己的分类器（provider-errors.md §5.3）。
      id: "zhipu",
      owners: ["crates/model/src/errors/zhipu.rs"],
      // 智谱专有的结束原因（字符串字面量与之完全相同才算）：`sensitive` 是内容拦截，`network_error` 是流内的网络失败。
      values: ["sensitive", "network_error"],
      // 业务码（字符串字面量与之完全相同才算）。与 errors/zhipu.rs 的 CODES 相同，只去掉 `500`（与 HTTP 状态同名，太常见）；
      // vendor.test.mjs 比对两边，防止漂移。
      codes: [
        "1005",
        "1006",
        "1008",
        "1113",
        "1120",
        "1210",
        "1211",
        "1213",
        "1214",
        "1230",
        "1234",
        "1261",
        "1301",
        "1302",
        "1303",
        "1304",
        "1305",
        "1308",
        "1309",
        "1310",
        "1311",
        "1312",
        "1313",
        "1314",
        "1315",
        "1316",
        "1317",
        "1318",
        "1319",
        "1320",
        "1321",
        "2007",
        "2056",
        "20097",
        "3001",
        "3002",
        "3006",
        "3007",
        "3008",
        "3009",
        "3010",
      ],
      // 代码里的数字字面量（去掉注释与字符串之后的 `1302`、`1302u16`、`1_302`）也算码的 crate：处理模型错误的 crate。
      // net、env-local 里 WebSocket 的关闭码 1005 / 1006 是另一回事，不在其中。
      numericCrates: [
        "model",
        "query",
        "session",
        "compact",
        "agents",
        "headless",
        "tui",
        "runtime",
        "kernel-api",
        "protocol-zcode",
      ],
      // 选厂商的写法 `Vendor::Zhipu`（含 `Vendor::{…, Zhipu}`，只看代码）：只在分类器目录与按端点选厂商的 profiles.rs
      // （provider-errors.md §2.2，方案表是唯一来源）。测试文件也查。
      selector: {
        re: /\bVendor\s*::\s*(?:Zhipu\b|\{[^}]*\bZhipu\b)/,
        owners: ["crates/model/src/errors/", "crates/model/src/profiles.rs"],
      },
    },
  ],
  // Anthropic / OpenAI 协议的错误类型、错误码与结束原因（字符串字面量与之完全相同才算；`content-filter` 这类带连字符的
  // 别的写法不算）：只在分类器目录（provider-errors.md §5.1、§5.2、§5.4）。
  protocol: {
    owners: ["crates/model/src/errors/"],
    groups: [
      // 配额与账单用尽（中性分类 QuotaExhausted）。
      {
        id: "quota",
        values: [
          "insufficient_quota",
          "billing_error",
          "credit_balance_exhausted",
          "organization_spend_limit_exceeded",
          "project_spend_limit_exceeded",
          "organization_usage_limit_exceeded",
          "exceeded_current_quota_error",
        ],
      },
      // 内容拦截（ContentBlocked；OpenAI 的结束原因）。
      { id: "content-filter", values: ["content_filter"] },
      // 过载（Overloaded）。
      {
        id: "overloaded",
        values: ["overloaded_error", "server_is_overloaded", "engine_overloaded_error"],
      },
      // 限流（RateLimited）。
      {
        id: "rate-limit",
        values: ["rate_limit_error", "rate_limit_exceeded", "rate_limit_reached_error"],
      },
      // 请求过大（TooLarge）。
      { id: "too-large", values: ["request_too_large"] },
      // 上下文超长（ContextOverflow；Anthropic 的结束原因与 OpenAI 的错误码）。
      {
        id: "context-overflow",
        values: ["model_context_window_exceeded", "context_length_exceeded"],
      },
    ],
  },
};

/// 厂商错误词的例外：只按文件与精确片段（同 PROTOCOL_TERM_EXCEPTIONS），没用到的是死项。值 `why` 写理由。
export const VENDOR_ERROR_TERM_EXCEPTIONS = [];

// ── 有含义的数字（docs/specs/rust-cli/constants.md §4，工作线 F122）。检查在 boundaries/numbers.mjs。──

/// 换不了写法、确实没有含义的数字：全局放行的模式。`value` 是数值，`before` / `after` 是同一行里字面量前后的代码（已去掉
/// 注释与字符串）的正则，`reason` 写为什么没有含义。每加一条，numbers.test.mjs 加一个用例。
export const NUMBER_ALLOWED = [];

/// 推送前“不许升高”（scripts/pre-push-rust-cli.mjs）：numbers.json 里某个文件的处数比推送基线大时逐项登记。`was` 是推送基线
/// 里的值（没有为 0），`now` 是新的上限，`reason` 写用户的决定或“从 A 拆出”。
export const NUMBER_RAISES = [];
