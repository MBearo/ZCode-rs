import assert from "node:assert/strict";
import test from "node:test";
import { access, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { buildPermissionDeniedContent } from "../../../apps/zcode-cli/packages/bootstrap/src/permission-options.js";
import { end, event, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

async function snapshot(h: Harness, id: string, connection = "inspect") {
  const after = h.messages.length;
  const sub = await h.subscribe(`conversation/${id}`, connection);
  const m = await h.wait(
    (m) =>
      m.params?.subscriptionId === sub.ack.subscriptionId &&
      m.params?.frame?.payload?.kind === "snapshot",
    after,
  );
  return m.params.frame.payload.snapshot;
}
async function prompt(h: Harness, id: string, after: number) {
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
async function rows(h: Harness, id: string): Promise<Message[]> {
  return (await h.rows(id)).rows;
}
async function session(h: Harness) {
  const id = await h.create();
  await h.subscribe(`conversation/${id}`);
  return id;
}
function prompted(h: Harness, id: string, after: number) {
  return h.messages
    .slice(after)
    .some(
      (m) =>
        m.params?.topic === `conversation/${id}` &&
        m.params.frame?.payload?.deltas?.some((d: Message) => d.patch?.pendingInteractions?.length),
    );
}

test("Rust build mode asks before Write; a denial with feedback reaches the model verbatim", async () => {
  const f = await fixture({ permissionMode: "build" });
  try {
    const h = f.start();
    const id = await session(h);
    const phone = await h.subscribe(`conversation/${id}`, "phone", "web-remote-replayable");
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "deny" }));
    const p = await prompt(h, id, before);
    // 桌面连续流与手机可重放流看到同一个待决请求。
    await h.wait(
      (m) =>
        m.params?.subscriptionId === phone.ack.subscriptionId &&
        m.params.frame?.payload?.deltas?.some((d: Message) =>
          d.patch?.pendingInteractions?.some((i: Message) => i.interactionId === p.interactionId),
        ),
      before,
    );
    assert.equal(p.kind, "permission");
    assert.equal(p.payload.toolName, "Write");
    assert.deepEqual(
      p.payload.options.map((o: Message) => o.optionId),
      ["allowOnce", "allowAlways", "deny"],
    );
    assert.equal(p.payload.fullAccessOption.optionId, "fullAccess");
    const pending = (await rows(h, id)).find((r: Message) => r.kind === "toolCall");
    assert.equal(pending?.status, "pendingApproval");
    assert.equal(pending?.approvalInteractionId, p.interactionId);
    const ack = await answer(h, id, p.interactionId, { optionId: "deny", freeText: "  use pnpm " });
    assert.equal(ack.status, "accepted");
    await h.completed(id, before);
    assert.equal(f.requests[1]!.messages.at(-1).content, buildPermissionDeniedContent("use pnpm"));
    const row = (await rows(h, id)).find((r: Message) => r.kind === "toolCall");
    assert.equal(row?.status, "cancelled");
    await assert.rejects(access(join(f.cwd, "result.txt")));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust allowAlways stores the project rule and later runs skip the prompt after restart", async () => {
  const f = await fixture({ permissionMode: "build" });
  try {
    const h = f.start();
    const id = await session(h);
    let before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "write" }));
    const p = await prompt(h, id, before);
    assert.equal(
      (await answer(h, id, p.interactionId, { optionId: "allowAlways" })).status,
      "accepted",
    );
    await h.completed(id, before);
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "written by Rust");
    await h.close();

    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    try {
      const row = db
        .prepare(
          "SELECT value FROM rust_project_setting WHERE namespace='permission' AND key='ruleset'",
        )
        .get() as { value: string };
      assert.deepEqual(JSON.parse(row.value), {
        version: 1,
        allow: [{ toolName: "Write", ruleContent: "result.txt" }],
      });
    } finally {
      db.close();
    }

    await rm(join(f.cwd, "result.txt"));
    const restored = f.start();
    const next = await session(restored);
    before = restored.messages.length;
    await restored.command(restored.envelope("sendText", next, { text: "write" }));
    await restored.completed(next, before);
    assert.equal(prompted(restored, next, before), false);
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "written by Rust");
  } finally {
    await f.close();
  }
});

test("Rust edit mode writes files without asking", async () => {
  const f = await fixture({ permissionMode: "edit" });
  try {
    const h = f.start();
    const id = await session(h);
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id, before);
    assert.equal(prompted(h, id, before), false);
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "written by Rust");
  } finally {
    await f.close();
  }
});

test("Rust full access switches the session to yolo and allows the pending call once", async () => {
  const f = await fixture({ permissionMode: "build" });
  try {
    const h = f.start();
    const id = await session(h);
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "write" }));
    const p = await prompt(h, id, before);
    assert.equal(
      (await answer(h, id, p.interactionId, { optionId: "fullAccess" })).status,
      "accepted",
    );
    await h.completed(id, before);
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "written by Rust");
    const config = (await snapshot(h, id)).config;
    assert.equal(config.mode, "yolo");
    assert.equal(config.planEnabled, false);
    assert.deepEqual(config.permissionGrant, { interactionId: p.interactionId });
  } finally {
    await f.close();
  }
});

test("Rust switchCollaborationMode is a no-op for the current mode and remembers the project mode", async () => {
  const f = await fixture({ permissionMode: null });
  try {
    const h = f.start();
    const id = await session(h);
    assert.equal((await snapshot(h, id)).config.mode, "build");
    const same = await h.command(h.envelope("switchCollaborationMode", id, { mode: "build" }));
    assert.equal(same.status, "noop");
    assert.equal(same.reasonCode, "config.unchanged");
    const edit = await h.command(h.envelope("switchCollaborationMode", id, { mode: "edit" }));
    assert.equal(edit.status, "accepted");
    assert.equal((await snapshot(h, id, "after-edit")).config.mode, "edit");
    await h.close();
    const restored = f.start();
    const next = await session(restored);
    // Node：新会话模式 = 创建参数 → 项目偏好 → 配置文件 → build。
    assert.equal((await snapshot(restored, next)).config.mode, "edit");
  } finally {
    await f.close();
  }
});

test("Rust rejects a goal submitted with Plan on", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await session(h);
    const ack = await h.command(
      h.envelope("sendGoalCommand", id, { text: "ship it", planEnabled: true }),
    );
    assert.equal(ack.status, "rejected");
    assert.equal(ack.reasonCode, "guard.planGoalMutuallyExclusive");
    assert.equal(f.requests.length, 0);
  } finally {
    await f.close();
  }
});

test("Rust subagent permission prompts surface on the root session with their origin", async () => {
  const f = await fixture({
    permissionMode: "build",
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const last = request.messages.at(-1);
      const tool = (id: string, name: string, args: unknown) => {
        event(response, {
          tool_calls: [
            { index: 0, id, type: "function", function: { name, arguments: JSON.stringify(args) } },
          ],
        });
        end(response, "tool_calls");
      };
      if (last.content === "delegate") {
        tool("agent", "Agent", { description: "write file", prompt: "child-write" });
      } else if (last.content === "child-write") {
        tool("child-call", "Write", { file_path: "child.txt", content: "from child" });
      } else {
        event(response, { content: "done" });
        end(response, "stop");
      }
    },
  });
  try {
    const h = f.start();
    const id = await session(h);
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "delegate" }));
    const p = await prompt(h, id, before);
    assert.equal(p.payload.toolName, "Write");
    assert.equal(p.payload.origin.kind, "subagent");
    assert.equal(p.payload.origin.parentSessionId, id);
    assert.equal(p.payload.origin.parentToolCallId, "agent");
    assert.equal(p.payload.origin.description, "write file");
    // 子代理请求不提供全权限入口（Node SubagentInteractionBroker）。
    assert.equal(p.payload.fullAccessOption, undefined);
    const agentRow = (await rows(h, id)).find((r: Message) => r.toolCallId === "agent");
    assert.equal(p.anchorRowId, agentRow?.rowId);
    assert.equal(
      (await answer(h, id, p.interactionId, { optionId: "allowOnce" })).status,
      "accepted",
    );
    await h.completed(id, before);
    assert.equal(await readFile(join(f.cwd, "child.txt"), "utf8"), "from child");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
