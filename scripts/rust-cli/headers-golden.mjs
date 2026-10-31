#!/usr/bin/env node
// 模型请求头的 Node 对照夹具（docs/specs/rust-cli/model.md §2.4，工作线 F64）：用 Node 的头组装函数生成两份夹具，
// 本工程对同一输入必须给出相同的结果。
//
// - `packages/rust-cli/crates/cli/tests/source-headers-golden.json`：来源头（`buildCliZCodeSourceHeaders`，Intl、
//   process.platform、os.arch / os.release 换成固定值）、locale 与时区（每个值起一个只带这个变量的 node 进程读
//   `Intl.DateTimeFormat().resolvedOptions()`）。cli `request_headers_tests.rs` 比较。
// - `packages/rust-cli/crates/model/tests/fixtures/attribution-golden.json`：每请求头
//   （`createModelRequestAttributionHeaders`）与同名头的合并（`mergeModelRequestHeaders`）。model 的
//   `attribution_tests.rs`、`request_headers_tests.rs` 比较。
//
//   node scripts/rust-cli/headers-golden.mjs [--node-root <装好依赖的检出>] [--check]
//
// `--check`：不写文件，生成的内容与仓库里的不同时以 1 退出（node-interop-check.mjs 第十七段调用）。只调用纯函数，
// 不联网；导入期间 HOME 指向临时目录（`withHome`），不读本机的 ~/.zcode。
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createRequire, syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { OPENCODE_GO, ZAI_ANTHROPIC } from "./fixture/model.mjs";
import { ensureTypeScript, importNode, nodeRootFromArgs, repoRoot, withHome } from "./node-root.mjs";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
ensureTypeScript(import.meta.filename, nodeRoot, args);
const check = args.includes("--check");

const SOURCE_GOLDEN = join(repoRoot, "packages/rust-cli/crates/cli/tests/source-headers-golden.json");
const ATTRIBUTION_GOLDEN = join(
  repoRoot,
  "packages/rust-cli/crates/model/tests/fixtures/attribution-golden.json",
);

/** 只带这些变量（和 PATH）的 node 进程读到的 Intl 缺省值。 */
function intl(env) {
  const result = spawnSync(
    process.execPath,
    ["-p", "JSON.stringify(Intl.DateTimeFormat().resolvedOptions())"],
    { env: { PATH: process.env.PATH, ...env }, encoding: "utf8" },
  );
  if (result.status !== 0) throw new Error(`node -p failed: ${result.stderr}`);
  return JSON.parse(result.stdout);
}

// 本工程照做的写法（model.md §2.4 的读法）；ICU 另有的边角写法不在这里。
const LOCALES = [
  {},
  { LANG: "" },
  ...[
    "C",
    "POSIX",
    "C.UTF-8",
    "POSIX.UTF-8",
    "en_US.UTF-8",
    "zh_CN.UTF-8",
    "zh_TW.UTF-8",
    "ja_JP.UTF-8",
    "ko_KR.UTF-8",
    "de_DE.UTF-8",
    "fr_FR.ISO8859-1",
    "pt_BR",
    "en_GB.UTF-8",
    "zh_CN.GB18030",
    "en",
    "en.UTF-8",
    "zh_Hans_CN.UTF-8",
    "zh_hant_tw",
    "ZH_cn.utf8",
    "EN_us",
    "en-us",
    "es_419",
    "en_001",
    "en_840",
    "xx_YY.UTF-8",
    "fil_PH",
    "yue_HK",
    "nb_NO",
    "no_NO",
    "iw_IL",
    "in_ID",
    "ji",
    "jw_ID",
    "mo_MD",
    "sh_RS",
    "de_DE@euro",
    "de_DE.UTF-8@euro",
    "en_us.utf8@euro",
    "sr_RS@latin",
    "ca_ES@valencia",
    "zh_CN@pinyin",
    "no@nynorsk",
    "en_US@x",
    "en_US@posix",
    "de_DE@posix",
    "en_US_POSIX",
    "en__POSIX",
    "de_DE_1901",
    "en_Latn",
    "en_Latn_US_FOO",
    "ab_CD_EFGHI_JK",
    "aa_BB_cccc",
    "en_U",
    "en_US_x",
    " en_US",
    "_US",
    "en_",
    "root",
    "abcdefghi",
    "e",
    "c",
    "posix",
    "en_US.UTF-8 ",
  ].map((LANG) => ({ LANG })),
  { LC_ALL: "de_DE.UTF-8", LC_MESSAGES: "ja_JP.UTF-8", LANG: "fr_FR.UTF-8" },
  { LC_MESSAGES: "ja_JP", LANG: "fr_FR" },
  { LC_ALL: "", LANG: "fr_FR" },
  { LC_MESSAGES: "", LANG: "fr_FR" },
  { LC_CTYPE: "ja_JP", LANG: "fr_FR" },
  { LANGUAGE: "ja", LANG: "fr_FR" },
];

// 设了 TZ 的写法（没设时 Node 读本机的 /etc/localtime，不进夹具）。
const TIME_ZONES = [
  "",
  "UTC",
  "Etc/UTC",
  "UCT",
  "Etc/UCT",
  "Universal",
  "Etc/Universal",
  "Zulu",
  "Etc/Zulu",
  "GMT0",
  "GMT+0",
  "GMT-0",
  "Etc/GMT",
  "Etc/GMT0",
  "Etc/GMT+0",
  "Etc/GMT-0",
  "Greenwich",
  "Etc/Greenwich",
  "GMT",
  "Asia/Shanghai",
  ":Asia/Shanghai",
  "/usr/share/zoneinfo/Asia/Tokyo",
  ":/usr/share/zoneinfo/Asia/Tokyo",
  "Asia/Shanghai ",
  "Europe/London",
  "America/New_York",
  "Etc/GMT-14",
  "America/Buenos_Aires",
  "Europe/Kiev",
  "Asia/Calcutta",
  "Asia/Kolkata",
  "Asia/Ho_Chi_Minh",
  "Asia/Kathmandu",
  "Asia/Yangon",
  "Europe/Kyiv",
  "America/Nuuk",
  "Atlantic/Faroe",
  "Pacific/Chuuk",
  "Pacific/Pohnpei",
  "Pacific/Kanton",
  "America/Atikokan",
  "America/Argentina/Buenos_Aires",
  "America/Indiana/Indianapolis",
  "America/Kentucky/Louisville",
  "Africa/Asmara",
];

const BASE = {
  env: {},
  appVersion: "0.1.52",
  locale: "en-US",
  timeZone: "Asia/Shanghai",
  platform: "darwin",
  arch: "arm64",
  release: "24.6.0",
};
const SOURCE_CASES = [
  BASE,
  { ...BASE, env: { ZCODE_ENV: " TEST " } },
  { ...BASE, env: { ZCODE_ENV: "test", ZCODE_TEST_BASE_URL: "https://test.example.com/x" } },
  { ...BASE, env: { ZCODE_BASE_URL: "https://zcode.example.com:8443/path" } },
  { ...BASE, env: { ZCODE_APP_VERSION: "3.4.5" } },
  { ...BASE, env: { ZCODE_APP_VERSION: "" } },
  { ...BASE, env: { ZCODE_APP_VERSION: " 3.4.5-beta.1 " } },
  { ...BASE, appVersion: "版本" },
  { ...BASE, appVersion: "" },
  { ...BASE, locale: undefined, timeZone: undefined },
  { ...BASE, locale: "de-DE-x-lvariant-euro", timeZone: "Asia/Tokyo é" },
  { ...BASE, platform: "win32", arch: "x64", release: "10.0.22631" },
  { ...BASE, platform: "linux", arch: "arm64", release: "6.8.0-45-generic" },
  { ...BASE, platform: "linux", arch: "x64", release: " " },
  { ...BASE, platform: "freebsd", arch: "x64", release: "14.1-RELEASE" },
];

const require = createRequire(import.meta.url);

/** 在固定的 Intl、平台与系统值下调 Node 的 `buildCliZCodeSourceHeaders`。 */
async function sourceHeaders(build, testCase) {
  const os = require("node:os");
  const saved = {
    DateTimeFormat: Intl.DateTimeFormat,
    platform: Object.getOwnPropertyDescriptor(process, "platform"),
    arch: os.arch,
    release: os.release,
  };
  Intl.DateTimeFormat = function DateTimeFormat() {
    return { resolvedOptions: () => ({ locale: testCase.locale, timeZone: testCase.timeZone }) };
  };
  Object.defineProperty(process, "platform", { value: testCase.platform, configurable: true });
  os.arch = () => testCase.arch;
  os.release = () => testCase.release;
  syncBuiltinESMExports();
  try {
    return Object.entries(build(testCase.env, { appVersion: testCase.appVersion, sourceTitle: "cli" }));
  } finally {
    Intl.DateTimeFormat = saved.DateTimeFormat;
    Object.defineProperty(process, "platform", saved.platform);
    os.arch = saved.arch;
    os.release = saved.release;
    syncBuiltinESMExports();
  }
}

// 本工程的请求来源 → Node 的用途名（model.md §2.4 的用途表；Node 的出处：`querySourceForTask`、`compact-active.ts`、
// `SESSION_TITLE_QUERY_SOURCE`、`webfetch-processing.ts`、`websearch.ts`）。其余照本工程的名字发。
const SOURCES = [
  [{ kind: "main_thread" }, "main_turn"],
  [{ kind: "subagent", name: "Explore" }, "subagent"],
  [{ kind: "compact" }, "compact"],
  [{ kind: "side", name: "generate_session_title" }, "session_title"],
  [{ kind: "side", name: "web_fetch_apply" }, "web_fetch_processing"],
  [{ kind: "side", name: "web_search_tool" }, "web_search_tool"],
  [{ kind: "side", name: "hook_prompt" }, "hook_prompt"],
  [{ kind: "side", name: "rename_generate_name" }, "rename_generate_name"],
  [{ kind: "side", name: "has space" }, "has space"],
  [{ kind: "side", name: "a".repeat(129) }, "a".repeat(129)],
];

/** 每请求头的用例：本工程的输入，以及按 Node 的写法换算出的 `statusContext`。 */
function attributionCases() {
  const owners = [
    { session: "sess_2d480878-38af-4bae-a54f-096e2fb0f4d7", run: "run_r1", agent: null },
    { session: "sess_subagent_agent_a1", run: "run_r2", agent: "agent_a1" },
    { session: "subagent_agent_a2", run: null, agent: "agent_a2" },
    { session: "sess_", run: null, agent: null },
    { session: null, run: null, agent: null },
  ];
  const cases = [];
  for (const owner of owners)
    for (const [source, querySource] of SOURCES)
      for (const baseUrl of [ZAI_ANTHROPIC, `${OPENCODE_GO}/`])
        cases.push({ ...owner, source, querySource, baseUrl });
  return cases;
}

const MERGE_CASES = [
  [{ "User-Agent": "ZCode/1" }, { "user-agent": "ProbeUA/9" }],
  [{ "X-Title": "a", Accept: "b" }, { ACCEPT: "c" }, { "x-title": "d", "X-New": "e" }],
  [{ "anthropic-version": "2023-06-01" }, {}, { "Anthropic-Version": "2099-01-01" }],
  [{ a: "1", B: "2", c: "3" }, { b: "4" }],
];

const home = await mkdtemp(join(tmpdir(), "rust-cli-headers-golden-"));
try {
  const golden = await withHome(home, async () => {
    const { buildCliZCodeSourceHeaders } = await importNode(nodeRoot, "bootstrap/src/model-config.ts");
    const { createModelRequestAttributionHeaders } = await importNode(
      nodeRoot,
      "adapters/src/model/runner-attribution.ts",
    );
    const { mergeModelRequestHeaders } = await importNode(
      nodeRoot,
      "adapters/src/model/model-request-headers.ts",
    );
    const source = [];
    for (const testCase of SOURCE_CASES)
      source.push({ ...testCase, headers: await sourceHeaders(buildCliZCodeSourceHeaders, testCase) });
    const locales = LOCALES.map((env) => ({ env, locale: intl(env).locale }));
    const timeZones = TIME_ZONES.map((tz) => ({ tz, timeZone: intl({ TZ: tz }).timeZone ?? null }));
    const attribution = attributionCases().map((testCase) => ({
      ...testCase,
      headers: Object.entries(
        createModelRequestAttributionHeaders({
          baseURL: testCase.baseUrl,
          // 本工程的回合 `run_<x>` 对应 Node 的 query id `query_<x>`（两边各去掉自己的前缀）。
          queryId: testCase.run ? `query_${testCase.run.slice("run_".length)}` : undefined,
          querySource: testCase.querySource,
          requestId: "req-1",
          sessionId: testCase.session ?? undefined,
          traceId: "trace-1",
          modelRequestSessionType: testCase.agent ? "subagent" : testCase.session ? "main" : undefined,
        }),
      ),
    }));
    const merge = MERGE_CASES.map((sources) => ({
      sources: sources.map((headers) => Object.entries(headers)),
      merged: Object.entries(mergeModelRequestHeaders(...sources)),
    }));
    return {
      source: { source, locales, timeZones },
      attribution: { attribution, merge },
    };
  });
  const outputs = [
    [SOURCE_GOLDEN, golden.source],
    [ATTRIBUTION_GOLDEN, golden.attribution],
  ];
  let stale = 0;
  for (const [file, value] of outputs) {
    const text = `${JSON.stringify(value, null, 2)}\n`;
    if (check) {
      const current = await readFile(file, "utf8").catch(() => "");
      if (current !== text) {
        console.error(`headers-golden: ${file} is out of date (run node scripts/rust-cli/headers-golden.mjs)`);
        stale += 1;
      }
    } else {
      await writeFile(file, text);
      console.log(`headers-golden: ${file}`);
    }
  }
  if (stale) process.exitCode = 1;
  else if (check) console.log("headers-golden: up to date");
} finally {
  await rm(home, { recursive: true, force: true });
}
