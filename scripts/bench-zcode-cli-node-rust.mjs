// Compare the current Node and Rust App Server processes with one local SSE workload.
import { execFile, spawn } from "node:child_process";
import { cpus, tmpdir } from "node:os";
import { createServer } from "node:http";
import { once } from "node:events";
import { randomUUID, createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";

const run = promisify(execFile);
const nodeBundle = resolve(process.argv[2] ?? "apps/zcode-cli/packages/cli/dist/zcode.cjs");
const rustBinary = resolve(process.argv[3] ?? "apps/zcode-cli-rust/target/release/zcode-cli-rust");
const output = resolve(process.argv[4] ?? ".zcode-runtime/node-rust-bench");
const repetitions = Number(process.argv[5] ?? 5);
if (!Number.isInteger(repetitions) || repetitions < 1) throw new Error("Invalid repetitions");
const turns = 8;
const chunks = 256;
const text = "ZCode benchmark response ";
const sse = `data: ${JSON.stringify({ choices: [{ delta: { content: text } }] })}\n\n`;
const finish =
  'data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":4}}\n\ndata: [DONE]\n\n';
const builtinSource = await readFile(resolve("config/provider/zcode-builtin.json"), "utf8");
await mkdir(output, { recursive: true });

function parseCpuTime(value) {
  const fields = value.trim().split(":").map(Number);
  if (fields.some((field) => !Number.isFinite(field)))
    throw new Error(`Invalid CPU time: ${value}`);
  return fields.reduce((seconds, field) => seconds * 60 + field, 0);
}

async function processStats(pid) {
  const { stdout } = await run("ps", ["-o", "rss=", "-o", "time=", "-p", String(pid)]);
  const match = stdout.trim().match(/^(\d+)\s+(\S+)$/u);
  if (!match) throw new Error(`Unexpected ps output: ${stdout}`);
  return { rssKiB: Number(match[1]), cpuSeconds: parseCpuTime(match[2]) };
}

async function runSample(kind, repetition) {
  const root = await mkdtemp(join(tmpdir(), `zcode-${kind}-bench-`));
  const home = join(root, "home");
  const workspace = join(root, "workspace");
  const storage = join(root, "storage");
  await Promise.all([mkdir(home), mkdir(workspace), mkdir(storage)]);
  let requestCount = 0;
  const server = createServer(async (request, response) => {
    for await (const _ of request) {
      // Consume the full request before returning the deterministic stream.
    }
    requestCount++;
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    for (let index = 0; index < chunks; index++) {
      if (!response.write(sse)) await once(response, "drain");
    }
    response.end(finish);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const baseUrl = `http://127.0.0.1:${server.address().port}/v1`;
  const builtin = JSON.parse(builtinSource);
  builtin.config.providerConfigRules.providerRules = [];
  const builtinFile = join(root, "builtin.json");
  const personalFile = join(root, "personal.json");
  const rustConfig = join(root, "rust-model.json");
  await Promise.all([
    writeFile(builtinFile, JSON.stringify(builtin)),
    writeFile(
      personalFile,
      JSON.stringify({
        schemaVersion: 1,
        config: {
          providerConfigRules: {
            providerRules: [
              {
                providerId: "personal:fixture",
                providerName: "Fixture",
                config: {
                  group: "standard-personal",
                  access: { type: "api-key", apiKey: "fixture-only" },
                  api: { type: "openai-chat-completions", baseUrl },
                  personalModelIds: ["model-a"],
                },
              },
            ],
          },
          modelConfigRules: {
            providerModelRules: [
              {
                providerId: "personal:fixture",
                modelId: "model-a",
                config: {
                  properties: { contextWindow: 256000 },
                  optionSpecs: { reasoningLevel: { values: ["none"], map: "{}" } },
                },
              },
            ],
            manualProviderModelRules: [],
          },
          defaultModelSelection: {
            providerId: "personal:fixture",
            modelId: "model-a",
            options: { reasoningLevel: "none" },
          },
        },
      }),
    ),
    writeFile(
      rustConfig,
      JSON.stringify({
        apiType: "openai-chat-completions",
        providerId: "personal:fixture",
        modelId: "model-a",
        reasoningLevel: "none",
        contextWindow: 256000,
        baseUrl,
      }),
    ),
  ]);
  const command = kind === "node" ? process.execPath : rustBinary;
  const args =
    kind === "node"
      ? [nodeBundle, "app-server", "--stdio", "--surface", "terminal", "--cwd", workspace]
      : [
          "app-server",
          "--stdio",
          "--surface",
          "terminal",
          "--cwd",
          workspace,
          "--data-dir",
          join(root, "rust-data"),
          "--config",
          rustConfig,
        ];
  const child = spawn(command, args, {
    stdio: ["pipe", "pipe", "pipe"],
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      XDG_CONFIG_HOME: join(home, ".config"),
      ZCODE_STORAGE_DIR: storage,
      ZCODE_SESSION_DB_PATH: join(root, "node.sqlite"),
      ZCODE_BUILTIN_PROVIDER_CONFIG_FILE: builtinFile,
      ZCODE_PERSONAL_PROVIDER_CONFIG_FILE: personalFile,
      ZCODE_WORKSPACE_IDENTITY: workspace,
    },
  });
  const exited = once(child, "close");
  const started = performance.now();
  let stderr = "";
  let buffer = "";
  let serial = 0;
  const pending = new Map();
  const active = new Map();
  child.stderr.setEncoding("utf8").on("data", (chunk) => {
    stderr = (stderr + chunk).slice(-4000);
  });
  child.stdout.setEncoding("utf8").on("data", (chunk) => {
    buffer += chunk;
    let newline;
    while ((newline = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, newline);
      buffer = buffer.slice(newline + 1);
      let frame;
      try {
        frame = JSON.parse(line);
      } catch (error) {
        rejectAll(error);
        return;
      }
      if (frame.id !== undefined) {
        if (frame.method === "session/requestRuntimePreferences") {
          child.stdin.write(
            `${JSON.stringify({
              id: frame.id,
              result: {
                askUserQuestionAutoResolutionEnabled: true,
                nativeSearchEnhancementsEnabled: true,
                memoryEnabled: false,
              },
            })}\n`,
          );
          continue;
        }
        const waiter = pending.get(frame.id);
        pending.delete(frame.id);
        if (frame.error) waiter?.reject(new Error(frame.error.message));
        else waiter?.resolve(frame.result);
      }
      const turn = active.get(frame.params?.topic);
      if (!turn) continue;
      for (const delta of frame.params?.frame?.payload?.deltas ?? []) {
        if (delta.patch?.control?.phase === "completedSuccess") turn.resolve();
        if (delta.patch?.control?.phase === "error")
          turn.reject(new Error(JSON.stringify(delta.patch.control.lastError)));
      }
    }
  });
  function rejectAll(error) {
    for (const waiter of pending.values()) waiter.reject(error);
    for (const turn of active.values()) turn.reject(error);
    pending.clear();
    active.clear();
  }
  child.once("close", (code, signal) =>
    rejectAll(new Error(`${kind} exited ${code}/${signal}: ${stderr}`)),
  );
  function rpc(method, params = {}) {
    const id = ++serial;
    return new Promise((resolveRpc, reject) => {
      pending.set(id, { resolve: resolveRpc, reject });
      child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
    });
  }
  function commandRpc(type, sessionId, payload) {
    return rpc("v4/command", {
      commandId: randomUUID(),
      clientId: "benchmark",
      sessionId,
      type,
      payload,
      issuedAt: Date.now(),
    });
  }
  const watchdog = setTimeout(() => child.kill("SIGKILL"), 60_000);
  let sampling = false;
  let sampler;
  let measurement;
  let exitError;
  try {
    await rpc("runtime/capabilities");
    const startupMs = performance.now() - started;
    const idle = await processStats(child.pid);
    const created = await commandRpc("createSession", null, { workspaceId: workspace });
    if (!created?.result?.sessionId) {
      throw new Error(`createSession response: ${JSON.stringify(created)}`);
    }
    const sessionId = created.result.sessionId;
    await rpc("v4/conversation/subscribe", {
      topic: `conversation/${sessionId}`,
      connectionId: "benchmark",
      clientMode: "desktop-continuous",
    });
    const rssSamples = [];
    sampling = true;
    sampler = (async () => {
      while (sampling) {
        try {
          rssSamples.push((await processStats(child.pid)).rssKiB);
        } catch {
          /* The final sample is taken after the workload. */
        }
        await delay(25);
      }
    })();
    const before = await processStats(child.pid);
    const workloadStart = performance.now();
    for (let turn = 0; turn < turns; turn++) {
      const done = new Promise((resolveTurn, reject) => {
        active.set(`conversation/${sessionId}`, { resolve: resolveTurn, reject });
      });
      await commandRpc("sendText", sessionId, { text: `turn ${turn}` });
      await done;
      active.delete(`conversation/${sessionId}`);
    }
    const wallSeconds = (performance.now() - workloadStart) / 1000;
    const after = await processStats(child.pid);
    sampling = false;
    await sampler;
    if (requestCount !== turns)
      throw new Error(`Expected ${turns} model requests, got ${requestCount}`);
    const cpuSeconds = after.cpuSeconds - before.cpuSeconds;
    measurement = {
      kind,
      repetition,
      platform: process.platform,
      arch: process.arch,
      cpuModel: cpus()[0]?.model,
      nodeVersion: process.version,
      binary: kind === "node" ? nodeBundle : rustBinary,
      fixture: {
        turns,
        chunksPerTurn: chunks,
        textBytesPerChunk: Buffer.byteLength(text),
        contextWindow: 256000,
      },
      startupMs,
      idleRssKiB: idle.rssKiB,
      sampledPeakRssKiB: Math.max(before.rssKiB, after.rssKiB, ...rssSamples),
      rssSampleCount: rssSamples.length,
      samplingIntervalMs: 25,
      wallSeconds,
      cpuSeconds,
      averageCpuPercent: (cpuSeconds / wallSeconds) * 100,
      requestCount,
    };
  } finally {
    sampling = false;
    await sampler?.catch(() => {});
    child.stdin.end();
    const [code, signal] = await exited;
    clearTimeout(watchdog);
    server.closeAllConnections();
    await new Promise((done) => server.close(done));
    await rm(root, { recursive: true, force: true });
    if (code !== 0 || signal) exitError = new Error(`${kind} exited ${code}/${signal}: ${stderr}`);
  }
  if (exitError) throw exitError;
  return measurement;
}

const results = [];
for (let repetition = 1; repetition <= repetitions; repetition++) {
  const order = repetition % 2 ? ["node", "rust"] : ["rust", "node"];
  for (const kind of order) {
    const sample = await runSample(kind, repetition);
    results.push(sample);
    await writeFile(
      join(output, `${kind}-${repetition}.json`),
      JSON.stringify(sample, null, 2) + "\n",
    );
    console.log(
      `${kind} ${repetition}/${repetitions}: RSS ${(sample.sampledPeakRssKiB / 1024).toFixed(1)} MiB, CPU ${sample.cpuSeconds.toFixed(2)} s`,
    );
  }
}
function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}
const summary = Object.fromEntries(
  ["node", "rust"].map((kind) => {
    const samples = results.filter((sample) => sample.kind === kind);
    return [
      kind,
      Object.fromEntries(
        [
          "startupMs",
          "idleRssKiB",
          "sampledPeakRssKiB",
          "wallSeconds",
          "cpuSeconds",
          "averageCpuPercent",
        ].map((key) => [key, median(samples.map((sample) => sample[key]))]),
      ),
    ];
  }),
);
const metadata = {
  sourceCommit: (await run("git", ["rev-parse", "HEAD"])).stdout.trim(),
  nodeBundleSha256: createHash("sha256")
    .update(await readFile(nodeBundle))
    .digest("hex"),
  rustBinarySha256: createHash("sha256")
    .update(await readFile(rustBinary))
    .digest("hex"),
  repetitions,
  platform: process.platform,
  arch: process.arch,
  cpuModel: cpus()[0]?.model,
};
await writeFile(
  join(output, "summary.json"),
  JSON.stringify({ metadata, summary }, null, 2) + "\n",
);
console.log(JSON.stringify({ metadata, summary }, null, 2));
