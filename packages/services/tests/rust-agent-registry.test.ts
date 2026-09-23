import assert from "node:assert/strict";
import test from "node:test";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import { zcodeProviderRuntimeHeadersRequestParamsSchema } from "@zcode/shared";
import { fixture, event, end } from "./rust-agent-fixture.js";

import { configureRegistry as configure } from "./rust-agent-registry-fixture.js";

test("Account auth failure and workspace cancellation do not make HTTP requests or persist auxiliary sessions", async () => {
  const f = await fixture({ registry: true });
  try {
    const { revision } = await configure(f, true);
    const h = f.start();
    await h.client.request(
      "provider/updateAccountConfig",
      {
        revision: "auth",
        basedOnZCodeBuiltinRevision: revision,
        providers: { "account:fixture": { access: { type: "zhipu-account", entitled: true } } },
        states: { "account:fixture": { current: true } },
      },
      z.any(),
    );
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "failed auth" }));
    const auth = await h.wait((m) => m.method === "interaction/requestProviderRuntimeHeaders");
    await h.client.respondError(auth.id, { code: -32000, message: "fixture auth rejected" });
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) => d.patch?.control?.lastError?.attribution?.reason === "auth_failed",
      ),
    );
    const before = h.messages.length;
    const pending = h.client.request(
      "workspace/generateText",
      {
        workspace: { workspacePath: f.cwd },
        operationId: "cancel-op",
        prompt: "do not persist",
        querySource: "fixture",
      },
      z.any(),
    );
    const rejected = assert.rejects(pending);
    const waiting = await h.wait(
      (m) => m.method === "interaction/requestProviderRuntimeHeaders",
      before,
    );
    const result = await h.client.request(
      "workspace/cancelGenerateText",
      { workspace: { workspacePath: f.cwd }, operationId: "cancel-op" },
      z.any(),
    );
    assert.equal(result.cancelled, true);
    await rejected;
    await h.client.respond(waiting.id, {
      headersApplied: true,
      requestAuth: { apiKey: "late-auxiliary-secret" },
    });
    assert.equal(f.requests.length, 0);
    await h.subscribe(`sessions-index/${f.cwd}`);
    const index = await h.wait((m) => m.params?.frame?.payload?.snapshot?.sessions);
    assert.deepEqual(
      index.params.frame.payload.snapshot.sessions.map((s: any) => s.sessionId),
      [id],
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Removed queued model holds its admitted input without terminating the actor", async () => {
  let finish: () => void = () => {};
  const f = await fixture({
    registry: true,
    respond: async (_request, res) => {
      await new Promise<void>((r) => {
        finish = r;
      });
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      event(res, { content: "done" });
      end(res, "stop");
    },
  });
  try {
    const { personal } = await configure(f);
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.subscribe(`workspace-config/${f.cwd}`);
    await h.command(h.envelope("sendText", id, { text: "running" }));
    await h.command(
      h.envelope("sendText", id, {
        text: "queued",
        modelSelection: {
          providerId: "personal:fixture",
          modelId: "model-b",
          options: { reasoningLevel: "high" },
        },
      }),
    );
    personal.config.providerConfigRules.providerRules[0]!.config.personalModelIds = ["model-a"];
    const before = h.messages.length;
    await writeFile(join(f.root, "personal.json"), JSON.stringify(personal));
    await h.wait(
      (m) => m.params?.frame?.payload?.snapshot?.config?.configOptions?.[0]?.options?.length === 1,
      before,
    );
    finish();
    const held = await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) => d.patch?.control?.lastError?.code === "model_not_found",
      ),
    );
    const patch = held.params.frame.payload.deltas.find(
      (d: any) => d.patch?.control?.lastError,
    )?.patch;
    assert.equal(patch.queue.items[0].text, "queued");
    assert.equal(patch.queue.autoDrain, false);
    assert.equal(
      (await h.client.request("runtime/capabilities", {}, z.any())).accountProviderConfig,
      true,
    );
    assert.equal(f.requests.length, 1);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    finish();
    await f.close();
  }
});

test("App provider files drive model selection, hot switch after tools, queue and restart without Rust JSON", async () => {
  const f = await fixture({
    registry: true,
    respond: async (request, res) => {
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      if (request.messages.at(-1).content === "tools") {
        event(res, {
          tool_calls: [
            {
              index: 0,
              id: "slow-tool",
              type: "function",
              function: {
                name: "Bash",
                arguments: JSON.stringify({ command: "sleep 0.3; echo done" }),
              },
            },
          ],
        });
        end(res, "tool_calls");
      } else {
        event(res, { content: request.model });
        end(res, "stop");
      }
    },
  });
  try {
    const { personal } = await configure(f);
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "tools" }));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some((d: any) => d.row?.kind === "toolCall"),
    );
    await h.command(
      h.envelope("switchModelConfig", id, {
        provider: "personal:fixture",
        model: "model-b",
        thought: "high",
      }),
    );
    await h.completed(id);
    assert.deepEqual(
      f.requests.map((r) => [r.model, r.reasoning_effort]),
      [
        ["model-a", "low"],
        ["model-b", "high"],
      ],
    );
    assert.equal(f.requestHeaders[0]!.authorization, "Bearer fixture-personal-key");
    for (const request of f.requests) {
      assert.match(
        request.messages[2].content,
        new RegExp(`model named personal:fixture/${request.model}\\.`),
      );
    }
    await h.close();
    const h2 = f.start();
    await h2.subscribe(`conversation/${id}`);
    await h2.command(h2.envelope("sendText", id, { text: "restored" }));
    await h2.completed(id);
    assert.equal(f.requests.at(-1)?.model, "model-b");
    personal.config.defaultModelSelection.modelId = "model-b";
    await writeFile(join(f.root, "personal.json"), JSON.stringify(personal));
    await new Promise((r) => setTimeout(r, 1200));
    const next = await h2.create();
    await h2.subscribe(`conversation/${next}`);
    await h2.command(h2.envelope("sendText", next, { text: "new" }));
    await h2.completed(next);
    assert.equal(f.requests.at(-1)?.model, "model-b");
    assert.deepEqual(h.schemaErrors, []);
    assert.deepEqual(h2.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Account auth is bidirectional, cancelled by stop, ignores late replies and never persists credentials", async () => {
  const f = await fixture({ registry: true });
  try {
    const { revision } = await configure(f, true);
    const h = f.start();
    const sync = await h.client.request(
      "provider/updateAccountConfig",
      {
        revision: "a1",
        basedOnZCodeBuiltinRevision: revision,
        providers: { "account:fixture": { access: { type: "zhipu-account", entitled: true } } },
        states: { "account:fixture": { current: true } },
      },
      z.any(),
    );
    assert.equal(sync.receivedRevision, "a1");
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "first" }));
    const auth = await h.wait((m) => m.method === "interaction/requestProviderRuntimeHeaders");
    zcodeProviderRuntimeHeadersRequestParamsSchema.parse(auth.params);
    await h.client.respond(auth.id, {
      headersApplied: true,
      requestAuth: { apiKey: "transient-secret-1", headers: { "x-fixture": "runtime" } },
    });
    await h.completed(id);
    assert.equal(f.requestHeaders[0]!.authorization, "Bearer transient-secret-1");
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "cancel" }));
    const waiting = await h.wait(
      (m) => m.method === "interaction/requestProviderRuntimeHeaders",
      after,
    );
    await h.command(h.envelope("stop", id));
    await h.wait(
      (m) =>
        m.method === "interaction/providerRuntimeHeadersCancelled" &&
        m.params.requestId === waiting.params.requestId,
      after,
    );
    await h.client.respond(waiting.id, {
      headersApplied: true,
      requestAuth: { apiKey: "late-secret" },
    });
    await new Promise((r) => setTimeout(r, 100));
    assert.equal(f.requests.length, 1);
    await h.close();
    for (const suffix of ["", "-wal"]) {
      const bytes = await readFile(join(f.dataDir, `rust-sessions.sqlite${suffix}`)).catch(() =>
        Buffer.alloc(0),
      );
      assert.equal(bytes.includes(Buffer.from("transient-secret")), false);
      assert.equal(bytes.includes(Buffer.from("late-secret")), false);
    }
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
