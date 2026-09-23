import assert from "node:assert/strict";
import test from "node:test";
import { zcodeSessionListResultSchema, zcodeSessionListParamsSchema } from "@zcode/shared";
import { fixture } from "./rust-agent-fixture.js";

test("Rust session/list observes persisted identity without opening cold sessions or altering subscriptions", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const list = (params: unknown = {}) =>
      h.client.request("session/list", params, zcodeSessionListResultSchema);
    assert.deepEqual(await list(), { sessions: [] });
    const id = await h.create();
    assert.deepEqual(await list({ sessionIds: [id] }), { sessions: [] });
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "hello" }));
    await h.completed(id);
    const before = await list();
    assert.equal(before.sessions.length, 1);
    assert.equal(before.sessions[0]!.sessionId, id);
    assert.equal(before.sessions[0]!.sessionKind, "interactive");
    assert.deepEqual((await list({ sessionIds: ["missing", id, id], limit: 1 })).sessions, [
      before.sessions[0],
      before.sessions[0],
    ]);
    const checkpoint = h.messages.length;
    for (let i = 0; i < 3; i++) assert.deepEqual(await list(), before);
    assert(!h.messages.slice(checkpoint).some((m) => m.method === "v4/conversation/frame"));
    const count = f.requests.length;
    await h.command(h.envelope("deleteSession", id));
    assert.deepEqual(await list(), before);
    assert.equal(f.requests.length, count);
    await h.close();
    const cold = f.start();
    assert.deepEqual(
      await cold.client.request("session/list", {}, zcodeSessionListResultSchema),
      before,
    );
    assert.equal(f.requests.length, count);
    assert.deepEqual(cold.schemaErrors, []);
    await cold.close();
  } finally {
    await f.close();
  }
});

test("Rust session/list rejects invalid current TS parameters rather than returning an empty success", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    for (const input of [
      { unknown: true },
      { limit: 0 },
      { limit: -1 },
      { limit: 1.5 },
      { limit: "2" },
      { limit: null },
      { includeArchived: null },
      { includeArchived: "true" },
      { sessionIds: [] },
      { sessionIds: [""] },
      { sessionIds: [" \t\n "] },
      { sessionIds: null },
      { sessionIds: Array(65).fill("x") },
      { workspace: {} },
      { workspace: null },
      { workspace: { workspacePath: "/x", workspaceKey: "/x", workspaceIdentity: null } },
      { workspace: { workspacePath: "  ", workspaceKey: "/x" } },
      { workspace: { workspacePath: "/x", workspaceKey: "/x", remoteSessionId: " " } },
    ]) {
      assert.equal(zcodeSessionListParamsSchema.safeParse(input).success, false);
      await assert.rejects(h.client.request("session/list", input, zcodeSessionListResultSchema));
    }
    assert.deepEqual(await h.client.request("session/list", null, zcodeSessionListResultSchema), {
      sessions: [],
    });
    await h.close();
  } finally {
    await f.close();
  }
});
