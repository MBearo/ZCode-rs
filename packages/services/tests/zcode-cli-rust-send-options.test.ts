import assert from "node:assert/strict";
import test from "node:test";
import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { end, event, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
const AMBIENT_PREFIX = '<in-app-browser-context source="ambient-ui-state">';

async function send(h: Harness, id: string, payload: Message, commandId?: string) {
  const after = h.messages.length;
  const envelope = h.envelope("sendText", id, payload);
  const ack = await h.command(commandId ? { ...envelope, commandId } : envelope);
  if (ack.status === "accepted") await h.completed(id, after);
  return ack;
}

async function snapshot(h: Harness, id: string, connection: string) {
  const after = h.messages.length;
  const sub = await h.subscribe(`conversation/${id}`, connection);
  const frame = await h.wait(
    (m) =>
      m.params?.subscriptionId === sub.ack.subscriptionId &&
      m.params?.frame?.payload?.kind === "snapshot",
    after,
  );
  return frame.params.frame.payload.snapshot;
}

async function finished(h: Harness, commandId: string) {
  await h.wait((m) =>
    m.params?.frame?.payload?.deltas?.some(
      (d: Message) =>
        d.row?.kind === "turnHeader" &&
        d.row.sourceCommandId === commandId &&
        d.row.state === "completedSuccess",
    ),
  );
}

function toolNames(request: Message): string[] {
  return (request.tools ?? []).map((t: Message) => t.function.name);
}

function userTexts(request: Message): string[] {
  return request.messages
    .filter((m: Message) => m.role === "user")
    .map((m: Message) => String(m.content));
}

function lastUser(request: Message): unknown {
  return request.messages.findLast((m: Message) => m.role === "user").content;
}

test("Rust browser ambient context reaches only the model request", async () => {
  const f = await fixture();
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, {
      text: "open docs",
      browserAmbientContext: { tabCount: 2, currentUrl: "https://docs.example.test/" },
    });
    const rewritten = String(lastUser(f.requests[0]!));
    assert(rewritten.startsWith(AMBIENT_PREFIX));
    assert(rewritten.includes("- The user has the in-app browser open with 2 tabs."));
    assert(rewritten.includes("- Current URL: https://docs.example.test/"));
    assert(rewritten.endsWith("## My request for ZCode:\nopen docs"));
    const rows: Message[] = (await h.rows(id)).rows.filter((r) => r.kind === "userInput");
    assert.equal(rows[0]!.text, "open docs");
    // 同一进程内后续请求仍携带改写；重启后改写消失，历史只保留用户原文（与 Node 一致）。
    await send(h, id, { text: "again" });
    assert(userTexts(f.requests[1]!).some((text) => text.startsWith(AMBIENT_PREFIX)));
    await h.close();
    const resumed = f.start();
    await resumed.subscribe(`conversation/${id}`);
    await send(resumed, id, { text: "after restart" });
    const history = userTexts(f.requests[2]!);
    assert(!history.some((text) => text.includes(AMBIENT_PREFIX)));
    assert(history.includes("open docs"));
    assert.deepEqual(resumed.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust turn tool disallowlists hide tools for the turn, queued promotion and automation input", async () => {
  const gate = Promise.withResolvers<void>();
  const f = await fixture({
    async respond(request, res, attempt) {
      if (attempt === 2) await gate.promise;
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      event(res, { content: `answer ${attempt}` });
      end(res, "stop");
    },
  });
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, { text: "plain" });
    const all = toolNames(f.requests[0]!);
    assert(all.includes("Bash") && all.includes("Read"));
    const busy = h.envelope("sendText", id, { text: "busy" });
    await h.command(busy);
    const queued = h.envelope("sendText", id, {
      text: "queued",
      requestedDelivery: "queue",
      toolDisallowlist: ["Read"],
    });
    await h.command(queued);
    const state = await snapshot(h, id, "inspect");
    const item = state.queue.items.find((q: Message) => q.sourceCommandId === queued.commandId);
    assert.deepEqual(item.toolDisallowlist, ["Read"]);
    gate.resolve();
    await finished(h, queued.commandId);
    const promoted = f.requests.find((r) => lastUser(r) === "queued")!;
    assert(!toolNames(promoted).includes("Read"));
    assert(toolNames(promoted).includes("Bash"));
    await send(h, id, { text: "explicit", toolDisallowlist: ["Bash"] }, "automation-nightly:run:1");
    const automation = f.requests.at(-1)!;
    assert(!toolNames(automation).includes("Bash"));
    assert(toolNames(automation).includes("Read"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust modelExecution runs one turn on frozen credentials without persisting them", async () => {
  const gate = Promise.withResolvers<void>();
  const f = await fixture({
    async respond(request, res, attempt) {
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      const last = request.messages.at(-1);
      if (lastUser(request) === "hold") await gate.promise;
      if (lastUser(request) === "delegate" && last.role !== "tool") {
        event(res, {
          tool_calls: [
            {
              index: 0,
              id: "bg",
              type: "function",
              function: {
                name: "Agent",
                arguments: JSON.stringify({
                  description: "later",
                  prompt: "background child",
                  run_in_background: true,
                }),
              },
            },
            {
              index: 1,
              id: "fg",
              type: "function",
              function: {
                name: "Agent",
                arguments: JSON.stringify({ description: "now", prompt: "foreground child" }),
              },
            },
          ],
        });
        end(res, "tool_calls");
        return;
      }
      event(res, { content: `answer ${attempt}` });
      end(res, "stop");
    },
  });
  const execution = {
    selectionScope: "execution",
    requestAuth: { apiKey: "exec-secret-key", headers: { "X-Exec-Ticket": "ticket-1" } },
    subagents: { foregroundModel: "submission", background: "deny" },
    memoryExtraction: "skip",
  };
  const modelSelection = {
    providerId: "fixture",
    modelId: "core-model",
    options: { reasoningLevel: "none" },
  };
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "hold" }));
    const rejected = await h.command(
      h.envelope("sendText", id, { text: "busy", modelSelection, modelExecution: execution }),
    );
    assert.equal(rejected.status, "failed");
    assert.equal(rejected.reasonCode, "activePrompt");
    assert.match(String(rejected.message), /turn_not_steerable/);
    gate.resolve();
    await h.completed(id, after);

    await send(h, id, { text: "delegate", modelSelection, modelExecution: execution });
    const parent = f.requests.findIndex((r) => lastUser(r) === "delegate");
    assert.equal(f.requestHeaders[parent]!.authorization, "Bearer exec-secret-key");
    assert.equal(f.requestHeaders[parent]!["x-exec-ticket"], "ticket-1");
    const child = f.requests.findIndex((r) => lastUser(r) === "foreground child");
    assert(child > 0, "foreground child ran");
    assert.equal(f.requestHeaders[child]!.authorization, "Bearer exec-secret-key");
    assert(!f.requests.some((r) => lastUser(r) === "background child"));
    const results = f.requests
      .flatMap((r) => r.messages)
      .filter((m: Message) => m.role === "tool" && m.tool_call_id === "bg");
    assert.match(
      String(results.at(-1)?.content),
      /Idle-time tasks do not support background agents\. Run this agent in the foreground\./,
    );

    await send(h, id, { text: "normal" });
    const normal = f.requests.findIndex((r) => lastUser(r) === "normal");
    assert.notEqual(f.requestHeaders[normal]!.authorization, "Bearer exec-secret-key");
    assert.equal(f.requestHeaders[normal]!["x-exec-ticket"], undefined);
    await h.close();
    for (const name of await readdir(f.dataDir)) {
      if (!name.includes("sqlite")) continue;
      const bytes = await readFile(join(f.dataDir, name));
      assert(!bytes.includes("exec-secret-key"), name);
      assert(!bytes.includes("ticket-1"), name);
    }
  } finally {
    await f.close();
  }
});
