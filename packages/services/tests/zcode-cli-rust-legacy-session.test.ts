import assert from "node:assert/strict";
import test from "node:test";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";
import { fixture, type Harness } from "./zcode-cli-rust-fixture.js";
import { configureRegistry } from "./zcode-cli-rust-registry-fixture.js";

type Message = Record<string, any>;
const MISSING = /\bSession (not found|is not active):/i;

function create(h: Harness, params: Message) {
  return h.client.request(
    "session/create",
    { workspace: { workspacePath: h.workspace, workspaceKey: h.workspace }, ...params },
    zcodeSessionStateSnapshotSchema,
  );
}
function resume(h: Harness, params: Message) {
  return h.client.request("session/resume", params, zcodeSessionStateSnapshotSchema);
}
async function rejection(promise: Promise<unknown>) {
  try {
    await promise;
  } catch (error) {
    return error as { code?: number; message: string; data?: any };
  }
  throw new Error("expected rejection");
}
async function registryFixture() {
  const f = await fixture({ registry: true });
  await configureRegistry(f);
  return f;
}

test("Rust legacy session/create returns Node's fresh snapshot without touching storage or the model", async () => {
  const f = await registryFixture();
  try {
    const h = f.start();
    const s = await create(h, {});
    assert.match(s.session.sessionId, /^sess_[0-9a-f-]{36}$/);
    assert.equal(s.session.mode, "build");
    assert.equal(s.session.title, "");
    assert.equal(s.session.titleSource, undefined);
    assert.deepEqual(s.session.workspace, { workspacePath: f.cwd, workspaceKey: f.cwd });
    assert.equal(s.projection.sessionId, "unknown");
    assert.equal(s.projection.contextWindow, 200000);
    assert.equal(s.runtime.eventSeq, 0);
    assert.equal(s.runtime.stateRevision, 0);
    assert.deepEqual(
      s.settings.model.available.map((m: Message) => [
        m.ref.modelId,
        m.providerLabel,
        m.reasoning.defaultLevel,
      ]),
      [
        ["model-a", "Fixture", "high"],
        ["model-b", "Fixture", "high"],
      ],
    );
    assert.deepEqual(s.settings.model.current, {
      providerId: "personal:fixture",
      modelId: "model-a",
      options: { reasoningLevel: "low" },
    });
    assert.deepEqual(s.settings.model.lastUsed, {
      providerId: "personal:fixture",
      modelId: "model-a",
    });
    assert.deepEqual(s.settings.thoughtLevel, {
      available: [
        { label: "low", value: "low" },
        { label: "high", value: "high" },
      ],
      current: "low",
      enabled: true,
    });
    assert.deepEqual(s.todoGroups, []);
    assert.equal(f.requests.length, 0);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust legacy create applies model and thought level like Node setModel and setThoughtLevel", async () => {
  const f = await registryFixture();
  try {
    const h = f.start();
    const a = (options?: Message) => ({
      providerId: "personal:fixture",
      modelId: "model-a",
      ...(options ? { options } : {}),
    });
    const b = {
      providerId: "personal:fixture",
      modelId: "model-b",
      options: { reasoningLevel: "low" },
    };
    const cases: [Message, Message, string | undefined, number][] = [
      [{ thoughtLevel: "high" }, a({ reasoningLevel: "high" }), "high", 1],
      [{ model: b }, { providerId: "personal:fixture", modelId: "model-b" }, undefined, 1],
      [{ model: a({ reasoningLevel: "high" }), thoughtLevel: "ultra" }, a(), undefined, 1],
      [
        { model: a({ reasoningLevel: "low" }), thoughtLevel: " high " },
        a({ reasoningLevel: "high" }),
        "high",
        2,
      ],
    ];
    for (const [params, current, thought, revision] of cases) {
      const s = await create(h, params);
      assert.deepEqual(s.settings.model.current, current, JSON.stringify(params));
      assert.equal(s.settings.thoughtLevel.current, thought);
      assert.equal(s.runtime.stateRevision, revision);
    }
    const missingLevel = await rejection(
      create(h, { model: { providerId: "personal:fixture", modelId: "model-b" } }),
    );
    assert.equal(missingLevel.code, -32603);
    assert.equal(missingLevel.message, "Reasoning level is required for personal:fixture/model-b");
    assert.deepEqual(missingLevel.data, {
      name: "ModelProtocolError",
      code: "invalid_model_request",
    });
    const unknown = await rejection(
      create(h, {
        model: {
          providerId: "personal:fixture",
          modelId: "nope",
          options: { reasoningLevel: "low" },
        },
      }),
    );
    assert.equal(unknown.message, "Provider Registry 中不存在 Model: personal:fixture/nope");
    assert.equal(unknown.data?.code, "model_not_found");
    const provider = await rejection(create(h, { model: { providerId: "nope", modelId: "x" } }));
    assert.equal(provider.message, "Provider Registry 中不存在 Model: nope/x");
    assert.equal(provider.data?.name, "Error");
  } finally {
    await f.close();
  }
});

test("Rust legacy params errors carry Node's text and ZodError data", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const unknown = await rejection(create(h, { bogus: 1 }));
    assert.equal(unknown.code, -32602);
    assert.equal(unknown.message, 'Invalid params — (root): Unrecognized key: "bogus"');
    assert.equal(unknown.data.name, "ZodError");
    assert.deepEqual(JSON.parse(unknown.data.message), [
      {
        code: "unrecognized_keys",
        keys: ["bogus"],
        path: [],
        message: 'Unrecognized key: "bogus"',
      },
    ]);
    const id = await rejection(create(h, { sessionId: "s1" }));
    assert.equal(id.code, -32602);
    assert.equal(id.message, "sessionId is only supported for imported history creates");
    const mode = await rejection(resume(h, { sessionId: "s", mode: "plan" }));
    assert.equal(mode.message, 'Invalid params — (root): Unrecognized key: "mode"');
    const missing = await rejection(resume(h, { sessionId: "missing" }));
    assert.equal(missing.code, -32004);
    assert.equal(missing.message, "Session not found: missing");
    assert.match(missing.message, MISSING);
  } finally {
    await f.close();
  }
});

test("Rust deferred legacy create runs its first V4 input and keeps plan in the execution state", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const s = await create(h, {
      mode: "plan",
      persistence: "deferred",
      titleGenerationEnabled: false,
      parentSessionId: "parent",
    });
    assert.equal(s.settings.mode.current, "build");
    assert.equal(s.session.parentSessionId, undefined);
    const id = s.session.sessionId;
    const before = h.messages.length;
    await h.subscribe(`conversation/${id}`);
    const snapshot = await h.wait((m) => m.params?.frame?.payload?.kind === "snapshot", before);
    assert.equal(snapshot.params.frame.payload.snapshot.config.planEnabled, true);
    assert.equal(
      (await h.command(h.envelope("sendText", id, { text: "hello", planEnabled: false }))).status,
      "accepted",
    );
    await h.completed(id);
    assert.equal(f.requests.length, 1);
    const listed = (await h.client.request("session/list", {})) as Message;
    assert.deepEqual(
      listed.sessions.map((row: Message) => row.sessionId),
      [id],
    );
  } finally {
    await f.close();
  }
});

test("Rust legacy tool allowlist and denylist decide the tools the model sees", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const s = await create(h, { toolAllowlist: ["Read", "Bash"], toolDenylist: ["Bash"] });
    const id = s.session.sessionId;
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "hello" }));
    await h.completed(id);
    assert.deepEqual(
      f.requests[0]!.tools.map((t: Message) => t.function.name),
      ["Read"],
    );
  } finally {
    await f.close();
  }
});

test("Rust Claude history import writes Node ids, replaces earlier imports and leaves the model unbound", async () => {
  const f = await registryFixture();
  try {
    const h = f.start();
    const sessionId = "claude-import-abc";
    const history = {
      source: "claudeCode",
      title: " Claude chat ",
      messages: [
        { role: "user", content: "hi claude", timestamp: 1700000000000 },
        { role: "assistant", content: "hello human", timestamp: 1699999999000 },
        { role: "user", content: "second question" },
        { role: "assistant", content: "second answer", timestamp: 1700000050000 },
      ],
    };
    const s = await create(h, { sessionId, persistence: "immediate", importedHistory: history });
    assert.equal(s.session.sessionId, sessionId);
    assert.equal(s.session.title, "Claude chat");
    assert.equal(s.session.titleSource, "custom");
    assert.equal(s.session.createdAt, 1700000000000);
    assert.equal(s.session.model, undefined);
    assert.equal(s.settings.model.current, undefined);
    assert.deepEqual(s.settings.thoughtLevel, { available: [], enabled: false });
    assert.equal(s.projection.sessionId, sessionId);
    assert.equal(s.runtime.stateRevision, 1);
    const rows = (await h.rows(sessionId)).rows as Message[];
    assert.deepEqual(
      rows.map((r) => [r.kind, r.turnId, r.createdAt]),
      [
        ["turnHeader", `msg_${sessionId}_import_0`, 1700000000000],
        ["userInput", `msg_${sessionId}_import_0`, 1700000000000],
        ["assistantText", `msg_${sessionId}_import_0`, 1700000000001],
        ["turnHeader", `msg_${sessionId}_import_2`, 1700000000002],
        ["userInput", `msg_${sessionId}_import_2`, 1700000000002],
        ["assistantText", `msg_${sessionId}_import_2`, 1700000050000],
      ],
    );
    const again = await create(h, {
      sessionId,
      importedHistory: {
        source: "claudeCode",
        messages: [
          { role: "user", content: "only" },
          { role: "assistant", content: "one" },
        ],
      },
    });
    assert.equal(again.session.title, "Imported session");
    assert.equal(again.session.createdAt, 1700000000000);
    const replaced = (await h.rows(sessionId)).rows as Message[];
    assert.deepEqual(replaced.map((r) => r.text).filter(Boolean), ["only", "one"]);
    await h.close();
    const cold = f.start();
    const resumed = await resume(cold, { sessionId });
    assert.equal(resumed.session.title, "Imported session");
    assert.deepEqual(cold.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust legacy resume returns an active session untouched and restores a cold one from its last reply", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const s = await create(h, { mode: "edit" });
    const id = s.session.sessionId;
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "hello" }));
    await h.completed(id);
    const active = await resume(h, { sessionId: id, thoughtLevel: "high", toolDenylist: ["Read"] });
    assert.equal(active.runtime.stateRevision, 0);
    assert.equal(active.settings.mode.current, "edit");
    // 回复之后切换模式：Node 冷恢复时取最后一条回复记录的模式。
    await h.command(h.envelope("switchCollaborationMode", id, { mode: "yolo" }));
    await h.close();
    const cold = f.start();
    const resumed = await resume(cold, { sessionId: id });
    assert.equal(resumed.settings.mode.current, "edit");
    assert.equal(resumed.session.titleSource, "generated");
    assert.deepEqual(resumed.session.workspace, { workspacePath: f.cwd, workspaceKey: f.cwd });
    assert.equal(resumed.projection.sessionId, id);
    assert.deepEqual(cold.schemaErrors, []);
  } finally {
    await f.close();
  }
});
