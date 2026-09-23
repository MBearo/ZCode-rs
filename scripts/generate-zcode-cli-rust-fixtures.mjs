// Run with node --import tsx. Generates Rust parity fixtures by executing the TS
// implementations on fixed inputs. Rust table tests must reproduce every output.
import { readFile, writeFile } from "node:fs/promises";
import { format } from "oxfmt";
import { parseConfigFileToRuntimePatchWithDiagnostics } from "../apps/zcode-cli/packages/adapters/src/config/schema.ts";
import {
  createPrioritizedConfig,
  mergeConfigs,
} from "../apps/zcode-cli/packages/adapters/src/config/config-merger.ts";
import { parseEnvConfig } from "../apps/zcode-cli/packages/adapters/src/config/env-config.adapter.ts";
import { createConfigPort } from "../apps/zcode-cli/packages/adapters/src/config/index.ts";
import { DefaultRuntimeConfig } from "../apps/zcode-cli/packages/contracts/src/config/index.ts";

const files = {
  empty: {},
  network: { network: { httpProxy: "proxy.local:8080", noProxy: "localhost", unknown: 1 } },
  invalidType: { network: { timeout: "slow" } },
  notObject: [1, 2],
  mcpLegacy: {
    mcp: {
      servers: {
        stdio: { command: "node", args: ["a.js"], environment: { A: "1" }, enable: false },
        remote: { type: "remote", url: "https://x.test/mcp", http_headers: { K: "v" } },
        inferred: { url: "https://y.test" },
        conflict: { command: "x", enable: true, enabled: false },
        timeouts: { command: "x", timeout: 5, startup_timeout_sec: 3 },
        bad: { type: "stdio" },
        badOauth: { type: "http", url: "https://z", oauth: { type: "client_credentials" } },
      },
    },
  },
  mcpNotObject: { mcp: { servers: [1] } },
  plugins: {
    plugins: {
      enabledPlugins: { "zcode-cua@zcode-plugins-official": true, "a@m": false },
      options: { "zcode-cua@zcode-plugins-official": { k: 1 } },
      suppressedBuiltins: [
        "zcode-cua@zcode-plugins-official",
        "computer-use@zcode-plugins-official",
        "x@m",
      ],
      dirs: ["/p"],
    },
  },
  skills: {
    skills: {
      enabled: false,
      roots: ["/r"],
      "/abs/SKILL.md": { enable: false },
      rel: { enable: true },
    },
    skill: { "/other/SKILL.md": { enable: true } },
    command: { "/c.md": { enable: false } },
  },
  hooks: {
    hooks: {
      enabled: true,
      events: { PreToolUse: [{ matcher: "Bash", hooks: [{ type: "command", command: "echo" }] }] },
    },
  },
  passthrough: {
    custom: { any: [1] },
    ui: { locale: "zh-CN" },
    toolConcurrency: { maxConcurrency: 4 },
  },
};

const parse = Object.entries(files).map(([name, value]) => {
  try {
    const result = parseConfigFileToRuntimePatchWithDiagnostics(value);
    return {
      name,
      input: value,
      loaded: true,
      patch: result.config,
      diagnostics: result.diagnostics.map((d) => ({
        code: d.code,
        path: d.path ?? null,
        severity: d.severity,
      })),
    };
  } catch {
    return { name, input: value, loaded: false, patch: {}, diagnostics: [] };
  }
});

const layers = [
  {
    name: "sections replace lower layers, plugins and hooks merge",
    layers: [
      ["system", DefaultRuntimeConfig],
      [
        "user",
        {
          network: { httpProxy: "http://user" },
          plugins: {
            dirs: ["/u"],
            enabledPlugins: { "a@m": true },
            options: { "a@m": { x: 1 } },
            extraKnownMarketplaces: { m: { source: { source: "github", repo: "o/r" } } },
          },
          hooks: {
            enabled: true,
            events: { Stop: [{ hooks: [{ type: "command", command: "u" }] }] },
          },
          mcp: { servers: { u: { type: "stdio", command: "u" } } },
        },
      ],
      [
        "project",
        {
          network: { timeout: 5000 },
          plugins: {
            dirs: ["/p", "/u"],
            enabledPlugins: { "b@m": true },
            options: { "a@m": { y: 2 } },
            extraKnownMarketplaces: { p: { source: { source: "github", repo: "p/r" } } },
          },
          hooks: {
            enabled: false,
            events: { Stop: [{ hooks: [{ type: "command", command: "p" }] }] },
          },
        },
      ],
      ["env", { network: { noProxy: "*" }, toolConcurrency: { maxConcurrency: 2 } }],
    ],
  },
  {
    name: "later project file replaces earlier mcp section",
    layers: [
      ["project", { mcp: { servers: { a: { type: "stdio", command: "a" } } } }],
      ["project", { mcp: { servers: { b: { type: "stdio", command: "b" } } } }],
    ],
  },
  { name: "empty", layers: [] },
];
const merge = layers.map(({ name, layers: input }) => {
  const merged = mergeConfigs(
    ...input.map(([scope, patch]) => createPrioritizedConfig(patch, scope)),
  );
  return { name, layers: input, merged, effective: createConfigPort(merged).getAll() };
});

const envs = [
  [
    ["ZCODE_HTTP_PROXY", "http://p"],
    ["ZCODE_NO_PROXY", "a,b"],
    ["ZCODE_AGENT_CA_CERT", "/ca.pem"],
  ],
  [
    ["ZCODE_HTTP_TIMEOUT", "abc"],
    ["ZCODE_MAX_TOOL_CONCURRENCY", " 0x10 "],
    ["ZCODE_LOG_FORMAT", "JSON"],
  ],
  [
    ["ZCODE_TIMEOUT", "1e3"],
    ["ZCODE_HTTP_TIMEOUT", ""],
    ["ZCODE_SESSION_DB", "/db"],
    ["ZCODE_STORAGE_DIR", "/s"],
    ["OTHER", "x"],
  ],
  [
    ["ZCODE_LOG_FORMAT", "yaml"],
    ["ZCODE_MAX_TOOL_CONCURRENCY", "2.5"],
  ],
];
const env = envs.map((pairs) => ({
  env: pairs,
  expected: parseEnvConfig(Object.fromEntries(pairs)),
}));

const data = { config: { parse, merge, env } };
const path = new URL("../apps/zcode-cli-rust/crates/domain/fixtures/config.json", import.meta.url);
const formatted = await format(path.pathname, `${JSON.stringify(data, null, 2)}\n`);
if (formatted.errors.length) throw new Error("Cannot format Rust fixtures");
if (process.argv.includes("--check")) {
  if ((await readFile(path, "utf8")) !== formatted.code)
    throw new Error("Rust parity fixtures differ from TS; regenerate them and fix Rust");
} else await writeFile(path, formatted.code);
