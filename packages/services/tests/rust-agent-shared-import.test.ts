import assert from "node:assert/strict";
import test from "node:test";
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { SessionId, ProjectId, WorkspaceId, MessageId, PartId } from "@zcode/contracts";
import { fixture } from "./rust-agent-fixture.js";
import { sharedHistory, sharedSnapshot, markdown, ref } from "./rust-agent-shared-fixture.js";

export async function seedSharedImports(f: Awaited<ReturnType<typeof fixture>>) {
  const source = join(f.root, "ts.sqlite");
  const store = createSqliteSessionStore({ dbPath: source });
  for (const status of ["pending", "reserved", "attached", "discarded", "legacy"] as const) {
    const history = sharedHistory(`context-${status}`, status === "legacy" ? "attached" : status);
    const session = {
      id: status as SessionId,
      projectID: "fixture" as ProjectId,
      workspaceID: f.cwd as WorkspaceId,
      directory: f.cwd,
      slug: status,
      title: history.title,
      titleSource: "custom" as const,
      version: "fixture",
      time: { created: 100, updated: 100 },
    };
    const contextMessage = {
      info: {
        id: `msg-${status}` as MessageId,
        sessionID: session.id,
        role: "user" as const,
        time: { created: 100 },
        agent: "main",
        synthetic: true,
        source: "shared_context" as const,
        visibility: "model-only" as const,
        semantics: {
          origin: "import" as const,
          kind: "shared_context" as const,
          source: "conversation_share",
          uiVisibility: "hidden" as const,
          providerVisibility: "visible" as const,
          transcriptVisibility: "visible" as const,
        },
        ...(status === "legacy"
          ? {}
          : { metadata: { contextId: history.provenance.contextId, sharedContextStatus: status } }),
      },
      parts: [
        {
          id: `part-${status}` as PartId,
          sessionID: session.id,
          messageID: `msg-${status}` as MessageId,
          type: "text" as const,
          text: markdown,
        },
      ],
    };
    if (status === "legacy") {
      await store.createSession(session);
      await store.saveMessage(contextMessage.info);
      await store.savePart(contextMessage.parts[0]!);
    } else {
      await store.commitSharedContextImportBundle({
        session,
        contextMessage,
        provenance: {
          id: `shared:${status}`,
          sessionID: session.id,
          type: "v4/shared_context_import",
          time: { created: 100, updated: 100 },
          data: {
            ...history.provenance,
            ...(status === "reserved" ? { sourceId: "lost-queue" } : {}),
          },
        },
      });
    }
    await store.saveSessionEntry({
      id: `mode-${status}`,
      sessionID: session.id,
      type: "runtime/execution_state",
      time: { created: 100, updated: 100 },
      data: { mode: "yolo", planEnabled: false },
    });
    await store.saveSessionEntry({
      id: `model-${status}`,
      sessionID: session.id,
      type: "runtime/model_selection",
      time: { created: 100, updated: 100 },
      // Store 接口接收 ModelSelection，SQLite adapter 自己添加 modelSelection 包装。
      data: {
        providerId: "fixture",
        modelId: "core-model",
        options: { reasoningLevel: "none" },
      },
    });
  }
  store.close();
  return { source, original: await readFile(source) };
}

test("Real TS shared import lifecycle is preserved without injecting pending, reserved or discarded candidates", async () => {
  const f = await fixture({ legacy: true });
  try {
    const { source, original } = await seedSharedImports(f);
    const h = f.start();
    for (const status of ["pending", "reserved", "attached", "discarded", "legacy"]) {
      const s = await sharedSnapshot(h, status);
      assert.equal(
        s.sharedContextImport.status,
        status === "legacy" ? undefined : status === "reserved" ? "pending" : status,
      );
      assert.deepEqual(s.rows.window, []);
      await h.command(h.envelope("sendText", status, { text: `check ${status}` }));
      await h.completed(status);
      assert.equal(
        JSON.stringify(f.requests.at(-1)).includes("SHARED_CONTEXT_SECRET"),
        status === "attached" || status === "legacy",
      );
    }
    const offset = h.messages.length;
    await h.command(
      h.envelope("sendText", "pending", {
        text: "attach migrated candidate",
        context_refs: ref("context-pending"),
      }),
    );
    await h.completed("pending", offset);
    assert.equal(f.requests.at(-1)!.messages.filter((m: any) => m.content === markdown).length, 1);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
    assert.deepEqual(await readFile(source), original);
    const cold = f.start();
    assert.equal((await sharedSnapshot(cold, "pending")).sharedContextImport.status, "attached");
    await cold.close();
  } finally {
    await f.close();
  }
});

test("Corrupt TS shared provenance rolls back the entire import rather than silently sending its text", async () => {
  const f = await fixture({ legacy: true });
  try {
    const { source } = await seedSharedImports(f);
    const sourceDb = new DatabaseSync(source);
    sourceDb
      .prepare(
        "UPDATE session_entry SET data=json_set(data,'$.markdownSha256',?) WHERE id='shared:pending'",
      )
      .run("c".repeat(64));
    sourceDb.close();
    const original = await readFile(source);
    const h = f.start();
    await h.wait((m) => m.method === "startup/storageState" && m.params.phase === "failed");
    await h.close(1);
    assert.match(h.stderr, /Shared context digest mismatch/);
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"), { readOnly: true });
    assert.equal(db.prepare("SELECT count(*) AS n FROM rust_session").get()!.n, 0);
    db.close();
    assert.deepEqual(await readFile(source), original);
    assert.equal(f.requests.length, 0);
  } finally {
    await f.close();
  }
});
