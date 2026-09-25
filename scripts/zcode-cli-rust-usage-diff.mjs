// Run with TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json node --import tsx.
// Usage statistics differential check (spec rust-m9-usage-logs §3): the real Node CLI
// (`zcode.cjs`) and the Rust binary run the same scenarios against one local Anthropic
// model, then their `model_usage`, `turn_usage` and `tool_usage` rows and the
// `v4/telemetry/event` facts are compared after ids and times are normalized.
// Usage: [zcode.cjs] [rust] [--only=<scenario,...>] [--show=<table,...>]
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { prepareHome, startRuntime } from "./zcode-cli-rust-interop-runtime.mjs";
import {
  diffFacts,
  normalizeFacts,
  localTtft,
  PROCESS,
  processFacts,
  scenarioSessions,
  sessionNotifications,
} from "./zcode-cli-rust-telemetry-diff.mjs";
import { MCP_FIXTURE, anthropicModel } from "./zcode-cli-rust-usage-model.mjs";

const args = process.argv.slice(2);
const positional = args.filter((a) => !a.startsWith("--"));
const option = (name) =>
  args
    .find((a) => a.startsWith(`--${name}=`))
    ?.split("=")[1]
    ?.split(",");
const bundle = resolve(positional[0] ?? "apps/zcode-cli/packages/cli/dist/zcode.cjs");
const binary = resolve(positional[1] ?? "apps/zcode-cli-rust/target/debug/zcode-cli-rust");
const only = option("only");
const show = option("show");

const BROWSER_USE = "browser-use@zcode-plugins-official";
const TELEMETRY = ["v4/telemetry/event", "computer-use/operation-event"];
const TERMINAL = ["completedSuccess", "error", "completedInterrupted"];

/** Waits for the next terminal control phase after frame `from`. */
async function terminal(runtime, from, timeoutMs = 30_000) {
  const done = await runtime.until(
    (d) => TERMINAL.includes(d.patch?.control?.phase),
    from,
    timeoutMs,
  );
  return done.patch.control.phase;
}

/** Scenarios whose prompts carry `ttft` (local TTFT, spec rust-m9-usage-logs §7). */
const TTFT = ["plain", "tool", "compact"];

async function send(runtime, id, text, ttft = false) {
  const from = runtime.frames.length;
  const extra = ttft ? { ttft: { version: 1, observationId: randomUUID() } } : {};
  const ack = await runtime.command("sendText", id, { text }, undefined, extra);
  if (ack.status !== "accepted") throw new Error(`${runtime.kind}: ${JSON.stringify(ack)}`);
  return terminal(runtime, from);
}

/** Scenario name → steps on a fresh session; returns the terminal phases. */
const SCENARIOS = {
  plain: async (rt, id) => [await send(rt, id, "plain hello there", true)],
  tool: async (rt, id) => [await send(rt, id, "please use tool now", true)],
  search: async (rt, id) => [await send(rt, id, "please web search this")],
  fail: async (rt, id) => [await send(rt, id, "please fail now")],
  cancel: async (rt, id) => {
    const from = rt.frames.length;
    await rt.command("sendText", id, { text: "please slow stream" });
    await rt.until((d) => d.op === "row.delta" || d.row?.kind === "assistantText", from);
    await rt.command("stop", id, {});
    return [await terminal(rt, from)];
  },
  compact: async (rt, id) => {
    const first = await send(rt, id, "plain before compaction", true);
    const from = rt.frames.length;
    await rt.command("compact", id, {});
    return [first, await terminal(rt, from)];
  },
  agent: async (rt, id) => [await send(rt, id, "please spawn agent")],
  toolError: async (rt, id) => [await send(rt, id, "please read missing file")],
  childTool: async (rt, id) => [await send(rt, id, "please run helper agent")],
  files: async (rt, id) => [
    await send(rt, id, "please create note"),
    await send(rt, id, "please change note"),
  ],
  mcpCrash: async (rt, id) => [await send(rt, id, "please crash mcp")],
  allowed: async (rt, id) => answered(rt, id, /allow|once/i),
  denied: async (rt, id) => answered(rt, id, /deny|reject/i),
};

/** A build-mode Bash call whose permission prompt is answered with `choice`. */
async function answered(rt, id, choice) {
  // 模式切换是 CAS 命令：信封带订阅快照的 revision。
  const snapshot = await rt.snapshot();
  const switched = await rt.command("switchCollaborationMode", id, { mode: "build" }, undefined, {
    baseRevision: snapshot.revision,
  });
  // 模式是项目偏好：前一场景切过后，新会话已在 build 模式（noop）。
  if (!["accepted", "noop"].includes(switched.status))
    throw new Error(`${rt.kind}: ${JSON.stringify(switched)}`);
  const from = rt.frames.length;
  await rt.command("sendText", id, { text: "please write file now" });
  const asked = await rt.until((d) => d.patch?.pendingInteractions?.length > 0, from);
  const interaction = asked.patch.pendingInteractions[0];
  const options = interaction.payload?.options ?? [];
  const option = options.find((o) => choice.test(`${o.id ?? o.optionId}`)) ?? options.at(-1);
  // 权限等待时长进入工具的 performance（permissionWaitMs）。
  await new Promise((r) => setTimeout(r, 30));
  await rt.command("resolveInteraction", id, {
    interactionId: interaction.interactionId ?? interaction.id,
    answer: { optionId: option?.id ?? option?.optionId },
  });
  return [await terminal(rt, from)];
}

/** Differences the spec records (rust-m9-usage-logs §2.6), not counted. */
const KNOWN = [
  // Node 以非流式请求生成标题，provider_metadata 是 AI SDK 的供应商元数据。
  (table, row, field) =>
    table === "model_usage" &&
    row.query_source === "session_title" &&
    field === "provider_metadata_json",
];

async function runRuntime(kind, model) {
  // 工作区路径与 shell 名进入请求前缀：用 realpath 与固定 SHELL，比较不受 macOS /var 链接与登录
  // shell 影响（这两项是单列的环境对齐项，不是统计差异）。
  const root = await realpath(await mkdtemp(join(tmpdir(), `zcode-usage-${kind}-`)));
  try {
    const env = await prepareHome(root, model.baseUrl, {
      api: "anthropic-messages",
      properties: { contextWindow: 256000, supportsNativeWebSearch: true },
      // Node 在空 HOME 里播种并默认启用官方 browser-use 插件（技能、node_repl 工具）；
      // 用量比较在相同的请求前缀上进行，浏览器工具的对齐单独核对。
      config: { plugins: { suppressedBuiltins: [BROWSER_USE] } },
    });
    const cwd = join(root, "ws");
    await mkdir(cwd, { recursive: true });
    await writeFile(join(cwd, "note.txt"), "usage note\n");
    const fixture = join(root, "mcp-fixture.mjs");
    await writeFile(fixture, MCP_FIXTURE);
    // 只有 mcpCrash 场景的会话带 stdio MCP server（进程启动与崩溃遥测，spec §6）。
    const extra = {
      mcpCrash: {
        mcpServers: [{ name: "fixture", command: process.execPath, args: [fixture], env: [] }],
      },
    };
    const runtime = startRuntime(kind, {
      bundle,
      binary,
      cwd,
      env: { ...env, SHELL: "/bin/bash" },
      dataDir: join(root, "data"),
    });
    const sessions = {};
    const phases = {};
    const initial = {};
    const ranges = {};
    for (const [name, run] of Object.entries(SCENARIOS)) {
      if (only && !only.includes(name)) continue;
      const start = runtime.frames.length;
      const created = await runtime.command("createSession", null, {
        workspaceId: cwd,
        ...extra[name],
      });
      const id = created.result?.sessionId;
      if (!id) throw new Error(`${kind}: ${JSON.stringify(created)}`);
      const subscribed = runtime.frames.length;
      await runtime.subscribe(id);
      initial[name] = (await runtime.snapshot(subscribed)).usage;
      sessions[name] = id;
      phases[name] = await run(runtime, id);
      // 标题等旁路请求在轮次后异步完成；留出落库时间。
      await new Promise((r) => setTimeout(r, 300));
      ranges[name] = [start, runtime.frames.length];
    }
    await runtime.close();
    // v4 `usage` 状态补丁按会话收集（spec rust-m9-usage-logs §4）。
    const usage = Object.fromEntries(
      Object.entries(sessions).map(([name, id]) => [
        name,
        [
          { snapshot: initial[name] },
          ...runtime.frames
            .filter((frame) => frame.params?.frame?.topic === `conversation/${id}`)
            .flatMap((frame) => frame.params?.frame?.payload?.deltas ?? [])
            .filter((delta) => delta.patch?.usage || delta.row?.marker?.type === "compact")
            .map((delta) => delta.patch?.usage ?? { compact: delta.row.marker }),
        ],
      ]),
    );
    const frames = runtime.frames
      .map((frame, index) => ({ ...frame, index }))
      .filter((f) => f.method === "v4/telemetry/local-ttft" || f.params?.frame?.ttft !== undefined);
    const telemetry = runtime.frames
      .map((frame, index) => ({ ...frame, index }))
      .filter((frame) => [...TELEMETRY, ...PROCESS].includes(frame.method));
    const db = new DatabaseSync(env.ZCODE_SESSION_DB_PATH, { readOnly: true });
    const read = (table) =>
      db
        .prepare(`select * from ${table} order by started_at, rowid`)
        .all()
        .map((r) => ({ ...r }));
    const tables = {
      model_usage: read("model_usage"),
      turn_usage: read("turn_usage"),
      tool_usage: read("tool_usage"),
    };
    db.close();
    return { root, sessions, phases, tables, telemetry, usage, ranges, frames };
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

const TIME = /(_at|At)$/;
const DURATION = /(duration_ms|_to_first_.*_ms|durationMs|Ms)$/;
const ID_COLUMNS = [
  "session_id",
  "turn_id",
  "trace_id",
  "span_id",
  "assistant_message_id",
  "parent_user_message_id",
  "user_message_id",
  "tool_call_id",
  "logical_request_id",
];

/**
 * Rows of one session group, ids and times replaced by stable placeholders;
 * `root` is the runtime's temporary directory as the Host passed it.
 */
function normalize(rows, names, root) {
  const ids = new Map([...names.entries()].map(([id, name]) => [id, `<${name}>`]));
  ids.set(root, "<root>");
  const counters = {};
  for (const row of rows)
    for (const column of ID_COLUMNS) {
      const value = row[column];
      if (typeof value !== "string" || !value || ids.has(value)) continue;
      counters[column] = (counters[column] ?? 0) + 1;
      ids.set(value, `<${column}#${counters[column]}>`);
    }
  const replaced = (text) => {
    let out = text;
    for (const [id, name] of [...ids.entries()].sort((a, b) => b[0].length - a[0].length))
      out = out.split(id).join(name);
    return out;
  };
  return rows.map((row) =>
    Object.fromEntries(
      Object.entries(row).map(([key, value]) => {
        if (value === null || value === undefined) return [key, null];
        if (TIME.test(key)) return [key, "<time>"];
        if (DURATION.test(key)) return [key, "<ms>"];
        if (key.endsWith("_json") && typeof value === "string") {
          try {
            return [key, JSON.parse(replaced(value))];
          } catch {
            return [key, replaced(value)];
          }
        }
        return [key, typeof value === "string" ? replaced(value) : value];
      }),
    ),
  );
}

/** Rows ordered by what they record (source, tool, status), then time. */
const ordered = (rows) =>
  rows
    .map((row, index) => ({ row, index }))
    .sort(
      (a, b) =>
        String(a.row.query_source ?? a.row.tool_name ?? "").localeCompare(
          String(b.row.query_source ?? b.row.tool_name ?? ""),
        ) || a.index - b.index,
    )
    .map(({ row }) => row);

/**
 * A session's usage patches and compaction markers for comparison: the
 * summary message id is a placeholder, and tool definition characters are a
 * known difference (spec rust-m9-usage-logs §4.3).
 */
const usageTrail = (entries) =>
  JSON.parse(JSON.stringify(entries), (key, value) => {
    if (key === "summaryRef" && typeof value === "string") return "<ref>";
    if (value?.source?.endsWith?.("_tool_schemas")) return { source: value.source };
    return value;
  }).filter(
    // Rust 在 run 结束时重发本轮全部行（§3 已知差异）：与上一条相同的压缩标记不计。
    (entry, index, all) =>
      !entry.compact ||
      JSON.stringify(entry) !==
        JSON.stringify(all.slice(0, index).findLast((previous) => previous.compact)),
  );

/** Differences between the Node and Rust rows of one table (`[]` when equal). */
function diffRows(table, nodeRows, rustRows) {
  const [node, rust] = [ordered(nodeRows), ordered(rustRows)];
  const out = [];
  for (let i = 0; i < Math.max(node.length, rust.length); i++) {
    const n = node[i];
    const r = rust[i];
    if (!n || !r) {
      out.push({ row: i, node: n ?? "(missing)", rust: r ?? "(missing)" });
      continue;
    }
    const fields = {};
    for (const key of new Set([...Object.keys(n), ...Object.keys(r)]))
      if (
        JSON.stringify(n[key]) !== JSON.stringify(r[key]) &&
        !KNOWN.some((known) => known(table, n, key))
      )
        fields[key] = { node: n[key], rust: r[key] };
    if (Object.keys(fields).length)
      out.push({ row: i, label: n.query_source ?? n.tool_name ?? n.status, fields });
  }
  return out;
}

const model = await anthropicModel();
try {
  const node = await runRuntime("node", model);
  const rust = await runRuntime("rust", model);
  let differences = 0;
  for (const name of Object.keys(SCENARIOS)) {
    if (only && !only.includes(name)) continue;
    const phases = { node: node.phases[name], rust: rust.phases[name] };
    const bySession = (result) => {
      const id = result.sessions[name];
      const names = new Map([[id, "session"]]);
      return Object.fromEntries(
        Object.entries(result.tables).map(([table, rows]) => [
          table,
          // 子代理的行属于子会话：按父会话的 turn/trace 归到同一场景。
          normalize(
            rows.filter(
              (row) =>
                row.session_id === id ||
                rows.some((p) => p.session_id === id && p.trace_id && p.trace_id === row.trace_id),
            ),
            names,
            result.root,
          ),
        ]),
      );
    };
    const n = bySession(node);
    const r = bySession(rust);
    const report = { scenario: name, phases };
    const facts = (result, methods = TELEMETRY) =>
      normalizeFacts(
        sessionNotifications(
          result.telemetry,
          methods,
          scenarioSessions(result.telemetry, result.sessions[name]),
        ),
        [result.root],
      );
    const processes = (result) => processFacts(result, name);
    const ttft = (result) => localTtft(result.frames, result.ranges[name]);
    const factDiff = [
      ...diffFacts(facts(node), facts(rust)),
      ...diffFacts(processes(node), processes(rust)),
      ...(TTFT.includes(name) ? diffFacts(ttft(node), ttft(rust)) : []),
    ];
    if (show?.includes("telemetry")) report.telemetry = { node: facts(node), rust: facts(rust) };
    if (show?.includes("ttft")) report.ttft = { node: ttft(node), rust: ttft(rust) };
    if (factDiff.length) report.telemetryDiff = factDiff;
    differences += factDiff.length;
    const trails = { node: usageTrail(node.usage[name]), rust: usageTrail(rust.usage[name]) };
    if (show?.includes("usage")) report.usage = trails;
    if (JSON.stringify(trails.node) !== JSON.stringify(trails.rust)) {
      report.usageTrail = trails;
      differences += 1;
    }
    for (const table of Object.keys(n)) {
      const diff = diffRows(table, n[table], r[table]);
      if (show?.includes(table)) report[`${table}:node`] = n[table];
      if (diff.length) report[table] = diff;
      differences += diff.length;
    }
    console.log(JSON.stringify(report, null, 1));
  }
  console.log(JSON.stringify({ differences }));
  if (differences) process.exitCode = 1;
} finally {
  model.close();
}
