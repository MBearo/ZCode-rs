// 边界检查的测试（docs/specs/rust-cli/README.md §3.1、§3.2、§4）：
//   node --test scripts/rust-cli/boundaries/check.test.mjs
// 每类违规一个用例，只用合成的依赖图、源码与配置；先确认合成的基线没有违规，再逐项加一个违规。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  categoriesIn,
  categoryOf,
  checkBlastRadius,
  checkClippy,
  checkDefinitions,
  checkGraph,
  checkSources,
  checkStableDependencies,
  instability,
  parseClippy,
  reverseClosure,
  stringLiterals,
  stripCode,
} from "./check.mjs";
import * as real from "./policy.mjs";

const policy = {
  ...real,
  LAYERS: {
    base: "foundation",
    fs: "foundation",
    "kernel-api": "kernel",
    tools: "kernel",
    "node-store": "adapter",
    cli: "entry",
  },
  ALLOWED: {
    base: [],
    fs: ["base"],
    "kernel-api": ["base"],
    tools: ["kernel-api"],
    "node-store": ["base", "fs", "kernel-api"],
    cli: "*",
  },
  THIRD_PARTY_OWNERS: { rusqlite: ["node-store"] },
  TEST_ONLY: ["tempfile"],
  PURE: ["serde", "tokio"],
  TOKIO_IO_FEATURES: { fs: ["fs"], process: [], net: [], "io-std": [], signal: [], full: [] },
  MAX_LINES: 20,
};

const ROOT_CLIPPY = `# 注释
allow-unwrap-in-tests = true
disallowed-methods = [
  { path = "std::fs::read", reason = "fs" },
  { path = "tokio::fs::symlink", reason = "fs", allow-invalid = true },
  { path = "std::process::Command::new", reason = "proc" },
  { path = "std::env::var", reason = "env" },
  { path = "std::time::SystemTime::now", reason = "clock" },
  { path = "uuid::Uuid::new_v4", reason = "ids" },
  { path = "std::path::Path::exists", reason = "fs" },
]
`;
const FS_CLIPPY = `allow-unwrap-in-tests = true
disallowed-methods = [
  { path = "std::process::Command::new", reason = "proc" },
  { path = "std::env::var", reason = "env" },
  { path = "std::time::SystemTime::now", reason = "clock" },
  { path = "uuid::Uuid::new_v4", reason = "ids" },
]
`;

const internal = new Map(
  Object.keys(policy.ALLOWED).map((crate) => [
    crate === "cli" ? "rust-cli" : `rust-cli-${crate}`,
    crate,
  ]),
);
const dep = (name, extra = {}) => ({ name, kind: null, features: [], path: null, ...extra });
const inside = (path) => path.startsWith("/w/packages/rust-cli/");

function packages() {
  return [
    { crate: "base", manifest: "crates/base/Cargo.toml", dependencies: [dep("serde")] },
    {
      crate: "fs",
      manifest: "crates/fs/Cargo.toml",
      dependencies: [
        dep("rust-cli-base", { path: "/w/packages/rust-cli/crates/base" }),
        dep("tokio", { features: ["fs", "rt"] }),
        dep("tempfile", { kind: "dev" }),
      ],
    },
    {
      crate: "kernel-api",
      manifest: "crates/kernel-api/Cargo.toml",
      dependencies: [dep("rust-cli-base")],
    },
    {
      crate: "tools",
      manifest: "crates/tools/Cargo.toml",
      dependencies: [
        dep("rust-cli-kernel-api"),
        // 测试依赖的 tokio 特性不受限制。
        dep("tokio", { kind: "dev", features: ["net", "process"] }),
      ],
    },
    {
      crate: "node-store",
      manifest: "crates/node-store/Cargo.toml",
      dependencies: [
        dep("rust-cli-base"),
        // `[dependencies.storage] package = "rust-cli-fs"`：cargo metadata 给的 name 是真实包名。
        dep("rust-cli-fs", { rename: "storage" }),
        dep("rust-cli-kernel-api"),
        dep("rusqlite"),
      ],
    },
    {
      crate: "cli",
      manifest: "crates/cli/Cargo.toml",
      dependencies: [dep("rust-cli-node-store"), dep("rust-cli-tools")],
    },
  ];
}

const graph = (list) => checkGraph(policy, list, internal, inside);
const edit = (list, crate, change) =>
  list.map((entry) =>
    entry.crate === crate ? { ...entry, dependencies: change(entry.dependencies) } : entry,
  );

function expectOnly(failures, pattern) {
  assert.equal(failures.length, 1, failures.join("\n"));
  assert.match(failures[0], pattern);
}

test("合成的基线没有违规", () => {
  assert.deepEqual(graph(packages()), []);
  assert.deepEqual(checkClippy(policy, ROOT_CLIPPY, { fs: FS_CLIPPY }), []);
});

test("依赖图：改名的依赖按真实包名核对，没登记的本工程依赖报错", () => {
  const list = edit(packages(), "tools", (deps) => [
    ...deps,
    dep("rust-cli-fs", { rename: "disk" }),
  ]);
  expectOnly(graph(list), /tools must not depend on fs/);
});

test("依赖图：允许表里没有实际依赖的项是死项", () => {
  const list = edit(packages(), "node-store", (deps) =>
    deps.filter((entry) => entry.name !== "rust-cli-fs"),
  );
  expectOnly(graph(list), /ALLOWED\.node-store: fs is not an actual dependency/);
});

test("依赖图：允许表本身不能越层（内核 crate 依赖 fs）", () => {
  const strict = { ...policy, ALLOWED: { ...policy.ALLOWED, tools: ["kernel-api", "fs"] } };
  const list = edit(packages(), "tools", (deps) => [...deps, dep("rust-cli-fs")]);
  expectOnly(checkGraph(strict, list, internal, inside), /kernel crates must not depend on fs/);
});

test("依赖图：没登记的 crate", () => {
  const list = [
    ...packages(),
    { crate: "extra", manifest: "crates/extra/Cargo.toml", dependencies: [] },
  ];
  const failures = graph(list);
  assert.ok(
    failures.some((failure) => /crates\/extra: not registered in ALLOWED/.test(failure)),
    failures.join("\n"),
  );
});

test("依赖图：path 依赖离开 packages/rust-cli", () => {
  const list = edit(packages(), "base", (deps) => [
    ...deps,
    dep("serde", { path: "/elsewhere/serde" }),
  ]);
  expectOnly(graph(list), /path dependency "\/elsewhere\/serde" leaves/);
});

test("第三方：IO 库只能由归属的 crate 依赖", () => {
  const list = edit(packages(), "tools", (deps) => [...deps, dep("rusqlite")]);
  expectOnly(graph(list), /only node-store may depend on rusqlite/);
});

test("第三方：没归类的库报错", () => {
  const list = edit(packages(), "tools", (deps) => [...deps, dep("hyper")]);
  expectOnly(graph(list), /hyper is not classified/);
});

test("第三方：只给测试用的库出现在普通依赖里", () => {
  const list = edit(packages(), "fs", (deps) => [...deps, dep("tempfile")]);
  expectOnly(graph(list), /tempfile may only be a dev-dependency/);
});

test("第三方：归属表与 PURE 的死项", () => {
  const list = edit(packages(), "node-store", (deps) =>
    deps.filter((entry) => entry.name !== "rusqlite"),
  );
  expectOnly(graph(list), /THIRD_PARTY_OWNERS\.rusqlite: node-store does not depend on it/);
  const list2 = edit(packages(), "base", () => []);
  expectOnly(graph(list2), /serde: listed as PURE/);
});

test("tokio：IO 特性只由登记的 crate 声明，登记了却不声明是死项", () => {
  const list = edit(packages(), "tools", (deps) => [
    ...deps,
    dep("tokio", { features: ["process"] }),
  ]);
  expectOnly(graph(list), /crates\/tools: tokio feature "process" is not registered/);
  const list2 = edit(packages(), "fs", (deps) =>
    deps.map((entry) => (entry.name === "tokio" ? { ...entry, features: ["rt"] } : entry)),
  );
  expectOnly(graph(list2), /TOKIO_IO_FEATURES\.fs: fs does not declare it/);
});

test("clippy.toml：禁令要能归类，allow-invalid 只给按平台 / 特性存在的路径", () => {
  const extra = ROOT_CLIPPY.replace(
    "]\n",
    '  { path = "chrono::Utc::now", reason = "x" },\n  { path = "std::fs::write", reason = "x", allow-invalid = true },\n]\n',
  );
  const failures = checkClippy(policy, extra, { fs: FS_CLIPPY });
  assert.ok(
    failures.some((failure) => /"chrono::Utc::now" belongs to no IO category/.test(failure)),
    failures.join("\n"),
  );
  assert.ok(
    failures.some((failure) => /"std::fs::write" may not use allow-invalid/.test(failure)),
    failures.join("\n"),
  );
});

test("clippy.toml：fs 的配置 = 根配置去掉文件类；别的 crate 不许有自己的配置", () => {
  const leaked = FS_CLIPPY.replace("]\n", '  { path = "std::fs::read", reason = "fs" },\n]\n');
  expectOnly(
    checkClippy(policy, ROOT_CLIPPY, { fs: leaked }),
    /fs\/clippy\.toml: entry std::fs::read differs/,
  );
  const missing = FS_CLIPPY.replace('  { path = "std::env::var", reason = "env" },\n', "");
  expectOnly(checkClippy(policy, ROOT_CLIPPY, { fs: missing }), /missing root entry std::env::var/);
  const settings = FS_CLIPPY.replace(
    "allow-unwrap-in-tests = true",
    "allow-unwrap-in-tests = false",
  );
  expectOnly(
    checkClippy(policy, ROOT_CLIPPY, { fs: settings }),
    /settings other than disallowed-methods differ/,
  );
  const failures = checkClippy(policy, ROOT_CLIPPY, { fs: FS_CLIPPY, tools: FS_CLIPPY });
  expectOnly(failures, /crates\/tools\/clippy\.toml: only crates in CRATE_LEVEL_OPEN/);
  expectOnly(checkClippy(policy, ROOT_CLIPPY, {}), /crates\/fs\/clippy\.toml: missing/);
});

const entries = parseClippy(ROOT_CLIPPY).entries;
const source = (crate, text, file = `crates/${crate}/src/lib.rs`) => ({ crate, file, text });
const sources = (list) => checkSources(policy, entries, list);
const exempt = (body) =>
  `#[expect(\n    clippy::disallowed_methods,\n    reason = "入口"\n)]\nfn f() {\n    ${body}\n}\n`;

test("源码：入口 crate 按类别豁免没有违规", () => {
  assert.deepEqual(
    sources([
      source(
        "node-store",
        exempt("let _ = std::time::SystemTime::now(); let _ = uuid::Uuid::new_v4();"),
      ),
      source(
        "cli",
        exempt('let _ = std::env::var("HOME"); let _ = std::path::Path::new("/").exists();'),
      ),
      source("fs", exempt("let _ = SystemTime::now();")),
    ]),
    [],
  );
});

test("源码：不是该类别入口的 crate 豁免", () => {
  expectOnly(
    sources([source("tools", exempt("let _ = fs::read(path);"))]),
    /tools is not an entry crate for file IO/,
  );
  // fs 只对文件类放开：进程类照样要登记。
  expectOnly(
    sources([source("fs", exempt('let _ = Command::new("git");'))]),
    /fs is not an entry crate for process IO/,
  );
});

test("源码：crate / 模块级豁免与 allow 都不行", () => {
  expectOnly(
    sources([source("fs", '#![expect(clippy::disallowed_methods, reason = "x")]\nfn f() {}\n')]),
    /crate \/ module level exemptions/,
  );
  const failures = sources([
    source("cli", exempt('let _ = std::env::var("A");').replace("expect", "allow")),
  ]);
  expectOnly(failures, /use #\[expect/);
});

test("源码：认不出被禁调用的豁免报错；注释与字符串里的路径不算", () => {
  const text = `#[expect(clippy::disallowed_methods, reason = "fs::read(path)")]\nfn f() {\n    // fs::read(path)\n    let _ = "fs::read(path)";\n    read(path);\n}\n`;
  expectOnly(sources([source("env-local", text)]), /covers no recognizable disallowed call/);
});

test("源码：同名的实例方法按文件里提到的类型归类（`.open(` 不因命名管道算成网络）", () => {
  const withNet = parseClippy(
    ROOT_CLIPPY.replace(
      "]\n",
      '  { path = "std::fs::OpenOptions::open", reason = "fs" },\n  { path = "tokio::net::windows::named_pipe::ClientOptions::open", reason = "net", allow-invalid = true },\n]\n',
    ),
  ).entries;
  const file = stripCode("use std::fs::OpenOptions;\nfn f() { OpenOptions::new().open(p); }\n");
  assert.deepEqual([...categoriesIn(policy, withNet, file, file)], ["file"]);
  const pipe = stripCode("use x::ClientOptions;\nfn f() { ClientOptions::new().open(p); }\n");
  assert.deepEqual([...categoriesIn(policy, withNet, pipe, pipe)], ["net"]);
});

test("F55 补的禁令：改环境、SystemTime::elapsed、改属主、按地址连 Unix 套接字都有类别，按入口核对", () => {
  const added = parseClippy(
    ROOT_CLIPPY.replace(
      "]\n",
      [
        '  { path = "std::env::remove_var", reason = "env" },',
        '  { path = "std::time::SystemTime::elapsed", reason = "clock" },',
        '  { path = "std::os::unix::fs::chown", reason = "fs", allow-invalid = true },',
        '  { path = "std::os::unix::net::UnixStream::connect_addr", reason = "net", allow-invalid = true },',
        "]\n",
      ].join("\n"),
    ),
  ).entries;
  const names = (path) => categoryOf(policy, path)?.name;
  assert.equal(names("std::env::remove_var"), "env");
  assert.equal(names("std::time::SystemTime::elapsed"), "clock");
  assert.equal(names("std::os::unix::fs::chown"), "file");
  assert.equal(names("std::os::unix::net::UnixStream::connect_addr"), "net");
  const check = (list) => checkSources(policy, added, list);
  // 入口删环境变量、fs 按锁的修改时间算年龄：是各自类别的入口。
  assert.deepEqual(
    check([
      source("cli", exempt('unsafe { std::env::remove_var("ARGV0") };')),
      source("fs", `use std::time::SystemTime;\n${exempt("let _ = modified.elapsed();")}`),
    ]),
    [],
  );
  expectOnly(
    check([source("tools", exempt('unsafe { std::env::remove_var("X") };'))]),
    /tools is not an entry crate for env IO/,
  );
  expectOnly(
    check([source("tools", `use std::time::SystemTime;\n${exempt("let _ = t.elapsed();")}`)]),
    /tools is not an entry crate for clock IO/,
  );
});

test("源码：豁免只覆盖下一个函数", () => {
  const text = `${exempt("let _ = fs::read(a);")}\nfn g() {\n    let _ = Command::new("x");\n}\n`;
  assert.deepEqual(sources([source("env-local", text)]), []);
});

test("源码：内核 crate 不用 Instant", () => {
  expectOnly(
    sources([source("tools", "use tokio::time::Instant;\nfn f() { let _ = Instant::now(); }\n")]),
    /kernel crates take time from the injected Clock/,
  );
  // 适配器可以用（超时与动画）。
  assert.deepEqual(
    sources([source("node-store", "fn f() { let _ = std::time::Instant::now(); }\n")]),
    [],
  );
});

test("源码：任务只经 base 启动，文件行数有上限，测试文件不查豁免", () => {
  expectOnly(
    sources([source("tools", "fn f() { tokio::spawn(async {}); }\n")]),
    /start tasks through/,
  );
  expectOnly(sources([source("tools", "\n".repeat(25))]), /exceeds 20 lines/);
  assert.deepEqual(
    sources([
      source(
        "tools",
        '#![allow(clippy::disallowed_methods, reason = "测试")]\n',
        "crates/tools/src/a_tests.rs",
      ),
    ]),
    [],
  );
});

test("stripCode：去掉注释、字符串、原始字符串与字符字面量，保留生命周期", () => {
  const code = stripCode(
    "let a = r#\"fs::read(\"#; let b = '\"'; fn x<'a>(s: &'a str) {} /* fs::read( */",
  );
  assert.equal(code.includes("fs::read("), false);
  assert.ok(code.includes("<'a>"));
  assert.deepEqual(
    [...categoriesIn(policy, entries, stripCode('Command::new("git")'))],
    ["process"],
  );
});

test("同一个名字只定义一处：`.zcode` 路径段只在 base 的 names.rs 的字面量里（F55，README §4.1）", () => {
  const names = source("base", 'pub const DOT_DIR: &str = ".zcode";\n', "crates/base/src/names.rs");
  // 只看 `.zcode` 一项（文件名的各项见下一个测试）。
  const only = { ...policy, SINGLE_DEFINITIONS: [real.SINGLE_DEFINITIONS[0]] };
  const definitions = (list) => checkDefinitions(only, [names, ...list]);
  // 用常量拼、注释里写、别的名字（`.zcode-plugin`、`.zcode.json`、`zcode.json`）、测试文件都不算。
  assert.deepEqual(
    definitions([
      source(
        "tools",
        'const A: &str = ".zcode-plugin/plugin.json";\nconst B: &str = ".zcode.json";\n',
      ),
      source(
        "tools",
        '// ~/.zcode/AGENTS.md\nfn f(root: &str) -> String { format!("{root}/{DOT_DIR}/x") }\n',
      ),
      source("cli", 'const C: &str = "zcode.json";\n'),
      source(
        "tools",
        'let p = "/w/.zcode/worktrees/a";\n',
        "crates/tools/src/worktree_tests_fakes.rs",
      ),
      source("context", 'let p = "/w/.zcode/rules";\n', "crates/context/src/a_tests.rs"),
    ]),
    [],
  );
  // 旧写法：路径段、`~/.zcode`、格式串、原始字符串、数组里的元素都报出行号。
  for (const text of [
    'style.join(&dir, &[".zcode", "rules"])',
    'const DB: &str = "~/.zcode/cli/db/db.sqlite";',
    'format!("{root}/.zcode/worktrees/")',
    'let t = r#"removed from .zcode/scheduled_tasks.json"#;',
    'const P: &[&str] = &[".vscode", ".zcode/agents"];',
  ]) {
    expectOnly(
      definitions([source("context", `fn f() {}\n${text}\n`)]),
      /crates\/context\/src\/lib\.rs:2: the product directory name \.zcode is defined only in crates\/base\/src\/names\.rs/,
    );
  }
  // 定义处没有这个字面量是死项。
  expectOnly(
    checkDefinitions(only, [source("base", "pub const X: u8 = 1;\n", "crates/base/src/names.rs")]),
    /does not define the product directory name \.zcode \(dead entry\)/,
  );
});

test("文件名只定义一处，只抓路径用途（F58，README §4.1）", () => {
  const names = source(
    "base",
    [
      'pub const DOT_DIR: &str = ".zcode";',
      'pub const RULES_FILE: &str = "AGENTS.md";',
      'pub const LOCAL_RULES_FILE: &str = "AGENTS.local.md";',
      'pub const PROJECT_CONFIG_FILE: &str = "zcode.json";',
      'pub const SCHEDULED_TASKS_FILE: &str = "scheduled_tasks.json";',
    ].join("\n"),
    "crates/base/src/names.rs",
  );
  const memory = source(
    "memory",
    'pub const ENTRYPOINT: &str = "MEMORY.md";\n',
    "crates/memory/src/entrypoint.rs",
  );
  const headers = source(
    "base",
    [
      'pub const CONTENT_TYPE: &str = "content-type";',
      'pub const RETRY_AFTER: &str = "retry-after";',
      'pub const USER_AGENT: &str = "user-agent";',
    ].join("\n"),
    "crates/base/src/proto/http.rs",
  );
  // F122 第二阶段：`.git`、空设备、MIME、`x-api-key` 的定义处。
  const git = source(
    "kernel-api",
    [
      'pub const VCS_DIRS: &[&str] = &[".git", ".svn"];',
      'pub const GIT_HOOKS_DIR: &str = ".git/hooks";',
    ].join("\n"),
    "crates/kernel-api/src/env_files.rs",
  );
  const devNull = source("base", 'pub const DEV_NULL: &str = "/dev/null";\n', "crates/base/src/path.rs");
  const mimes = source(
    "base",
    [
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
    ]
      .map((type, index) => `pub const M${index}: &str = "${type}";`)
      .join("\n"),
    "crates/base/src/proto/mime.rs",
  );
  const apiKey = source("net", 'pub const X_API_KEY: &str = "x-api-key";\n', "crates/net/src/header.rs");
  const definitions = (list) =>
    checkDefinitions(policy, [names, memory, headers, git, devNull, mimes, apiKey, ...list]);
  // 给用户或模型看的文字（前后有空格、标点）、别的名字（`.zcode.json`）、注释、测试文件都不算。
  assert.deepEqual(
    definitions([
      source("tui", 'const T: &str = "Create an AGENTS.md file in the project";\n'),
      source("tools", 'const U: &str = "edit .zcode.json or the MEMORY.md index.";\n'),
      source("policy", "// zcode.json 与 MEMORY.md\nconst V: u8 = 1;\n"),
      source("context", 'let p = "/w/AGENTS.md";\n', "crates/context/src/a_tests.rs"),
      // 头名只认整个字面量（F122）：带值的整行、别的头名不算。
      source("hooks", 'const H: &str = "content-type: text/plain";\nconst A: &str = "accept";\n'),
      // `.git` 只认路径用途的整段（F122 第二阶段）：参数文字、别的名字不算。
      source("env-local", 'const E: &[&str] = &["--exclude-dir=.git", ".gitignore", "a.git"];\n'),
    ]),
    [],
  );
  // 路径用途：整段、拼进路径的都报。
  for (const [text, name] of [
    ['let f = "AGENTS.md";', "the rules file name AGENTS\\.md"],
    ['format!("{dir}/AGENTS.local.md")', "the local rules file name AGENTS\\.local\\.md"],
    ['const C: &str = "zcode.json";', "the project config file name zcode\\.json"],
    [
      'let p = r"C:\\x\\scheduled_tasks.json";',
      "the scheduled tasks file name scheduled_tasks\\.json",
    ],
    ['join(&dir, &["memory", "MEMORY.md"])', "the memory index file name MEMORY\\.md"],
    ['headers.push(("Content-Type", json));', "the HTTP header name content-type"],
    ['let u = "user-agent";', "the HTTP header name user-agent"],
    ['dir.join(".git")', "the git directory name \\.git"],
    ['let h = ".git/hooks";', "the git directory name \\.git"],
    ['let n = "/dev/null";', "the null device /dev/null"],
    ['let m = "image/png";', "the MIME type image/png"],
    ['let k = "X-Api-Key";', "the HTTP header name x-api-key"],
  ]) {
    const failures = definitions([source("context", `fn f() {}\n${text}\n`)]);
    assert.equal(failures.length, 1, `${text}: ${failures}`);
    assert.match(failures[0], new RegExp(`crates/context/src/lib\\.rs:2: ${name}`), text);
  }
});

test("stringLiterals：普通、原始与字节串的原文与行号；注释与字符字面量不算", () => {
  assert.deepEqual(
    stringLiterals('let a = "x\\"y"; // "c"\nlet b = r#"q"z"#; let c = \'"\'; let d = b"e";'),
    [
      { value: 'x\\"y', line: 1 },
      { value: 'q"z', line: 2 },
      { value: "e", line: 2 },
    ],
  );
});

// 三类检查（docs/specs/rust-cli/remote-exec.md §15.6）：波及范围与稳定依赖，只用合成的依赖图。
const graphOf = (edges) =>
  new Map(Object.entries(edges).map(([crate, to]) => [crate, new Set(to)]));
const remote = {
  LAYERS: {
    base: "foundation",
    wire: "foundation",
    transport: "foundation",
    "kernel-api": "kernel",
    tools: "kernel",
    tui: "frontend",
    remote: "adapter",
    host: "entry",
    cli: "entry",
  },
  BLAST_FORBIDDEN_LAYERS: ["kernel", "frontend", "platform"],
  BLAST_RADIUS: { wire: ["cli", "host", "remote"], remote: ["cli", "host"] },
  SDP_EXCEPTIONS: {},
};
const remoteGraph = {
  base: [],
  wire: ["base"],
  transport: ["base"],
  "kernel-api": ["base"],
  tools: ["kernel-api"],
  tui: ["kernel-api"],
  remote: ["kernel-api", "wire"],
  host: ["remote", "transport"],
  cli: ["host", "remote", "tools", "tui"],
};

test("波及范围：合成的基线没有违规；闭包是全部直接与间接的依赖方", () => {
  const graph = graphOf(remoteGraph);
  assert.deepEqual([...reverseClosure(graph, "wire")].sort(), ["cli", "host", "remote"]);
  assert.deepEqual(checkBlastRadius(remote, graph), []);
});

test("波及范围：闭包与登记的不同、内核依赖远端 crate、登记了不存在的 crate", () => {
  const graph = graphOf({ ...remoteGraph, tools: ["kernel-api", "wire"] });
  const failures = checkBlastRadius(remote, graph);
  assert.ok(failures.some((line) => line.includes("crates depending on wire")));
  assert.ok(failures.some((line) => line.includes("tools (kernel) depends on wire")));
  const dead = checkBlastRadius({ ...remote, BLAST_RADIUS: { gone: [] } }, graphOf(remoteGraph));
  assert.deepEqual(dead, ["BLAST_RADIUS.gone: no such crate (dead entry)"]);
});

test("稳定依赖：I = 出度 /（入度 + 出度），只向更稳定的依赖；例外要登记、死项报错", () => {
  const graph = graphOf(remoteGraph);
  const values = instability(graph);
  assert.equal(values.get("base").i, 0);
  assert.equal(values.get("cli").i, 1);
  assert.equal(values.get("wire").i, 0.5);
  assert.deepEqual(checkStableDependencies(remote, graph), []);
  // stable 被三个 crate 依赖（I = 1/4），却依赖一个出度 2、入度 1 的 shaky（I = 2/3）。
  const worse = graphOf({
    stable: ["shaky"],
    shaky: ["x", "y"],
    x: [],
    y: [],
    a: ["stable"],
    b: ["stable"],
    c: ["stable"],
  });
  assert.deepEqual(checkStableDependencies(remote, worse), [
    "stable dependencies: stable (I=0.25) depends on the less stable shaky (I=0.67); depend on something more stable or register stable->shaky in SDP_EXCEPTIONS with a reason",
  ]);
  const allowed = { ...remote, SDP_EXCEPTIONS: { "stable->shaky": "reason" } };
  assert.deepEqual(checkStableDependencies(allowed, worse), []);
  const dead = { ...remote, SDP_EXCEPTIONS: { "cli->base": "reason" } };
  assert.deepEqual(checkStableDependencies(dead, graph), [
    "SDP_EXCEPTIONS.cli->base: no longer an exception (dead entry)",
  ]);
});
