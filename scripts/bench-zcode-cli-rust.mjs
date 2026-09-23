// Deterministic SSE workload. Compare release binaries with identical arguments.
import { spawn, execFile } from "node:child_process";
import { createServer } from "node:http";
import { once } from "node:events";
import { mkdtemp, mkdir, writeFile, rm, stat, readdir } from "node:fs/promises";
import { tmpdir, cpus } from "node:os";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { setImmediate as tick, setTimeout as delay } from "node:timers/promises";
import { promisify } from "node:util";

const binary = resolve(process.argv[2] ?? "apps/zcode-cli-rust/target/release/zcode-cli-rust");
const turns = Number(process.argv[3] ?? 8);
const sessions = Number(process.argv[4] ?? 1);
const chunks = Number(process.argv[5] ?? 2048);
const contextWindow = process.argv[6] === undefined ? undefined : Number(process.argv[6]);
const apiType = process.argv[7];
if (
  apiType &&
  !["openai-chat-completions", "openai-responses", "anthropic-messages"].includes(apiType)
)
  throw new Error("Unsupported benchmark API protocol");
if (contextWindow !== undefined && (!Number.isSafeInteger(contextWindow) || contextWindow <= 34000))
  throw new Error("Context window must leave a positive input budget");
if (![turns, sessions, chunks].every((n) => Number.isInteger(n) && n > 0))
  throw new Error("Positive turns/sessions/chunks required");
const root = await mkdtemp(join(tmpdir(), "zcode-cli-rust-bench-"));
const workspace = join(root, "workspace");
await mkdir(workspace);
const fixtureHome = join(root, "home");
await mkdir(fixtureHome);
const text = "Rust流式测试 ".repeat(4);
const frame = (value) => `data: ${JSON.stringify(value)}\n\n`;
let begin = "";
let sse = frame({ choices: [{ delta: { content: text } }] });
let end = 'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n';
if (apiType === "openai-responses") {
  const item = {
    type: "message",
    id: "msg",
    role: "assistant",
    status: "completed",
    content: [{ type: "output_text", text: text.repeat(chunks), annotations: [] }],
  };
  begin = frame({
    type: "response.output_item.added",
    output_index: 0,
    item: { ...item, status: "in_progress", content: [] },
  });
  sse = frame({
    type: "response.output_text.delta",
    output_index: 0,
    item_id: "msg",
    content_index: 0,
    delta: text,
  });
  end =
    frame({ type: "response.output_item.done", output_index: 0, item }) +
    frame({ type: "response.completed", response: { status: "completed", output: [item] } });
} else if (apiType === "anthropic-messages") {
  begin =
    frame({ type: "message_start", message: { usage: {} } }) +
    frame({ type: "content_block_start", index: 0, content_block: { type: "text", text: "" } });
  sse = frame({ type: "content_block_delta", index: 0, delta: { type: "text_delta", text } });
  end =
    frame({ type: "content_block_stop", index: 0 }) +
    frame({ type: "message_delta", delta: { stop_reason: "end_turn" } }) +
    frame({ type: "message_stop" });
}
const server = createServer(async (req, res) => {
  for await (const _ of req) {
    /* consume request */
  }
  res.writeHead(200, { "Content-Type": "text/event-stream" });
  if (begin) res.write(begin);
  for (let i = 0; i < chunks; i++) {
    if (res.destroyed) return;
    if (!res.write(sse)) await once(res, "drain");
    if (i % 8 === 0) await tick();
  }
  res.end(end);
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const config = join(root, "config.json");
await writeFile(
  config,
  JSON.stringify({
    ...(contextWindow === undefined ? {} : { contextWindow }),
    ...(apiType === undefined ? {} : { apiType }),
    providerId: "bench",
    modelId: "bench",
    reasoningLevel: "none",
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
  }),
);
const started = performance.now();
const child = spawn(
  binary,
  [
    "app-server",
    "--stdio",
    "--cwd",
    workspace,
    "--data-dir",
    join(root, "data"),
    "--config",
    config,
  ],
  {
    env: {
      ...process.env,
      HOME: fixtureHome,
      USERPROFILE: fixtureHome,
      XDG_CONFIG_HOME: join(fixtureHome, ".config"),
      ZCODE_STORAGE_DIR: join(fixtureHome, ".zcode"),
    },
  },
);
const exit = once(child, "close");
let buffer = "",
  stderr = "",
  frames = 0,
  protocolFrames = 0,
  serial = 0;
const pending = new Map(),
  active = new Map();
child.stderr.setEncoding("utf8").on("data", (s) => {
  stderr += s;
});
child.stdout.setEncoding("utf8").on("data", (s) => {
  buffer += s;
  let end;
  while ((end = buffer.indexOf("\n")) >= 0) {
    const frame = JSON.parse(buffer.slice(0, end));
    buffer = buffer.slice(end + 1);
    frames++;
    if (frame.id !== undefined) {
      const waiter = pending.get(frame.id);
      pending.delete(frame.id);
      if (frame.error) waiter?.reject(new Error(frame.error.message));
      else waiter?.resolve(frame.result);
    }
    if (frame.method === "v4/conversation/frame") protocolFrames++;
    const turn = active.get(frame.params?.topic);
    if (!turn) continue;
    for (const delta of frame.params?.frame?.payload?.deltas ?? []) {
      if (delta.row?.kind === "assistantText" || delta.op === "row.delta")
        turn.firstText ??= performance.now();
      if (["completedSuccess", "error"].includes(delta.patch?.control?.phase))
        turn.complete(delta.patch.control);
    }
  }
});
function rpc(method, params = {}) {
  const id = ++serial;
  return new Promise((resolveRpc, reject) => {
    pending.set(id, { resolve: resolveRpc, reject });
    child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
  });
}
function command(type, sessionId, payload) {
  return rpc("v4/command", {
    commandId: randomUUID(),
    clientId: "bench",
    sessionId,
    type,
    payload,
    issuedAt: Date.now(),
  });
}
child.once("close", (code, signal) => {
  const error = new Error(`Agent exited ${code}/${signal}: ${stderr}`);
  for (const waiter of pending.values()) waiter.reject(error);
  for (const turn of active.values()) turn.reject(error);
});
const run = promisify(execFile);
async function rssKiB() {
  try {
    const { stdout } =
      process.platform === "win32"
        ? await run("powershell.exe", [
            "-NoProfile",
            "-Command",
            `(Get-Process -Id ${child.pid}).WorkingSet64 / 1024`,
          ])
        : await run("ps", ["-o", "rss=", "-p", String(child.pid)]);
    const n = Number(stdout.trim());
    return Number.isFinite(n) ? n : null;
  } catch {
    return null;
  }
}
const watchdog = setTimeout(() => child.kill("SIGKILL"), 120_000);
async function storageSize() {
  const files = await readdir(join(root, "data"));
  return (
    await Promise.all(
      files
        .filter((f) => !f.endsWith(".lock"))
        .map(async (f) => (await stat(join(root, "data", f))).size),
    )
  ).reduce((a, b) => a + b, 0);
}
let measurement;
let exitStatus,
  sampling = false,
  sampler;
try {
  await rpc("runtime/capabilities");
  const startupMs = performance.now() - started;
  const idleRssKiB = await rssKiB();
  const ids = [];
  for (let i = 0; i < sessions; i++) {
    const created = await command("createSession", null, { workspaceId: workspace });
    const id = created.result.sessionId;
    ids.push(id);
    await rpc("v4/conversation/subscribe", {
      topic: `conversation/${id}`,
      connectionId: "bench",
      clientMode: "desktop-continuous",
    });
  }
  const samples = [],
    controlMs = [],
    steadyControlMs = [],
    rssSamples = [];
  sampling = true;
  sampler = (async () => {
    while (sampling) {
      const steady = [...active.values()].some((t) => t.ordinal > 1);
      const at = performance.now();
      await rpc("runtime/capabilities");
      const duration = performance.now() - at;
      controlMs.push(duration);
      if (steady) steadyControlMs.push(duration);
      if (controlMs.length % 5 === 0) {
        const rss = await rssKiB();
        if (rss !== null) rssSamples.push(rss);
      }
      await delay(5);
    }
  })();
  const workloadStart = performance.now();
  await Promise.all(
    ids.map(async (id, session) => {
      for (let i = 0; i < turns; i++) {
        const start = performance.now();
        let turn;
        const done = new Promise((complete, reject) => {
          turn = { complete, reject, ordinal: i + 1 };
          active.set(`conversation/${id}`, turn);
        });
        await command("sendText", id, { text: `turn ${i}` });
        const result = await done;
        if (result.phase !== "completedSuccess") throw new Error(JSON.stringify(result.lastError));
        samples.push({
          session,
          turn: i + 1,
          ttftMs: turn.firstText - start,
          totalMs: performance.now() - start,
        });
        active.delete(`conversation/${id}`);
      }
    }),
  );
  const totalMs = performance.now() - workloadStart;
  sampling = false;
  await sampler;
  const finalRssKiB = await rssKiB();
  const storageBytes = await storageSize();
  controlMs.sort((a, b) => a - b);
  steadyControlMs.sort((a, b) => a - b);
  measurement = {
    binary,
    contextWindow: contextWindow ?? null,
    apiType: apiType ?? "openai-chat-completions",
    responseBytes: Buffer.byteLength(begin + end) + chunks * Buffer.byteLength(sse),
    platform: process.platform,
    arch: process.arch,
    cpu: cpus()[0]?.model,
    turns,
    sessions,
    chunks,
    startupMs,
    totalMs,
    rpcP95Ms: controlMs[Math.floor(controlMs.length * 0.95)],
    idleRssKiB,
    finalRssKiB,
    sampledPeakRssKiB: Math.max(...rssSamples, idleRssKiB ?? 0, finalRssKiB ?? 0),
    rssSampleCount: rssSamples.length,
    frames,
    protocolFrames,
    storageBytes,
    samples,
    steadyRpcP95Ms: steadyControlMs[Math.floor(steadyControlMs.length * 0.95)] ?? null,
    steadyRpcSampleCount: steadyControlMs.length,
  };
} finally {
  sampling = false;
  await sampler?.catch(() => {});
  child.stdin.end();
  exitStatus = await exit;
  clearTimeout(watchdog);
  if (measurement) measurement.durableStorageBytes = await storageSize();
  server.closeAllConnections();
  await new Promise((r) => server.close(r));
  await rm(root, { recursive: true, force: true });
}
if (exitStatus[0] !== 0 || exitStatus[1])
  throw new Error(`Agent exited ${exitStatus[0]}/${exitStatus[1]}: ${stderr}`);

console.log(JSON.stringify(measurement, null, 2));
