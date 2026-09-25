import assert from "node:assert/strict";
import test from "node:test";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ServerResponse } from "node:http";
import { zcodeComputerUseOperationEventSchema } from "@zcode/shared";
import { conversationTelemetryFactSchema } from "@zcode/shared/zcode-protocol-v4";
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
