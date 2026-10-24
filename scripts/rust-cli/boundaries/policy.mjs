// packages/rust-cli 的边界规则表（docs/specs/rust-cli/README.md §3.1、§3.2、§4）。
//
// 这里是唯一来源：README 的依赖表、IO 类别表、第三方归属表与 tokio 特性表由 check.mjs 逐行比对。
// 新增 crate、依赖、IO 入口或第三方库时先改这里与 README，并写一行理由。

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
  frontend: { layers: [], crates: ["base", "prompts", "kernel-api", "runtime", "platform"] },
  entry: "*",
};

/// 依赖表（README §3.2）：表 = 实际依赖（普通、构建与测试依赖都算），死项与没登记的依赖都报错。
export const ALLOWED = {
  base: [],
  paths: ["base"],
  fs: ["base"],
  proc: [],
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
  "env-remote": ["exec-client", "exec-wire", "kernel-api"],
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
  compact: ["kernel-api", "prompts"],
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
  tui: ["base", "kernel-api", "platform", "runtime"],
  headless: ["base", "kernel-api", "runtime"],
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
];

/** 文件名作为路径的一整段（F58）：`name` 里的正则字符转义。 */
function pathSegment(name) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?:^|[\\\\/])${escaped}(?=$|[\\\\/])`);
}
