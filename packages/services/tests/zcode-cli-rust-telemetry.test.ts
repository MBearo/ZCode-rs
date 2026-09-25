import assert from "node:assert/strict";
import test from "node:test";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";
import {
  localTtftFactsSchema,
  zcodeComputerUseOperationEventSchema,
  zcodeMcpTelemetryEventSchema,
} from "@zcode/shared";
import {
  commandsQueryResultSchema,
  conversationTelemetryFactSchema,
  conversationTopicFrameSchema,
} from "@zcode/shared/zcode-protocol-v4";
import { end, event, fixture } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

function bashCall(res: ServerResponse) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, {
    tool_calls: [
      {
        index: 0,
        id: "call-bash",
        type: "function",
        function: {
          name: "Bash",
          arguments: JSON.stringify({ command: "echo hi", description: "Echo" }),
        },
      },
    ],
  });
  end(res, "tool_calls");
}

function answer(res: ServerResponse) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content: "done" });
  end(res, "stop");
}

// spec rust-m9-usage-logs §5：实时事件的遥测通过 Host 的 strict schema，且关联到提交命令。
test("Rust live telemetry passes the shared strict schemas", async () => {
  const f = await fixture({
    respond(_req, res) {
      if (f.requests.length === 1) bashCall(res);
      else answer(res);
    },
  });
  try {
    await writeFile(join(f.cwd, "a.txt"), "hello\n");
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    const command = h.envelope("sendText", id, { text: "run it" });
    await h.command(command);
    await h.completed(id, after);
    const sent = (method: string) =>
      h.messages
        .slice(after)
        .filter((m: Message) => m.method === method)
        .map((m: Message) => m.params);
    const facts = sent("v4/telemetry/event").map((fact) =>
      conversationTelemetryFactSchema.parse(fact),
    );
    const operations = sent("computer-use/operation-event").map((operation) =>
      zcodeComputerUseOperationEventSchema.parse(operation),
    );
    const kinds = facts.map((fact) => ("phase" in fact ? `${fact.kind}:${fact.phase}` : fact.kind));
    for (const kind of [
      "turn.started",
      "model.request.status",
      "usage.delta",
      "tool.lifecycle:scheduled",
      "tool.lifecycle:started",
      "tool.lifecycle:completed",
      "turn.terminal",
    ])
      assert.ok(kinds.includes(kind), kinds.join(","));
    for (const fact of facts.filter((f) => f.kind !== "model.request.status"))
      assert.equal(fact.sourceCommandId, command.commandId, fact.kind);
    const done = facts.find((f) => f.kind === "tool.lifecycle" && f.phase === "completed");
    assert.equal(done?.kind === "tool.lifecycle" && done.performance?.commandName, "echo");
    assert.deepEqual(
      operations.map((o) => o.kind),
      ["turn-started", "tool-scheduled", "tool-started", "turn-completed"],
    );
    const seqs = facts.map((f) => f.eventSeq);
    assert.deepEqual(
      seqs,
      [...seqs].sort((a, b) => a - b),
    );
  } finally {
    await f.close();
  }
});

/** A stdio MCP server whose one tool, `crash`, exits the process with code 3. */
const MCP_SERVER = `import { createInterface } from "node:readline";
const send = (message) => process.stdout.write(JSON.stringify(message) + "\\n");
createInterface({ input: process.stdin }).on("line", (line) => {
  const m = JSON.parse(line);
  if (m.method === "initialize")
    send({ jsonrpc: "2.0", id: m.id, result: { protocolVersion: m.params.protocolVersion,
      capabilities: { tools: {} }, serverInfo: { name: "fixture", version: "1.0.0" } } });
  else if (m.method === "tools/list")
    send({ jsonrpc: "2.0", id: m.id, result: { tools: [{ name: "crash",
      description: "Exits the server", inputSchema: { type: "object", properties: {} } }] } });
  else if (m.method === "tools/call") process.exit(3);
  else if (m.id !== undefined) send({ jsonrpc: "2.0", id: m.id, result: {} });
});
`;

// spec rust-m9-usage-logs §6：stdio MCP server 的启动、会话首次快照与崩溃通过 Host 的 strict schema。
test("Rust MCP process telemetry reports a stdio server's start and crash", async () => {
  const f = await fixture({
    respond(_req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      if (f.requests.length === 1) {
        event(res, {
          tool_calls: [
            {
              index: 0,
              id: "call-crash",
              type: "function",
              function: { name: "mcp__fixture__crash", arguments: "{}" },
            },
          ],
        });
        end(res, "tool_calls");
      } else {
        event(res, { content: "done" });
        end(res, "stop");
      }
    },
  });
  try {
    const server = join(f.cwd, "..", "mcp-server.mjs");
    await writeFile(server, MCP_SERVER);
    const h = f.start();
    const ack = await h.command(
      h.envelope("createSession", null, {
        workspaceId: h.workspace,
        mcpServers: [{ name: "fixture", command: process.execPath, args: [server], env: [] }],
      }),
    );
    const id = (ack.result as Message).sessionId as string;
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "crash it" }));
    await h.completed(id, after);
    const events = await h.wait(
      (m) => m.method === "process/mcpTelemetry" && m.params?.kind === "process_crash",
      after,
    );
    assert.ok(events);
    const sent = h.messages
      .slice(after)
      .filter((m: Message) => m.method === "process/mcpTelemetry")
      .map((m: Message) => zcodeMcpTelemetryEventSchema.parse(m.params));
    assert.deepEqual(
      sent.map((e) => e.kind),
      ["process_start", "session_startup", "process_crash"],
    );
    const [start, startup, crash] = sent;
    assert.ok(start?.kind === "process_start" && /^custom:[a-f0-9]{12}$/.test(start.mcpId));
    assert.ok(startup?.kind === "session_startup" && startup.processCount === 1);
    assert.ok(crash?.kind === "process_crash" && crash.exitCode === 3);
    assert.equal(crash.affectedSessionCount, 1);
    assert.equal(crash.mcpInstanceId, start.mcpInstanceId);
    const processes = (await h.client.request("process/childProcesses", {})) as Message;
    assert.deepEqual(processes.processes, []);
  } finally {
    await f.close();
  }
});

// spec rust-m9-usage-logs §7：带 ttft 的输入记录本地首字耗时，checkpoint 与帧上的事实都通过 strict schema。
test("Rust local TTFT records a prompt to its first text", async () => {
  const f = await fixture({ respond: (_req, res) => answer(res) });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const probe = commandsQueryResultSchema.parse(
      await h.client.request("v4/commands/query", {
        commands: [{ sessionId: null, commandId: `ttft-clock-${randomUUID()}` }],
        clock: true,
      }),
    );
    assert.ok(probe.clock && probe.clock.sentAt >= probe.clock.receivedAt);
    const after = h.messages.length;
    const observationId = randomUUID();
    const command = {
      ...h.envelope("sendText", id, { text: "hello" }),
      ttft: { version: 1, observationId },
    };
    await h.command(command);
    await h.completed(id, after);
    const checkpoints = h.messages
      .slice(after)
      .filter((m: Message) => m.method === "v4/telemetry/local-ttft")
      .map((m: Message) => localTtftFactsSchema.parse(m.params));
    assert.ok(checkpoints.length >= 3, JSON.stringify(checkpoints));
    const revisions = checkpoints.map((c) => c.revision);
    assert.deepEqual(
      revisions,
      revisions.map((_, i) => i + 1),
    );
    const last = checkpoints.at(-1)!;
    assert.equal(last.commandId, command.commandId);
    assert.equal(last.instanceId, probe.clock!.instanceId);
    assert.equal(last.sendMode, "idle");
    for (const at of [last.admittedAt, last.executionAt, last.requestAt])
      assert.ok(at! >= last.receivedAt);
    const frames = h.messages
      .slice(after)
      .map((m: Message) => m.params?.frame)
      .filter((frame: Message) => frame?.ttft);
    assert.ok(frames.length > 0);
    const facts = conversationTopicFrameSchema.parse(frames.at(-1)).ttft!;
    assert.equal(facts.observationId, observationId);
    assert.equal(facts.outputKind, "text");
    assert.ok(facts.outputAt! >= facts.requestAt! && facts.productTurnId && facts.cliVersion);
  } finally {
    await f.close();
  }
});
