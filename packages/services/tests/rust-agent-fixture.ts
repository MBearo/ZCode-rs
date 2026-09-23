import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createServer, type ServerResponse } from "node:http";
import { mkdtemp, mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { once } from "node:events";
import { randomUUID } from "node:crypto";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { ZCodeProtocolClient } from "../src/zcode-agent/zcodeProtocolClient.js";
import { ZCodeStdioTransport } from "../src/zcode-agent/zcodeStdioTransport.js";
import { zcodeProtocolMessageSchema } from "@zcode/shared";
import {
  commandAckSchema,
  conversationTopicWireFrameSchema,
  sessionsIndexTopicWireFrameSchema,
  workspaceConfigTopicWireFrameSchema,
  v4ConversationSubscribeResultSchema,
  v4ConversationRowsRangeResultSchema,
} from "@zcode/shared/zcode-protocol-v4";

export const binary = resolve(
  `apps/zcode-rust/target/debug/zcode-rust${process.platform === "win32" ? ".exe" : ""}`,
);
export async function waitForFile(path: string): Promise<string> {
  const started = Date.now();
  while (true) {
    try {
      const content = await readFile(path, "utf8");
      if (content.trim()) return content;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    assert.ok(Date.now() - started < 3000, `File was not produced: ${path}`);
    await delay(10);
  }
}
type Message = Record<string, any>;

export async function fixture(
  options: {
    binary?: string;
    respond?: (request: Message, response: ServerResponse, attempt: number) => void | Promise<void>;
    config?: Message;
    env?: Record<string, string>;
    registry?: boolean;
    legacy?: boolean;
    surface?: "desktop" | "terminal";
  } = {},
) {
  const root = await mkdtemp(join(tmpdir(), "zcode-rust-test-"));
  const cwd = join(root, "workspace");
  const dataDir = join(root, "data");
  const config = join(root, "model.json");
  await mkdir(cwd);
  const requests: Message[] = [];
  const requestBodies: string[] = [];
  const connectionPorts: number[] = [];
  const requestPaths: string[] = [];
  const requestHeaders: Record<string, string | string[] | undefined>[] = [];
  const server = createServer(async (req, res) => {
    // HTTP chunk 可在多字节字符中间切开；逐 Buffer 隐式转字符串会伪造 U+FFFD 和字节超限。
    req.setEncoding("utf8");
    let body = "";
    for await (const chunk of req) body += chunk;
    const request = JSON.parse(body);
    requests.push(request);
    requestPaths.push(req.url ?? "");
    requestHeaders.push(req.headers);
    requestBodies.push(body);
    connectionPorts.push(req.socket.remotePort ?? 0);
    if (options.respond) {
      await options.respond(request, res, requests.length);
      return;
    }
    const text = request.messages.findLast((m: Message) => m.role === "user")?.content ?? "";
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    const last = request.messages.at(-1);
    if (text === "large") {
      event(res, { content: "x".repeat(160_000) });
      end(res, "stop");
      return;
    }
    if (text === "slow") {
      await delay(2000);
      if (res.destroyed) return;
    }
    if ((text === "write" || text === "deny") && last.role !== "tool") {
      event(res, {
        tool_calls: [
          {
            index: 0,
            id: "call-write",
            type: "function",
            function: { name: "Write", arguments: '{"file_path":"result.txt",' },
          },
        ],
      });
      event(res, {
        tool_calls: [{ index: 0, function: { arguments: '"content":"written by Rust"}' } }],
      });
      end(res, "tool_calls");
      return;
    }
    if ((text === "shell" || text === "slow-shell") && last.role !== "tool") {
      event(res, {
        tool_calls: [
          {
            index: 0,
            id: "call-shell",
            type: "function",
            function: {
              name: "Bash",
              arguments: JSON.stringify({
                command:
                  text === "slow-shell"
                    ? "echo $$ > shell.pid; sleep 30"
                    : process.platform === "win32"
                      ? "echo core-shell"
                      : "printf core-shell",
              }),
            },
          },
        ],
      });
      end(res, "tool_calls");
      return;
    }
    // 刻意在中文 UTF-8 字符中间拆 TCP chunk，验证 parser 不能按每个 chunk 解码。
    const bytes = Buffer.from(
      `data: ${JSON.stringify({ choices: [{ delta: { content: "你好" } }] })}\r\n\r\n`,
    );
    const split = bytes.indexOf(Buffer.from("你好")) + 1;
    res.write(bytes.subarray(0, split));
    await delay(10);
    res.write(bytes.subarray(split));
    await delay(10);
    event(res, { content: " Rust" });
    end(res, "stop");
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing fixture address");
  await writeFile(
    config,
    JSON.stringify({
      providerId: "fixture",
      modelId: "core-model",
      reasoningLevel: "none",
      reasoningParameters: { reasoning_effort: "none" },
      baseUrl: `http://127.0.0.1:${address.port}/v1`,
      requestTimeoutSeconds: 5,
      ...options.config,
    }),
  );
  const children: Harness[] = [];
  const start = (identity?: string) => {
    const child = spawn(
      options.binary ?? binary,
      [
        "app-server",
        "--stdio",
        "--surface",
        options.surface ?? "terminal",
        "--cwd",
        cwd,
        "--data-dir",
        dataDir,
        ...(options.registry ? [] : ["--config", config]),
        ...(options.legacy ? ["--import-ts-db", join(root, "ts.sqlite")] : []),
      ],
      {
        stdio: ["pipe", "pipe", "pipe"],
        env: {
          ...process.env,
          HOME: root,
          USERPROFILE: root,
          ZCODE_SESSION_DB_PATH: join(root, "ts.sqlite"),
          ...options.env,
          ...(options.registry
            ? {
                ZCODE_BUILTIN_PROVIDER_CONFIG_FILE: join(root, "builtin.json"),
                ZCODE_PERSONAL_PROVIDER_CONFIG_FILE: join(root, "personal.json"),
              }
            : {}),
          ZCODE_WORKSPACE_IDENTITY: identity ?? "",
        },
      },
    );
    const harness = new Harness(child, identity ?? cwd);
    children.push(harness);
    return harness;
  };
  return {
    root,
    cwd,
    dataDir,
    config,
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    requests,
    requestBodies,
    connectionPorts,
    requestPaths,
    requestHeaders,
    start,
    async close() {
      const exits = await Promise.allSettled(children.map((child) => child.close()));
      server.closeAllConnections();
      await new Promise<void>((r) => server.close(() => r()));
      await rm(root, { recursive: true, force: true });
      const failure = exits.find((result) => result.status === "rejected");
      if (failure?.status === "rejected") throw failure.reason;
    },
  };
}
export function event(res: ServerResponse, delta: Message) {
  res.write(`data: ${JSON.stringify({ choices: [{ delta }] })}\n\n`);
}
export function end(res: ServerResponse, reason: string) {
  res.end(
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: reason }], usage: { prompt_tokens: 10, completion_tokens: 4 } })}\n\ndata: [DONE]\n\n`,
  );
}

export class Harness {
  readonly transport: ZCodeStdioTransport;
  readonly client: ZCodeProtocolClient;
  readonly messages: Message[] = [];
  readonly schemaErrors: string[] = [];
  private buffer = "";
  private waiters = new Set<() => void>();
  private closed = false;
  stderr = "";
  readonly exited: Promise<unknown>;
  constructor(
    readonly child: ChildProcessWithoutNullStreams,
    readonly workspace: string,
  ) {
    this.exited = once(child, "close");
    child.stderr.setEncoding("utf8").on("data", (data: string) => {
      this.stderr += data;
    });
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (data: string) => {
      this.buffer += data;
      let newline: number;
      while ((newline = this.buffer.indexOf("\n")) >= 0) {
        const line = this.buffer.slice(0, newline);
        this.buffer = this.buffer.slice(newline + 1);
        try {
          const message = JSON.parse(line);
          zcodeProtocolMessageSchema.parse(message);
          if (message.method === "v4/conversation/frame") {
            const wire = message.params;
            const schema = wire.topic.startsWith("conversation/")
              ? conversationTopicWireFrameSchema
              : wire.topic.startsWith("sessions-index/")
                ? sessionsIndexTopicWireFrameSchema
                : workspaceConfigTopicWireFrameSchema;
            schema.parse(wire);
          }
          this.messages.push(message);
        } catch (error) {
          this.schemaErrors.push(String(error));
        }
      }
      for (const wake of this.waiters) wake();
    });
    this.transport = new ZCodeStdioTransport(child);
    this.client = new ZCodeProtocolClient(this.transport, {
      requireStorageStartup: true,
      requestTimeoutMs: 5000,
    });
  }
  envelope(type: string, sessionId: string | null, payload: Message = {}) {
    return {
      commandId: randomUUID(),
      clientId: "fixture-client",
      sessionId,
      type,
      payload,
      issuedAt: Date.now(),
    };
  }
  command(command: Message) {
    return this.client.request("v4/command", command, commandAckSchema);
  }
  async create(text?: string) {
    const ack = await this.command(
      this.envelope("createSession", null, {
        workspaceId: this.workspace,
        ...(text ? { firstInput: { text } } : {}),
      }),
    );
    const id = (ack.result as { sessionId: string }).sessionId;
    return id;
  }
  subscribe(topic: string, connectionId = "fixture-desktop", clientMode = "desktop-continuous") {
    return this.client.request(
      "v4/conversation/subscribe",
      { topic, connectionId, clientMode },
      v4ConversationSubscribeResultSchema,
    );
  }
  rows(sessionId: string) {
    return this.client.request(
      "v4/conversation/rowsRange",
      { sessionId, limit: 200 },
      v4ConversationRowsRangeResultSchema,
    );
  }
  async wait(predicate: (message: Message) => boolean, after = 0): Promise<Message> {
    const find = () => this.messages.slice(after).find(predicate);
    const existing = find();
    if (existing) return existing;
    return new Promise((resolveWait, reject) => {
      const wake = () => {
        const message = find();
        if (message) {
          cleanup();
          resolveWait(message);
        }
      };
      const timer = setTimeout(() => {
        cleanup();
        reject(new Error(`Timed out; schema errors: ${this.schemaErrors.join("\n")}`));
      }, 8000);
      const cleanup = () => {
        clearTimeout(timer);
        this.waiters.delete(wake);
      };
      this.waiters.add(wake);
      wake();
    });
  }
  async completed(sessionId: string, after = 0) {
    return this.wait(
      (m) =>
        m.params?.topic === `conversation/${sessionId}` &&
        m.params.frame?.payload?.deltas?.some(
          (d: Message) => d.patch?.control?.phase === "completedSuccess",
        ),
      after,
    );
  }
  async permission(sessionId: string) {
    const message = await this.wait(
      (m) =>
        m.params?.topic === `conversation/${sessionId}` &&
        m.params.frame?.payload?.deltas?.some((d: Message) => d.patch?.pendingInteractions?.length),
    );
    return message.params.frame.payload.deltas.find(
      (d: Message) => d.patch?.pendingInteractions?.length,
    ).patch.pendingInteractions[0];
  }
  async close(expectedExit = 0) {
    if (this.closed) return;
    this.closed = true;
    if (this.child.exitCode === null) this.child.stdin.end();
    const timer = setTimeout(() => this.child.kill("SIGKILL"), 4000);
    const status = await this.exited;
    clearTimeout(timer);
    this.client.dispose();
    assert.deepEqual(
      status,
      [expectedExit, null],
      `Rust process failed to exit cleanly: ${this.stderr}`,
    );
  }
}
