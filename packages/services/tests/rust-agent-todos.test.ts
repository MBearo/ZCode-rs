import assert from "node:assert/strict";
import test from "node:test";
import {
  TodoWriteInputSchema,
  TodoReadInputSchema,
} from "../../../apps/zcode-cli/packages/contracts/src/tools/todo.js";
import {
  todoReadToolEntry,
  todoWriteToolEntry,
} from "../../../apps/zcode-cli/packages/core/src/tool/handlers/todo.js";
import type { ToolExecutionContext } from "../../../apps/zcode-cli/packages/core/src/tool/types.js";
import { fixture, event, end, type Harness } from "./rust-agent-fixture.js";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";

async function snapshot(
  h: Harness,
  id: string,
  connection = "fixture-desktop",
  mode = "desktop-continuous",
) {
  const before = h.messages.length;
  const sub = await h.subscribe(`conversation/${id}`, connection, mode);
  const m = await h.wait(
    (m) =>
      m.params?.subscriptionId === sub.ack.subscriptionId &&
      m.params?.frame?.payload?.kind === "snapshot",
    before,
  );
  return m.params.frame.payload.snapshot;
}

const todo = (content: string, status = "pending", priority = "medium") => ({
  content,
  status,
  priority,
});
const calls = (values: [string, unknown][]) =>
  values.map(([name, input], index) => ({
    index,
    id: `todo-${index}`,
    type: "function",
    function: { name, arguments: JSON.stringify(input) },
  }));
function todoFixture(values: [string, unknown][]) {
  return fixture({
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      if (!req.tools?.length) {
        event(res, { content: "COMPACT_SUMMARY preserve todo progress" });
        end(res, "stop");
        return;
      }
      if (req.messages.at(-1).role === "user") {
        event(res, {
          tool_calls: calls(req.messages.at(-1).content === "read" ? [["TodoRead", {}]] : values),
        });
        end(res, "tool_calls");
      } else {
        event(res, { content: "done" });
        end(res, "stop");
      }
    },
  });
}

test("Rust Todo follows TS handlers, commits ordered replacement and projects durable App plan", async () => {
  const first = [todo(" 检查文件 ", "in_progress", "high"), todo("测试", "in_progress", "low")];
  const second = [todo("检查文件", "completed", "high")];
  const values: [string, unknown][] = [
    ["TodoRead", {}],
    ["TodoWrite", { todos: first }],
    ["TodoRead", {}],
    ["TodoWrite", { todos: second }],
    ["TodoRead", {}],
  ];
  const f = await todoFixture(values);
  try {
    let todos: unknown[] = [];
    const context = {
      sessionId: "fixture",
      sessionStore: {
        readTodos: async () => structuredClone(todos),
        updateTodos: async (input: { todos: unknown[] }) => {
          todos = structuredClone(input.todos);
        },
      },
    } as unknown as ToolExecutionContext;
    const expected = [];
    for (const [name, input] of values)
      expected.push(
        await (name === "TodoRead" ? todoReadToolEntry : todoWriteToolEntry).handler(
          input,
          context,
        ),
      );
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const mobile = await h.subscribe(`conversation/${id}`, "mobile", "web-remote-replayable");
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id);
    const results = f.requests[1]!.messages.filter((m: any) => m.role === "tool");
    assert.deepEqual(
      results.map((m: any) => JSON.parse(m.content)),
      expected,
    );
    assert.deepEqual(
      results.map((m: any) => m.tool_call_id),
      values.map((_, i) => `todo-${i}`),
    );
    const snap = await snapshot(h, id);
    assert.deepEqual(snap.plan?.items, [
      { id: "检查文件", content: "检查文件", status: "completed" },
    ]);
    assert(
      h.messages.some(
        (m) =>
          m.params?.subscriptionId === mobile.ack.subscriptionId &&
          m.params?.frame?.payload?.deltas?.some(
            (d: any) => d.patch?.plan?.items?.[0]?.status === "completed",
          ),
      ),
    );
    const legacy = await h.client.request(
      "session/read",
      { sessionId: id },
      zcodeSessionStateSnapshotSchema,
    );
    assert.deepEqual(legacy.todos, second);
    const other = await h.create();
    await h.subscribe(`conversation/${other}`);
    await h.command(h.envelope("sendText", other, { text: "read" }));
    await h.completed(other);
    assert.deepEqual(JSON.parse(f.requests.at(-1)!.messages.at(-1).content), { todos: [] });
    await h.close();
    const cold = f.start();
    assert.deepEqual((await snapshot(cold, id)).plan, snap.plan);
    await cold.command(cold.envelope("sendText", id, { text: "read" }));
    await cold.completed(id);
    assert.deepEqual(JSON.parse(f.requests.at(-1)!.messages.at(-1).content), { todos: second });
    await cold.close();
  } finally {
    await f.close();
  }
});

test("Rust Todo reminder follows TS ten-turn cadence across restart without visible user rows", async () => {
  const f = await fixture({
    respond(_req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: "done" });
      end(res, "stop");
    },
  });
  try {
    let h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    for (let i = 0; i < 22; i++) {
      if (i === 10) {
        await h.close();
        h = f.start();
        await h.subscribe(`conversation/${id}`);
      }
      const after = h.messages.length;
      await h.command(h.envelope("sendText", id, { text: `turn-${i}` }));
      await h.completed(id, after);
    }
    const reminders = (i: number) =>
      f.requests[i]!.messages.filter(
        (m: any) =>
          typeof m.content === "string" &&
          m.content.includes("TodoWrite tool hasn't been used recently"),
      );
    assert.equal(reminders(9).length, 0);
    assert.equal(reminders(10).length, 1);
    assert.equal(reminders(19).length, 1);
    assert.equal(reminders(20).length, 2);
    assert.equal(reminders(21).length, 2);
    const { buildTodoReminderBody } = await import(
      new URL(
        "../../../apps/zcode-cli/packages/core/src/runtime/helpers/runtime-reminders.ts",
        import.meta.url,
      ).href
    );
    assert.equal(
      reminders(10)[0].content,
      `<system-reminder>\n${buildTodoReminderBody([])}\n</system-reminder>`,
    );
    assert(!JSON.stringify(f.requests).includes("_zcode_source"));
    assert.equal((await h.rows(id)).rows.filter((r) => r.kind === "userInput").length, 22);
    await h.close();
  } finally {
    await f.close();
  }
});

test("Rust Todo truncates model output on UTF-8 boundaries while retaining full state and App plan", async () => {
  const large = [todo("界".repeat(19500))];
  const f = await todoFixture([
    ["TodoWrite", { todos: large }],
    ["TodoWrite", { todos: large }],
    ["TodoRead", {}],
  ]);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id);
    const results = f.requests[1]!.messages.filter((m: any) => m.role === "tool");
    assert(
      Buffer.byteLength(results[1].content) <= 100000,
      `bytes=${Buffer.byteLength(results[1].content)} tail=${results[1].content.slice(-180)}`,
    );
    assert.match(results[1].content, /Tool output truncated by resultBudget/);
    assert(!results[1].content.includes("�"));
    assert.deepEqual(JSON.parse(results[2].content), { todos: large });
    const snap = await snapshot(h, id);
    assert.equal(snap.plan.items[0].content, large[0]!.content);
    await h.close();
  } finally {
    await f.close();
  }
});

test("Rust Todo validates TS strict inputs, retains state on failure and clears an empty list", async () => {
  const valid = [todo("   "), { ...todo("任务"), unused: true }];
  const invalid: [string, unknown][] = [
    ["TodoRead", { unknown: true }],
    ["TodoRead", null],
    ["TodoWrite", {}],
    ["TodoWrite", { todos: null }],
    ["TodoWrite", { todos: [todo("")] }],
    ["TodoWrite", { todos: [todo("x", "invalid")] }],
    ["TodoWrite", { todos: [todo("x", "pending", "invalid")] }],
    ["TodoWrite", { todos: [], unknown: 1 }],
  ];
  for (const [name, input] of invalid)
    assert.equal(
      (name === "TodoRead" ? TodoReadInputSchema : TodoWriteInputSchema).safeParse(input).success,
      false,
    );
  const values: [string, unknown][] = [
    ["TodoWrite", { todos: valid }],
    ...invalid,
    ["TodoRead", {}],
    ["TodoWrite", { todos: [] }],
    ["TodoRead", {}],
  ];
  const f = await todoFixture(values);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id);
    const rows = (await h.rows(id)).rows.filter((r) => r.kind === "toolCall");
    assert.deepEqual(
      rows.map((r) => r.status),
      ["success", ...invalid.map(() => "error"), "success", "success", "success"],
    );
    const results = f.requests[1]!.messages.filter((m: any) => m.role === "tool");
    assert.deepEqual(
      JSON.parse(results.at(-3).content).todos,
      TodoWriteInputSchema.parse({ todos: valid }).todos,
    );
    assert.deepEqual(JSON.parse(results.at(-1).content), { todos: [] });
    assert.equal((await snapshot(h, id)).plan, null);
    await h.close();
    const cold = f.start();
    assert.equal((await snapshot(cold, id)).plan, null);
    await cold.close();
  } finally {
    await f.close();
  }
});

test("Rust Todo survives manual compaction, session close and cold history without replaying updates", async () => {
  const todos = [todo("keep durable progress", "in_progress", "high")];
  const f = await todoFixture([["TodoWrite", { todos }]]);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id);
    const after = h.messages.length;
    await h.command(h.envelope("compact", id));
    await h.completed(id, after);
    assert.equal(f.requests.at(-1)!.tools, undefined);
    assert.equal((await snapshot(h, id)).plan.items[0].content, todos[0]!.content);
    assert.equal((await h.command(h.envelope("deleteSession", id))).status, "accepted");
    const closed = await snapshot(h, id);
    assert.equal(closed.plan.items[0].content, todos[0]!.content);
    await h.close();
    const cold = f.start();
    await cold.subscribe(`conversation/${id}`);
    await cold.command(cold.envelope("sendText", id, { text: "read" }));
    await cold.completed(id);
    const request = f.requests.at(-1)!;
    assert.deepEqual(JSON.parse(request.messages.at(-1).content), { todos });
    assert(
      request.messages.some(
        (m: any) => typeof m.content === "string" && m.content.includes("COMPACT_SUMMARY"),
      ),
    );
    assert.equal(
      (await cold.rows(id)).rows.filter((r) => r.kind === "toolCall" && r.toolName === "TodoWrite")
        .length,
      1,
    );
    await cold.close();
  } finally {
    await f.close();
  }
});
