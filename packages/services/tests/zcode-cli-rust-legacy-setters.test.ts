import assert from "node:assert/strict";
import test from "node:test";
import {
  zcodeSessionStateSnapshotSchema,
  zcodeStateUpdatedNotificationSchema,
} from "@zcode/shared";
import { fixture, type Harness } from "./zcode-cli-rust-fixture.js";
import { configureRegistry } from "./zcode-cli-rust-registry-fixture.js";

type Message = Record<string, any>;
const PROVIDER = "personal:fixture";

function create(h: Harness, params: Message = {}) {
  return h.client.request(
    "session/create",
    { workspace: { workspacePath: h.workspace, workspaceKey: h.workspace }, ...params },
    zcodeSessionStateSnapshotSchema,
  );
}
type Setter = "setModel" | "setThoughtLevel" | "setMode";
function set(h: Harness, method: Setter, params: Message) {
  return h.client.request(`session/${method}`, params, zcodeSessionStateSnapshotSchema);
}
async function rejection(promise: Promise<unknown>) {
  try {
    await promise;
  } catch (error) {
    return error as { code?: number; message: string; data?: any };
  }
  throw new Error("expected rejection");
}
async function stateUpdated(h: Harness, after: number) {
  const m = await h.wait((m) => m.method === "state.updated", after);
  return zcodeStateUpdatedNotificationSchema.parse(m.params);
}
async function registryFixture() {
  const f = await fixture({ registry: true });
  await configureRegistry(f);
  return f;
}
/** V4 config of `id` as seen by the subscription `sub` after `after`. */
async function v4Config(h: Harness, sub: Message, after: number, model: string) {
  const m = await h.wait(
    (m) =>
      m.params?.subscriptionId === sub.ack.subscriptionId &&
      m.params.frame?.payload?.deltas?.some(
        (d: Message) => d.patch?.config?.modelSelection?.modelId === model,
      ),
    after,
  );
  return m.params.frame.payload.deltas.at(-1).patch.config;
}

test("Rust legacy setModel keeps the level, bumps stateRevision and notifies state.updated", async () => {
  const f = await registryFixture();
  try {
    const h = f.start();
    const s = await create(h);
    const id = s.session.sessionId;
    const desktop = await h.subscribe(`conversation/${id}`);
    const phone = await h.subscribe(`conversation/${id}`, "phone", "web-remote-replayable");
    const before = h.messages.length;
    const model = { providerId: PROVIDER, modelId: "model-b", options: { reasoningLevel: "high" } };
    const next = await set(h, "setModel", { sessionId: id, model, expectedRevision: 0 });
    assert.deepEqual(next.settings.model.current, model);
    assert.deepEqual(
      next.settings.model.available.map((m: Message) => m.ref),
      [{ providerId: PROVIDER, modelId: "model-b" }],
    );
    assert.equal(next.settings.thoughtLevel.current, "high");
    assert.equal(next.runtime.stateRevision, 1);
    // 仅配置变更不刷新 updatedAt。
    assert.equal(next.session.updatedAt, s.session.updatedAt);
    const notification = await stateUpdated(h, before);
    assert.deepEqual(notification, {
      patch: next.settings,
      reason: "model_changed",
      revision: 1,
      scope: "session",
      sessionId: id,
      type: "state.updated",
      workspace: { workspacePath: f.cwd, workspaceKey: f.cwd },
    });
    for (const sub of [desktop, phone]) {
      const config = await v4Config(h, sub, before, "model-b");
      assert.equal(config.thought, "high");
    }
    // 同一选择仍推进 legacy revision（Node 没有 noop 分支）。
    const again = await set(h, "setModel", { sessionId: id, model });
    assert.equal(again.runtime.stateRevision, 2);
    const stale = await rejection(
      set(h, "setModel", { sessionId: id, model, expectedRevision: 0 }),
    );
    assert.equal(stale.code, -32009);
    assert.equal(stale.message, "Session state revision mismatch");
    assert.deepEqual(stale.data, { actualRevision: 2, expectedRevision: 0 });
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust legacy setModel rejects registry misses with Node's errors", async () => {
  const f = await registryFixture();
  try {
    const h = f.start();
    const id = (await create(h)).session.sessionId;
    const cases: [Message, number, string, Message][] = [
      [
        { providerId: "nope", modelId: "x" },
        -32603,
        "Provider Registry 中不存在 Model: [object Object]",
        { name: "Error" },
      ],
      [
        { providerId: PROVIDER, modelId: "model-z" },
        -32603,
        `Provider Registry 中不存在 Model: ${PROVIDER}/model-z`,
        { name: "ModelProtocolError", code: "model_not_found" },
      ],
      [
        { providerId: PROVIDER, modelId: "model-b" },
        -32603,
        `Reasoning level is required for ${PROVIDER}/model-b`,
        { name: "ModelProtocolError", code: "invalid_model_request" },
      ],
      [
        { providerId: PROVIDER, modelId: "model-b", options: { reasoningLevel: "ultra" } },
        -32603,
        `Reasoning effort "ultra" is not supported by ${PROVIDER}/model-b`,
        { name: "ModelProtocolError", code: "invalid_model_request" },
      ],
    ];
    for (const [model, code, message, data] of cases) {
      const error = await rejection(set(h, "setModel", { sessionId: id, model }));
      assert.equal(error.code, code);
      assert.equal(error.message, message);
      assert.deepEqual(error.data, data);
    }
    const missing = await rejection(
      set(h, "setModel", { sessionId: "sess_gone", model: { providerId: PROVIDER, modelId: "a" } }),
    );
    assert.equal(missing.code, -32004);
    assert.equal(missing.message, "Session is not active: sess_gone");
    const invalid = await rejection(set(h, "setModel", { sessionId: id }));
    assert.equal(invalid.code, -32602);
    assert.equal(
      invalid.message,
      "Invalid params — model: Invalid input: expected object, received undefined",
    );
    // 失败不改变会话。
    const read = await set(h, "setThoughtLevel", { sessionId: id, thoughtLevel: "low" });
    assert.equal(read.settings.model.current?.modelId, "model-a");
    assert.equal(read.runtime.stateRevision, 1);
  } finally {
    await f.close();
  }
});

test("Rust legacy setThoughtLevel validates against the session model", async () => {
  const f = await registryFixture();
  try {
    const h = f.start();
    const id = (await create(h)).session.sessionId;
    const required = await rejection(set(h, "setThoughtLevel", { sessionId: "sess_gone" }));
    assert.equal(required.code, -32602);
    assert.equal(required.message, "thoughtLevel is required");
    const unsupported = await rejection(
      set(h, "setThoughtLevel", { sessionId: id, thoughtLevel: "ultra" }),
    );
    assert.equal(unsupported.message, "Unsupported reasoning effort: ultra");
    assert.deepEqual(unsupported.data, { name: "Error" });
    const before = h.messages.length;
    const next = await set(h, "setThoughtLevel", { sessionId: id, thoughtLevel: " high " });
    assert.deepEqual(next.settings.model.current, {
      providerId: PROVIDER,
      modelId: "model-a",
      options: { reasoningLevel: "high" },
    });
    assert.equal(next.settings.thoughtLevel.current, "high");
    assert.equal((await stateUpdated(h, before)).reason, "thought_level_changed");
    // Claude 导入后模型解绑：不属于 Registry。
    const imported = await create(h, {
      sessionId: "claude-import-x",
      importedHistory: { source: "claudeCode", messages: [{ role: "user", content: "hi" }] },
    });
    const unbound = await rejection(
      set(h, "setThoughtLevel", { sessionId: imported.session.sessionId, thoughtLevel: "low" }),
    );
    assert.equal(unbound.message, "当前 Session Model 不属于 Provider Registry");
  } finally {
    await f.close();
  }
});

test("Rust legacy setMode applies auto, remembers it and shows plan as the permission mode", async () => {
  const f = await fixture({ permissionMode: null });
  try {
    const h = f.start();
    const id = (await create(h)).session.sessionId;
    const before = h.messages.length;
    const auto = await set(h, "setMode", { sessionId: id, mode: "auto" });
    assert.equal(auto.settings.mode.current, "auto");
    assert.equal(auto.settings.permission?.mode, "auto");
    const notification = await stateUpdated(h, before);
    assert.equal(notification.reason, "mode_changed");
    assert.equal((notification.patch as Message).mode.current, "auto");
    const invalid = await rejection(set(h, "setMode", { sessionId: id, mode: "turbo" }));
    assert.equal(invalid.code, -32602);
    await h.close();
    // 项目偏好记录权限模式，新会话沿用。
    const restored = f.start();
    const next = await create(restored);
    assert.equal(next.settings.mode.current, "auto");
    const plan = await set(restored, "setMode", {
      sessionId: next.session.sessionId,
      mode: "plan",
    });
    // plan 保留当前权限模式，legacy 设置只显示权限模式。
    assert.equal(plan.settings.mode.current, "auto");
    assert.equal(plan.runtime.stateRevision, 1);
  } finally {
    await f.close();
  }
});

test("Rust legacy session/list shows resident immediate drafts like Node", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const immediate = await create(h, { parentSessionId: "parent" });
    await create(h, { persistence: "deferred" });
    const listed = (await h.client.request("session/list", {})) as Message;
    assert.equal(listed.sessions.length, 1);
    const row = listed.sessions[0];
    assert.equal(row.sessionId, immediate.session.sessionId);
    assert.equal(row.title, "");
    assert.equal(row.status, "idle");
    assert.equal(row.parentSessionId, "parent");
    assert.equal(row.traceId, immediate.session.traceId);
    assert.equal(row.createdAt, row.updatedAt);
    assert.deepEqual(row.workspace, { workspacePath: f.cwd, workspaceKey: f.cwd });
    const other = (await h.client.request("session/list", {
      workspace: { workspacePath: "/elsewhere", workspaceKey: "/elsewhere" },
    })) as Message;
    assert.deepEqual(other.sessions, []);
    const byId = (await h.client.request("session/list", {
      sessionIds: [immediate.session.sessionId],
    })) as Message;
    assert.deepEqual(byId.sessions, []);
  } finally {
    await f.close();
  }
});
