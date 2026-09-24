import assert from "node:assert/strict";
import test from "node:test";
import { access, readFile } from "node:fs/promises";
import { join } from "node:path";
import { end, event, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

/** `enter` calls EnterPlanMode; `exit`/`exit+read` call ExitPlanMode (then Read). */
function planFixture() {
  return fixture({
    permissionMode: "build",
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      // 提醒以 user 消息插在末尾：按最后一条真实用户消息决定，且其后已有工具结果时收尾。
      const messages = request.messages as Message[];
      const index = messages.findLastIndex(
        (m) => m.role === "user" && !String(m.content).startsWith("<system-reminder>"),
      );
      const answered = messages.slice(index).some((m) => m.role === "tool");
      const text = answered ? "" : messages[index]?.content;
      const call = (calls: [string, string, unknown][]) => {
        event(response, {
          tool_calls: calls.map(([id, name, args], index) => ({
            index,
            id,
            type: "function",
            function: { name, arguments: JSON.stringify(args) },
          })),
        });
        end(response, "tool_calls");
      };
      if (text === "enter") {
        call([["enter", "EnterPlanMode", {}]]);
      } else if (text === "exit") {
        call([["exit", "ExitPlanMode", { plan: "step 1\nstep 2" }]]);
      } else if (text === "exit+read") {
        call([
          ["exit", "ExitPlanMode", { plan: "step 1" }],
          ["read", "Read", { file_path: "missing.txt" }],
        ]);
      } else {
        event(response, { content: "done" });
        end(response, "stop");
      }
    },
  });
}
async function session(h: Harness) {
  const id = await h.create();
  await h.subscribe(`conversation/${id}`);
  return id;
}
async function config(h: Harness, id: string, connection: string) {
  const after = h.messages.length;
  const sub = await h.subscribe(`conversation/${id}`, connection);
  const m = await h.wait(
    (m) =>
      m.params?.subscriptionId === sub.ack.subscriptionId &&
      m.params?.frame?.payload?.kind === "snapshot",
    after,
  );
  return m.params.frame.payload.snapshot.config;
}
async function approval(h: Harness, id: string, after: number) {
  const m = await h.wait(
    (m) =>
      m.params?.topic === `conversation/${id}` &&
      m.params.frame?.payload?.deltas?.some((d: Message) => d.patch?.pendingInteractions?.length),
    after,
  );
  return m.params.frame.payload.deltas.find((d: Message) => d.patch?.pendingInteractions?.length)
    .patch.pendingInteractions[0];
}
function answer(h: Harness, id: string, interactionId: string, response: Message) {
  return h.command(h.envelope("resolveInteraction", id, { interactionId, answer: response }));
}
function toolResult(request: Message, id: string) {
  return request.messages.find((m: Message) => m.role === "tool" && m.tool_call_id === id)?.content;
}
const lastUser = (request: Message) =>
  request.messages.findLast((m: Message) => m.role === "user")?.content as string;

test("Rust EnterPlanMode turns Plan on from the tool and reminds the model", async () => {
  const f = await planFixture();
  try {
    const h = f.start();
    const id = await session(h);
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "enter" }));
    await h.completed(id, before);
    assert.match(toolResult(f.requests[1]!, "enter"), /^Entered plan mode\./);
    assert.match(lastUser(f.requests[1]!), /^<system-reminder>\nPlan mode is active\./);
    const c = await config(h, id, "after-enter");
    assert.equal(c.planEnabled, true);
    assert.equal(c.mode, "build");
    assert.deepEqual(c.planTransition, { toolCallId: "enter", planEnabled: true });
    // 同一进程的下一轮：提醒保留在原位，且 5 条真实用户消息之前不再重复。
    const next = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "question" }));
    await h.completed(id, next);
    const reminders = f.requests[2]!.messages.filter((m: Message) =>
      String(m.content).startsWith("<system-reminder>\nPlan mode"),
    );
    assert.equal(reminders.length, 1);
  } finally {
    await f.close();
  }
});

test("Rust plan approval writes the plan file, exits Plan and reminds once", async () => {
  const f = await planFixture();
  try {
    const h = f.start();
    const id = await session(h);
    const phone = await h.subscribe(`conversation/${id}`, "phone", "web-remote-replayable");
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "exit", planEnabled: true }));
    const p = await approval(h, id, before);
    // 桌面连续流与手机可重放流看到同一个计划审批。
    await h.wait(
      (m) =>
        m.params?.subscriptionId === phone.ack.subscriptionId &&
        m.params.frame?.payload?.deltas?.some((d: Message) =>
          d.patch?.pendingInteractions?.some((i: Message) => i.interactionId === p.interactionId),
        ),
      before,
    );
    assert.equal(p.kind, "userInput");
    assert.equal(p.payload.kind, "userInput");
    assert.equal(p.payload.prompt, "Tool ExitPlanMode requires user interaction");
    assert.deepEqual(p.payload.schema, { interaction: "plan_approval", toolName: "ExitPlanMode" });
    assert.deepEqual(p.payload.questions[0].options, [
      {
        value: "approve",
        label: "Approve",
        description: "Exit plan mode and start implementation.",
      },
    ]);
    assert.equal(p.payload.fullAccessOption, undefined);
    const full = await answer(h, id, p.interactionId, { optionId: "fullAccess" });
    assert.equal(full.status, "failed");
    assert.equal((full as Message).message, "Full access is not supported for this interaction");
    assert.equal(
      (await answer(h, id, p.interactionId, { optionId: "allowOnce" })).status,
      "accepted",
    );
    await h.completed(id, before);
    assert.equal(
      toolResult(f.requests[1]!, "exit"),
      "User has approved your plan. You can now start coding. Start with updating your todo list if applicable.\n\n## Approved Plan:\nstep 1\nstep 2",
    );
    assert.equal(
      lastUser(f.requests[1]!),
      "<system-reminder>\n## Exited Plan Mode\n\nYou have exited plan mode. You can now make edits, run tools, and take actions.\n</system-reminder>",
    );
    assert.equal(
      await readFile(join(f.cwd, ".zcode", "plans", `plan-${id}.md`), "utf8"),
      "step 1\nstep 2",
    );
    const c = await config(h, id, "after-exit");
    assert.equal(c.planEnabled, false);
    assert.deepEqual(c.planTransition, { toolCallId: "exit", planEnabled: false });
    const plans = (await h.client.request("v4/conversation/plans", { sessionId: id })) as Message;
    assert.deepEqual(
      plans.plans.map((r: Message) => [r.toolName, r.status]),
      [["ExitPlanMode", "success"]],
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust plan feedback is steered into the same turn and keeps Plan on", async () => {
  const f = await planFixture();
  try {
    const h = f.start();
    const id = await session(h);
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "exit", planEnabled: true }));
    const p = await approval(h, id, before);
    await answer(h, id, p.interactionId, { freeText: "  add tests first " });
    await h.completed(id, before);
    assert.equal(toolResult(f.requests[1]!, "exit"), "The plan was not approved by the user.");
    assert.match(lastUser(f.requests[1]!), /add tests first/);
    const rows = (await h.rows(id)).rows as Message[];
    assert.equal(rows.find((r) => r.toolCallId === "exit")?.status, "cancelled");
    assert.equal(rows.filter((r) => r.kind === "userInput").at(-1)?.text, "add tests first");
    assert.equal((await config(h, id, "after-feedback")).planEnabled, true);
    await assert.rejects(access(join(f.cwd, ".zcode", "plans", `plan-${id}.md`)));
  } finally {
    await f.close();
  }
});

test("Rust plan denial without feedback ends the turn and cancels later tools", async () => {
  const f = await planFixture();
  try {
    const h = f.start();
    const id = await session(h);
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "exit+read", planEnabled: true }));
    const p = await approval(h, id, before);
    // 与 Node 一致：optionId "approve" 不是批准。
    await answer(h, id, p.interactionId, { optionId: "approve" });
    await h.completed(id, before);
    assert.equal(f.requests.length, 1);
    const rows = (await h.rows(id)).rows as Message[];
    const exit = rows.find((r) => r.toolCallId === "exit");
    const read = rows.find((r) => r.toolCallId === "read");
    assert.equal(exit?.status, "cancelled");
    assert.equal(exit?.output?.text, "Permission denied for ExitPlanMode");
    assert.equal(read?.status, "cancelled");
    assert.equal(
      read?.output?.text,
      "Tool cancelled because a previous tool result requested a turn stop.",
    );
    assert.equal((await config(h, id, "after-deny")).planEnabled, true);
  } finally {
    await f.close();
  }
});

test("Rust declares independent Plan state", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const capabilities = (await h.client.request("runtime/capabilities", {})) as Message;
    assert.equal(capabilities.independentPlanState, true);
  } finally {
    await f.close();
  }
});
