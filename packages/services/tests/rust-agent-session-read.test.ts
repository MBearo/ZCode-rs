import assert from "node:assert/strict";
import test from "node:test";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";
import { fixture } from "./rust-agent-fixture.js";

test("Host session/read projects tools and visible content without changing delivery, executing or losing cold history", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const read = (client = h.client, extra = {}) =>
      client.request("session/read", { sessionId: id, ...extra }, zcodeSessionStateSnapshotSchema);
    const draft = await read();
    assert.equal(draft.session.status, "idle");
    assert.deepEqual(draft.messages, []);
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id);
    const desktop = await read(h.client, { deliveryKind: "desktop-continuous" });
    const mobile = await read(h.client, { deliveryKind: "web-remote-replayable" });
    assert.equal(desktop.runtime.stateRevision, mobile.runtime.stateRevision);
    assert.equal((await read()).runtime.deliveryKind, undefined);
    assert.equal(desktop.messages.find((m) => m.info.role === "user")?.parts[0]?.type, "text");
    const tool = desktop.messages.flatMap((m) => m.parts).find((p) => p.type === "tool");
    assert.ok(tool?.type === "tool");
    assert.equal(tool.callId, "call-write");
    assert.equal(tool.state.status, "completed");
    assert.equal((await read(h.client, { messageLimit: 1 })).messages.length, 1);
    const requests = f.requests.length;
    await assert.rejects(
      h.client.request("session/read", { sessionId: "missing" }, zcodeSessionStateSnapshotSchema),
    );
    await h.close();
    const restarted = f.start();
    assert.deepEqual((await read(restarted.client)).messages, desktop.messages);
    assert.equal(f.requests.length, requests);
    assert.deepEqual(restarted.schemaErrors, []);
  } finally {
    await f.close();
  }
});
