import assert from "node:assert/strict";
import test from "node:test";
import { spawn } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { binary, end, event, fixture } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
type Fixture = Awaited<ReturnType<typeof fixture>>;
// 有效的 1x1 PNG：提示图片按 Node 规则解码，CRC 错误的图片会降级为占位。
const png = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR4nGP4Xw8AAoABfwxp8mEAAAAASUVORK5CYII=",
  "base64",
);

/** Runs `zcode-cli-rust <args>` against the fixture model; `onLine` sees stdout lines. */
function headless(f: Fixture, args: string[], onLine?: (line: string, kill: () => void) => void) {
  const child = spawn(
    binary,
    [...args, "--cwd", f.cwd, "--data-dir", f.dataDir, "--config", f.config],
    {
      env: { ...process.env, HOME: f.root, USERPROFILE: f.root },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  let stdout = "";
  let stderr = "";
  let pending = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
    stdout += chunk;
    pending += chunk;
    let index;
    while ((index = pending.indexOf("\n")) >= 0) {
      const line = pending.slice(0, index);
      pending = pending.slice(index + 1);
      onLine?.(line, () => child.kill("SIGTERM"));
    }
  });
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => (stderr += chunk));
  return new Promise<{ code: number | null; stdout: string; stderr: string }>((resolve) =>
    child.on("close", (code) => resolve({ code, stdout, stderr })),
  );
}
const lines = (stdout: string) =>
  stdout
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line) as Message);
function call(res: Parameters<typeof event>[0], id: string, name: string, input: Message) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, {
    tool_calls: [
      { index: 0, id, type: "function", function: { name, arguments: JSON.stringify(input) } },
    ],
  });
  end(res, "tool_calls");
}
function say(res: Parameters<typeof event>[0], content: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content });
  end(res, "stop");
}

test("Rust -p prints the answer as text, json and stream-json like Node", async () => {
  const f = await fixture();
  try {
    const text = await headless(f, ["-p", "hello"]);
    assert.deepEqual(text, { code: 0, stdout: "你好 Rust\n", stderr: "" });

    const json = await headless(f, ["-p", "hello", "--json"]);
    assert.equal(json.code, 0);
    assert.ok(json.stdout.startsWith('{\n  "sessionId"'));
    const summary = JSON.parse(json.stdout);
    assert.deepEqual(Object.keys(summary), [
      "sessionId",
      "traceId",
      "turnId",
      "response",
      "usage",
      "eventCount",
      "projection",
    ]);
    assert.equal(summary.response, "你好 Rust");
    assert.deepEqual(Object.keys(summary.usage), [
      "source",
      "modelRequestCount",
      "inputTokens",
      "outputTokens",
      "totalTokens",
      "cacheReadTokens",
      "cacheWriteTokens",
      "reasoningTokens",
      "webFetchRequests",
      "webSearchRequests",
    ]);
    assert.deepEqual(Object.keys(summary.projection), [
      "status",
      "turnCount",
      "totalTokenCount",
      "contextUsed",
      "contextWindow",
    ]);
    assert.equal(summary.projection.turnCount, 1);

    const stream = await headless(f, ["-p", "hello", "--output-format", "stream-json"]);
    assert.equal(stream.code, 0);
    const events = lines(stream.stdout);
    const result = events.at(-1)!;
    assert.equal(result.type, "result");
    assert.deepEqual(Object.keys(result).slice(0, 4), ["type", "sessionId", "traceId", "turnId"]);
    assert.equal(result.response, "你好 Rust");
    const body = events.slice(0, -1);
    assert.ok(body.every((e) => e.deliveryKind === undefined && e.sessionId === result.sessionId));
    assert.deepEqual(
      body.map((e) => e.seq),
      body.map((_, i) => i + 1),
    );
    assert.equal(body.find((e) => e.type === "turn.started")?.payload.input, "hello");
    assert.equal(body.at(-1)!.type, "turn.completed");
    assert.equal(body.at(-1)!.turnId, result.turnId);
  } finally {
    await f.close();
  }
});

test("Rust -p prints only the last step and denies every permission ask", async () => {
  const f = await fixture({
    respond(req, res) {
      const last = req.messages.at(-1);
      const user = req.messages.findLast((m: Message) => m.role === "user")?.content;
      if (last.role === "tool") return say(res, "Hello world");
      if (user === "ask") {
        return call(res, "call-ask", "AskUserQuestion", {
          questions: [
            {
              question: "Which?",
              header: "Pick",
              options: [
                { label: "A", description: "a" },
                { label: "B", description: "b" },
              ],
              multiSelect: false,
            },
          ],
        });
      }
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: "Step one." });
      event(res, {
        tool_calls: [
          {
            index: 0,
            id: "call-write",
            type: "function",
            function: {
              name: "Write",
              arguments: '{"file_path":"result.txt","content":"written"}',
            },
          },
        ],
      });
      end(res, "tool_calls");
    },
  });
  try {
    const yolo = await headless(f, ["-p", "write"]);
    assert.deepEqual(yolo, { code: 0, stdout: "Hello world\n", stderr: "" });
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "written");

    await writeFile(join(f.cwd, "result.txt"), "before");
    const build = await headless(f, [
      "-p",
      "write",
      "--mode",
      "build",
      "--output-format",
      "stream-json",
    ]);
    assert.equal(build.code, 0);
    const events = lines(build.stdout);
    const requested = events.find((e) => e.type === "permission.requested");
    assert.equal(requested?.payload.toolName, "Write");
    const resolved = events.find((e) => e.type === "permission.resolved");
    assert.deepEqual(
      [resolved?.payload.decision, resolved?.payload.reason],
      ["deny", "No permission client configured for Write"],
    );
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "before");
    const denied = f.requests.at(-1)!.messages.at(-1);
    assert.deepEqual(
      [denied.role, denied.content],
      ["tool", "No permission client configured for Write"],
    );

    const asked = await headless(f, ["-p", "ask"]);
    assert.deepEqual(asked, { code: 0, stdout: "Hello world\n", stderr: "" });
    assert.equal(
      f.requests.at(-1)!.messages.at(-1).content,
      "No permission client configured for AskUserQuestion",
    );
  } finally {
    await f.close();
  }
});

test("Rust -p attaches files, continues sessions and reports failures", async () => {
  let fail = false;
  const f = await fixture({
    config: {
      formatProperties: {
        inputFormat: { supportsText: true, supportsImage: true },
        outputFormat: { supportsText: true },
      },
    },
    respond(_req, res) {
      if (fail) {
        res.writeHead(401, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { message: "bad key" } }));
        return;
      }
      say(res, "ok");
    },
  });
  try {
    await writeFile(join(f.cwd, "dot.png"), png);
    const attached = await headless(f, [
      "-p",
      "look",
      "--attach",
      "dot.png",
      "--attach",
      "missing.txt",
    ]);
    assert.equal(attached.code, 0);
    const content = JSON.stringify(f.requests.at(-1)!.messages.at(-1).content);
    assert.ok(content.includes("data:image/png;base64,"));
    assert.ok(!content.includes("missing.txt"));

    const first = JSON.parse((await headless(f, ["-p", "first", "--json"])).stdout);
    const second = JSON.parse((await headless(f, ["-p", "second", "-c", "--json"])).stdout);
    assert.equal(second.sessionId, first.sessionId);
    assert.ok(JSON.stringify(f.requests.at(-1)!.messages).includes("first"));

    fail = true;
    const failed = await headless(f, ["-p", "hello", "--verbose"]);
    assert.equal(failed.code, 1);
    assert.equal(failed.stdout, "");
    assert.match(
      failed.stderr,
      /^Error: Turn execution failed \(traceId: [^)]+\)\nCause: Provider authentication failed\.\n$/,
    );
  } finally {
    await f.close();
  }
});

test("Rust -p rejects bad arguments like Node and exits 143 on SIGTERM", async () => {
  const f = await fixture();
  try {
    const empty = await headless(f, ["-p", "  "]);
    assert.deepEqual(empty, { code: 1, stdout: "", stderr: "--prompt requires non-empty text.\n" });
    const mode = await headless(f, ["-p", "x", "--mode", "auto"]);
    assert.equal(
      mode.stderr,
      "Unsupported --mode value: auto. Supported modes: build, edit, plan, yolo.\n",
    );
    const unknown = await headless(f, ["-p", "x", "--bogus"]);
    assert.equal(unknown.code, 1);
    assert.ok(unknown.stderr.startsWith("Unknown option '--bogus'."));
    assert.ok(unknown.stderr.includes("\n\nzcode-cli-rust "));
    const missing = await headless(f, ["-p", "x", "-c"]);
    assert.equal(missing.code, 1);
    assert.equal(missing.stderr, `Error: No resumable session found for ${f.cwd}\n`);
    assert.equal(f.requests.length, 0);

    const killed = await headless(
      f,
      ["-p", "slow", "--output-format", "stream-json"],
      (line, kill) => {
        if (JSON.parse(line).type === "turn.started") kill();
      },
    );
    assert.equal(killed.code, 143);
    const last = lines(killed.stdout).at(-1)!;
    assert.deepEqual([last.type, last.payload.resultType], ["turn.completed", "cancelled"]);
    assert.match(killed.stderr, /^Error: Turn was cancelled\. \(traceId: [^)]+\)\n$/);
  } finally {
    await f.close();
  }
});
