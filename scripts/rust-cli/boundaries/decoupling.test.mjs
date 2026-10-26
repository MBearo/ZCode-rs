// 解耦检查的测试（docs/specs/rust-cli/app-protocol.md §9）：
//   node --test scripts/rust-cli/boundaries/decoupling.test.mjs
// 只用合成的依赖图、源码与 feature 表：先确认合成的基线没有违规，再逐项加一个违规，证明每条规则会失败。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  checkFeatures,
  checkFrontendBranching,
  checkProtocolLayering,
  checkVocabulary,
  frozenEnumVariants,
} from "./decoupling.mjs";
import * as real from "./policy.mjs";

const policy = {
  ...real,
  LAYERS: {
    base: "foundation",
    "kernel-api": "kernel",
    session: "kernel",
    "node-store": "adapter",
    platform: "platform",
    runtime: "kernel",
    "protocol-core": "frontend-shared",
    "protocol-zcode": "frontend",
    "protocol-acp": "frontend",
    tui: "frontend",
    headless: "frontend",
    cli: "entry",
  },
  ALLOWED: {},
  PLANNED_CRATES: {
    "protocol-mcp": { layer: "frontend", allowed: ["base", "kernel-api", "protocol-core"] },
  },
  PROTOCOL_TERM_EXCEPTIONS: [],
  FRONTEND_BRANCH_EXCEPTIONS: [],
  FROZEN_ENUMS: [
    { file: "crates/session/src/shape.rs", name: "Shape", variants: ["Wide", "Narrow"] },
  ],
  // 方法名表与真实清单脱钩：只放测试要用的几个。
  PROTOCOLS: {
    ...real.PROTOCOLS,
    zcode: {
      ...real.PROTOCOLS.zcode,
      methods: ["session/create", "interaction/requestPermission", "v4/conversation/subscribe"],
    },
  },
};

/** 依赖图：crate → 它依赖的本工程 crate。 */
function graph(extra = {}) {
  const base = {
    base: [],
    "kernel-api": ["base"],
    session: ["base", "kernel-api"],
    runtime: ["kernel-api", "session"],
    "node-store": ["base", "kernel-api"],
    platform: ["base", "kernel-api"],
    "protocol-core": ["base", "kernel-api"],
    "protocol-zcode": ["base", "kernel-api", "platform", "protocol-core"],
    "protocol-acp": ["base", "kernel-api", "protocol-core"],
    tui: ["base", "kernel-api", "runtime"],
    cli: ["base", "kernel-api", "runtime", "protocol-zcode", "protocol-acp", "tui", "node-store"],
  };
  return new Map(
    Object.entries({ ...base, ...extra }).map(([crate, targets]) => [crate, new Set(targets)]),
  );
}

const CLEAN_SESSION = `//! 会话 actor。
pub enum Shape {
    Wide,
    Narrow,
}
pub fn run(shape: Shape) -> u32 {
    let label = "turn/started";
    match shape { Shape::Wide => 1, Shape::Narrow => 2 }
}
`;

function sources(extra = []) {
  return [
    { crate: "session", file: "crates/session/src/shape.rs", text: CLEAN_SESSION },
    {
      crate: "protocol-zcode",
      file: "crates/protocol-zcode/src/lib.rs",
      text: 'pub const COMMAND: &str = "v4/command"; // ZCode Protocol V4 的 logEpoch\n',
    },
    {
      crate: "protocol-acp",
      file: "crates/protocol-acp/src/lib.rs",
      text: 'pub const PROMPT: &str = "session/prompt"; // ACP 的 agentCapabilities\n',
    },
    {
      crate: "cli",
      file: "crates/cli/src/main.rs",
      text: 'const SUB: &str = "app-server";\n',
    },
    ...extra,
  ];
}

test("合成的基线没有违规", () => {
  assert.deepEqual(checkProtocolLayering(policy, graph()), []);
  assert.deepEqual(checkVocabulary(policy, sources()), []);
  assert.deepEqual(checkFrontendBranching(policy, sources()), []);
  assert.deepEqual(checkFeatures(policy, [{ crate: "kernel-api", features: ["testing"] }]), []);
});

test("依赖：内核、适配器、平台层依赖协议 crate 或协议框架都报错", () => {
  for (const [crate, target] of [
    ["session", "protocol-zcode"],
    ["node-store", "protocol-core"],
    ["platform", "protocol-acp"],
  ]) {
    const failures = checkProtocolLayering(
      policy,
      graph({ [crate]: [...graph().get(crate), target] }),
    );
    assert.ok(
      failures.some((line) => line.includes(`${crate} (`) && line.includes(target)),
      `${crate} -> ${target}: ${failures.join("\n")}`,
    );
  }
});

test("依赖：前端只被入口依赖；协议框架只被前端与入口依赖", () => {
  const failures = checkProtocolLayering(
    policy,
    graph({ runtime: ["kernel-api", "session", "tui"] }),
  );
  assert.ok(failures.some((line) => line.includes("tui") && line.includes("runtime")));
});

test("依赖：协议 crate 互不依赖（直接与间接）", () => {
  const direct = checkProtocolLayering(
    policy,
    graph({ "protocol-acp": ["base", "kernel-api", "protocol-core", "protocol-zcode"] }),
  );
  assert.ok(
    direct.some((line) => line.includes("protocol-acp") && line.includes("protocol-zcode")),
  );
  // 间接：协议框架依赖了一个协议 crate，另一个协议经框架间接依赖它。
  const indirect = checkProtocolLayering(
    policy,
    graph({ "protocol-core": ["base", "kernel-api", "protocol-zcode"] }),
  );
  assert.ok(
    indirect.some((line) => line.includes("protocol-acp") && line.includes("protocol-zcode")),
  );
});

test("依赖：规划中的 crate 建好了还留在 PLANNED_CRATES、或规划的依赖越层，都报错", () => {
  const built = checkProtocolLayering(
    policy,
    new Map([...graph(), ["protocol-mcp", new Set(["base", "kernel-api", "protocol-core"])]]),
  );
  assert.ok(built.some((line) => line.includes("PLANNED_CRATES.protocol-mcp")));
  const overreach = checkProtocolLayering(
    {
      ...policy,
      PLANNED_CRATES: { "protocol-mcp": { layer: "frontend", allowed: ["session"] } },
    },
    graph(),
  );
  assert.ok(
    overreach.some(
      (line) => line.includes("PLANNED_CRATES.protocol-mcp") && line.includes("session"),
    ),
  );
});

test("词汇：内核里出现 App 协议的名字、方法名、投递模式都报错（注释也算）", () => {
  for (const [text, id] of [
    ["// 给 ZCode Protocol 用\n", "zcode-protocol"],
    ["pub struct AppServerOptions;\n", "app-server"],
    ["// V4 的投影要这个字段\n", "v4-name"],
    ['let method = "v4/command";\n', "v4-method"],
    ["pub log_epoch: u64,\n", "log-epoch"],
    ['let mode = "desktop-continuous";\n', "client-mode"],
    ["pub base_revision: u64,\n", "command-cas"],
    ['let topic = "sessions-index/ws";\n', "topic"],
    ['let method = "interaction/requestPermission";\n', "host-method"],
    ['let method = "session/create";\n', "host-method"],
    ["let key = workspace_key;\n", "workspace-key"],
  ]) {
    const failures = checkVocabulary(
      policy,
      sources([{ crate: "session", file: "crates/session/src/bad.rs", text }]),
    );
    assert.ok(
      failures.some((line) => line.includes("crates/session/src/bad.rs:1") && line.includes(id)),
      `${JSON.stringify(text)}: ${failures.join("\n")}`,
    );
  }
});

test("词汇：同名空间的方法按精确的名字归属：ACP 的 session/prompt 不算 ZCode 的方法，ZCode 的 session/create 不能进 ACP", () => {
  const acpOwn = checkVocabulary(policy, sources());
  assert.deepEqual(acpOwn, []);
  const failures = checkVocabulary(
    policy,
    sources([
      {
        crate: "protocol-acp",
        file: "crates/protocol-acp/src/legacy.rs",
        text: 'let m = "session/create";\n',
      },
    ]),
  );
  assert.ok(failures.some((line) => line.includes("protocol-acp") && line.includes("host-method")));
});

test("词汇：ACP 的词不进内核，也不进另一个协议 crate；ZCode 的词不进 ACP", () => {
  const kernel = checkVocabulary(
    policy,
    sources([
      { crate: "session", file: "crates/session/src/acp.rs", text: "// ACP 的 session/update\n" },
    ]),
  );
  assert.ok(
    kernel.some(
      (line) => line.includes("crates/session/src/acp.rs:1") && line.includes("acp-name"),
    ),
  );
  const cross = checkVocabulary(
    policy,
    sources([
      {
        crate: "protocol-acp",
        file: "crates/protocol-acp/src/bridge.rs",
        text: 'let m = "v4/conversation/subscribe";\n',
      },
    ]),
  );
  assert.ok(cross.some((line) => line.includes("protocol-acp") && line.includes("v4-method")));
  const framework = checkVocabulary(
    policy,
    sources([
      {
        crate: "protocol-core",
        file: "crates/protocol-core/src/rpc.rs",
        text: "pub struct ClientMode;\n",
      },
    ]),
  );
  assert.ok(
    framework.some((line) => line.includes("protocol-core") && line.includes("client-mode")),
  );
});

test("词汇：stream-json 的词只查代码与字面量，内核注释里的说明不算", () => {
  const comment = checkVocabulary(
    policy,
    sources([
      { crate: "session", file: "crates/session/src/print.rs", text: "// stream-json 输入时\n" },
    ]),
  );
  assert.deepEqual(comment, []);
  const code = checkVocabulary(
    policy,
    sources([
      { crate: "session", file: "crates/session/src/print.rs", text: "let stream_json = true;\n" },
    ]),
  );
  assert.ok(code.some((line) => line.includes("stream-json")));
});

test("词汇：不是协议词的相近写法不报（IPv4、uuid v4、URL 的 /v4/、MCP 的方法名）", () => {
  const text = [
    "use std::net::Ipv4Addr;",
    "fn private_v4(ip: Ipv4Addr) -> bool { true }",
    "// uuid v4，与 Node createUuid() 同一格式",
    'let url = "https://api.example.com/v4/chat/completions";',
    'let a = "tools/list"; let b = "notifications/initialized"; let c = "initialize";',
    "let mode = PermissionMode::Plan;",
  ].join("\n");
  assert.deepEqual(
    checkVocabulary(
      policy,
      sources([{ crate: "session", file: "crates/session/src/ok.rs", text }]),
    ),
    [],
  );
});

test("词汇：例外只按文件与精确片段放过，没用到的例外是死项", () => {
  const text = "// 第二阶段的 ZCode Protocol 前端也一样\n";
  const extra = [{ crate: "kernel-api", file: "crates/kernel-api/src/session_api.rs", text }];
  const exception = {
    file: "crates/kernel-api/src/session_api.rs",
    fragment: "ZCode Protocol 前端",
    why: "已有的说明",
  };
  assert.deepEqual(
    checkVocabulary({ ...policy, PROTOCOL_TERM_EXCEPTIONS: [exception] }, sources(extra)),
    [],
  );
  // 同一个片段在别的文件里不放过。
  const elsewhere = checkVocabulary(
    { ...policy, PROTOCOL_TERM_EXCEPTIONS: [exception] },
    sources([...extra, { crate: "session", file: "crates/session/src/x.rs", text }]),
  );
  assert.ok(elsewhere.some((line) => line.startsWith("crates/session/src/x.rs:1")));
  const dead = checkVocabulary(
    { ...policy, PROTOCOL_TERM_EXCEPTIONS: [{ ...exception, fragment: "不存在的片段" }] },
    sources(extra),
  );
  assert.ok(dead.some((line) => line.includes("dead entry")));
});

test('按前端分支：内核里的 frontend / app_mode / surface / cfg(feature = "app") 写法报错', () => {
  for (const [text, id] of [
    ["fn f(frontend: Frontend) {}\n", "frontend"],
    ["if app_mode { return; }\n", "app-mode"],
    ["if is_desktop { return; }\n", "app-mode"],
    ["match surface { _ => {} }\n", "surface"],
    ['#[cfg(feature = "app")]\nfn g() {}\n', "cfg-feature"],
    ["#[cfg(desktop)]\nfn g() {}\n", "cfg-frontend"],
  ]) {
    const failures = checkFrontendBranching(
      policy,
      sources([{ crate: "session", file: "crates/session/src/branch.rs", text }]),
    );
    assert.ok(
      failures.some((line) => line.includes("crates/session/src/branch.rs") && line.includes(id)),
      `${JSON.stringify(text)}: ${failures.join("\n")}`,
    );
  }
  // 注释与字符串里的这些词不算（只看代码）。
  assert.deepEqual(
    checkFrontendBranching(
      policy,
      sources([
        {
          crate: "session",
          file: "crates/session/src/branch.rs",
          text: '// frontend 只发命令\nlet a = "surface";\n#[cfg(feature = "testing")]\nfn t() {}\n',
        },
      ]),
    ),
    [],
  );
});

test("按前端分支：登记的“形态”枚举加了变体就报错", () => {
  const added = CLEAN_SESSION.replace("    Narrow,\n", "    Narrow,\n    Desktop,\n");
  const failures = checkFrontendBranching(policy, [
    { crate: "session", file: "crates/session/src/shape.rs", text: added },
  ]);
  assert.ok(failures.some((line) => line.includes("Shape") && line.includes("Desktop")));
  assert.deepEqual(frozenEnumVariants(added, "Shape"), ["Wide", "Narrow", "Desktop"]);
  const missing = checkFrontendBranching(policy, [
    { crate: "session", file: "crates/session/src/other.rs", text: "fn x() {}\n" },
  ]);
  assert.ok(missing.some((line) => line.includes("FROZEN_ENUMS") && line.includes("not found")));
});

test("feature：内核、适配器、平台层的 crate 只能有登记的 feature", () => {
  const failures = checkFeatures(policy, [
    { crate: "session", features: ["app"] },
    { crate: "protocol-zcode", features: ["desktop"] },
  ]);
  assert.equal(failures.length, 1);
  assert.ok(failures[0].includes("session") && failures[0].includes("app"));
});
