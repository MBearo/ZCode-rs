// A Node (`zcode.cjs`) or Rust App Server over stdio for the cross-runtime checks: both
// share one HOME, session database and Provider Registry, so either continues the other's
// sessions (spec rust-m11-node-storage §11).
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { once } from "node:events";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";

const TITLE = "Generate a concise title for this coding session.";

/** The text of a chat message content (string or parts). */
export function contentText(content) {
  if (typeof content === "string") return content;
  return (content ?? []).map((part) => (typeof part?.text === "string" ? part.text : "")).join("");
}

/**
 * A local Chat Completions model: `use tool` asks for `Read note.txt`, `slow` streams one
 * chunk and stalls, `shell` runs a long Bash command; tool results and other prompts get
 * `answer: <prompt>`. Title sidecar requests are answered apart from `requests`.
 */
export async function modelServer() {
  const requests = [];
  const server = createServer(async (request, response) => {
    let body = "";
    for await (const chunk of request) body += chunk;
    const parsed = JSON.parse(body);
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    const send = (delta, finish) => {
      response.write(`data: ${JSON.stringify({ choices: [{ delta }] })}\n\n`);
      if (finish)
        response.end(
          `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: finish }], usage: { prompt_tokens: 10, completion_tokens: 3 } })}\n\ndata: [DONE]\n\n`,
        );
    };
    if (body.includes(TITLE)) return send({ content: '{"title":"Interop"}' }, "stop");
    requests.push(parsed);
    const last = parsed.messages.at(-1);
    const text = contentText(last?.content);
    const call = (name, args) =>
      send(
        {
          tool_calls: [
            {
              index: 0,
              id: `call_${randomUUID().slice(0, 8)}`,
              type: "function",
              function: { name, arguments: JSON.stringify(args) },
            },
          ],
        },
        "tool_calls",
      );
    if (last?.role === "tool") return send({ content: "tool done" }, "stop");
    if (text.includes("use tool")) return call("Read", { file_path: "note.txt" });
    if (text.includes("shell")) return call("Bash", { command: "echo $$ > shell.pid; sleep 30" });
    if (text.includes("slow")) {
      send({ content: "partial " });
      return; // 保持连接直到进程被杀。
    }
    send({ content: `answer: ${text}` }, "stop");
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return {
    requests,
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    close: () => {
      server.closeAllConnections();
      server.close();
    },
  };
}

/**
 * The shared HOME: yolo user config and a Provider Registry with the local model
 * (`api` is the provider API type, `properties` the model properties).
 */
export async function prepareHome(
  root,
  baseUrl,
  { api = "openai-chat-completions", properties = { contextWindow: 256000 }, config = {} } = {},
) {
  const home = join(root, "home");
  await mkdir(join(home, ".zcode", "cli"), { recursive: true });
  await writeFile(
    join(home, ".zcode", "cli", "config.json"),
    JSON.stringify({ permission: { mode: "yolo" }, ...config }),
  );
  const builtin = JSON.parse(await readFile(resolve("config/provider/zcode-builtin.json"), "utf8"));
  builtin.config.providerConfigRules.providerRules = [];
  const personal = {
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
              api: { type: api, baseUrl },
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
              properties,
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
  };
  await writeFile(join(root, "builtin.json"), JSON.stringify(builtin));
  await writeFile(join(root, "personal.json"), JSON.stringify(personal));
  return {
    HOME: home,
    USERPROFILE: home,
    XDG_CONFIG_HOME: join(home, ".config"),
    ZCODE_SESSION_DB_PATH: join(root, "db.sqlite"),
    ZCODE_BUILTIN_PROVIDER_CONFIG_FILE: join(root, "builtin.json"),
    ZCODE_PERSONAL_PROVIDER_CONFIG_FILE: join(root, "personal.json"),
  };
}

/** One App Server process (`kind` is `node` or `rust`). */
export function startRuntime(kind, { bundle, binary, cwd, env, dataDir }) {
  const command = kind === "node" ? process.execPath : binary;
  const common = ["app-server", "--stdio", "--surface", "terminal", "--cwd", cwd];
  const args = kind === "node" ? [bundle, ...common] : [...common, "--data-dir", dataDir];
  const child = spawn(command, args, {
    stdio: ["pipe", "pipe", "pipe"],
    // 与 Host 启动本地工作区一致：identity 即工作区路径。
    env: { ...process.env, ...env, ZCODE_WORKSPACE_IDENTITY: cwd },
  });
  const frames = [];
  const pending = new Map();
  let serial = 0;
  let buffer = "";
  let stderr = "";
  child.stderr.setEncoding("utf8").on("data", (chunk) => (stderr += chunk));
  child.stdout.setEncoding("utf8").on("data", (chunk) => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf("\n")) >= 0) {
      const frame = JSON.parse(buffer.slice(0, end));
      buffer = buffer.slice(end + 1);
      if (frame.method === "session/requestRuntimePreferences" && frame.id !== undefined) {
        const result = {
          askUserQuestionAutoResolutionEnabled: true,
          nativeSearchEnhancementsEnabled: true,
          memoryEnabled: false,
        };
        child.stdin.write(`${JSON.stringify({ id: frame.id, result })}\n`);
        continue;
      }
      if (frame.id !== undefined && pending.has(frame.id) && frame.method === undefined) {
        const waiter = pending.get(frame.id);
        pending.delete(frame.id);
        if (frame.error) waiter.reject(new Error(frame.error.message));
        else waiter.resolve(frame.result);
        continue;
      }
      frames.push(frame);
    }
  });
  const exited = once(child, "close");
  const rpc = (method, params = {}) =>
    new Promise((resolveRpc, reject) => {
      const id = ++serial;
      pending.set(id, { resolve: resolveRpc, reject });
      child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
    });
  const deltas = (from = 0) =>
    frames.slice(from).flatMap((frame) => frame.params?.frame?.payload?.deltas ?? []);
  const runtime = {
    kind,
    child,
    frames,
    rpc,
    get stderr() {
      return stderr;
    },
    /** A v4 command; `extra` adds envelope members such as `baseRevision`. */
    command(type, sessionId, payload = {}, commandId = randomUUID(), extra = {}) {
      return rpc("v4/command", {
        commandId,
        clientId: `interop-${kind}`,
        sessionId,
        type,
        payload,
        issuedAt: Date.now(),
        ...extra,
      });
    },
    subscribe(sessionId) {
      return rpc("v4/conversation/subscribe", {
        topic: `conversation/${sessionId}`,
        connectionId: `interop-${kind}`,
        clientMode: "desktop-continuous",
      });
    },
    /** Waits for a delta after frame `from` matching `test` (default 20 s). */
    async until(test, from = 0, timeoutMs = 20_000) {
      const started = Date.now();
      while (Date.now() - started < timeoutMs) {
        const found = deltas(from).find(test);
        if (found) return found;
        await new Promise((r) => setTimeout(r, 20));
      }
      throw new Error(`${kind}: timed out; stderr: ${stderr.slice(-800)}`);
    },
    /** Waits for the conversation snapshot frame sent after frame `from`. */
    async snapshot(from = 0, timeoutMs = 20_000) {
      const started = Date.now();
      while (Date.now() - started < timeoutMs) {
        const found = frames
          .slice(from)
          .findLast((frame) => frame.params?.frame?.payload?.kind === "snapshot");
        if (found) return found.params.frame.payload.snapshot;
        await new Promise((r) => setTimeout(r, 20));
      }
      throw new Error(`${kind}: no snapshot; stderr: ${stderr.slice(-800)}`);
    },
    /** Sends a prompt and waits for its turn to end; the control patch. */
    async turn(sessionId, text) {
      const from = frames.length;
      const ack = await runtime.command("sendText", sessionId, { text });
      if (ack.status !== "accepted") throw new Error(`${kind}: ${JSON.stringify(ack)}`);
      const done = await runtime.until(
        (d) =>
          ["completedSuccess", "error", "completedInterrupted"].includes(d.patch?.control?.phase),
        from,
      );
      if (done.patch.control.phase !== "completedSuccess")
        throw new Error(`${kind}: ${JSON.stringify(done.patch.control.lastError)}`);
      return ack;
    },
    async close() {
      child.stdin.end();
      const [code] = await exited;
      if (code !== 0) throw new Error(`${kind} exited ${code}: ${stderr.slice(-800)}`);
    },
    async kill() {
      child.kill("SIGKILL");
      await exited;
    },
  };
  return runtime;
}

/** The conversation a model request carried: user/assistant/tool entries as text. */
export function transcript(request) {
  return request.messages
    .filter((m) => m.role !== "system")
    .flatMap((m) => {
      const text = contentText(m.content);
      if (m.role === "user" && text.trimStart().startsWith("<system-reminder>")) return [];
      if (m.role === "tool") return [`tool:${m.tool_call_id ? "result" : ""}`];
      const calls = (m.tool_calls ?? []).map((c) => `call:${c.function?.name}`);
      return [...(text ? [`${m.role}:${text}`] : []), ...calls];
    });
}
