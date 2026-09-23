import assert from "node:assert/strict";
import test from "node:test";
import { join } from "node:path";
import { readFile, writeFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { configureRegistry } from "./zcode-cli-rust-registry-fixture.js";
import {
  importShared,
  sharedHistory,
  sharedSnapshot,
  ref,
  markdown,
} from "./zcode-cli-rust-shared-fixture.js";

test("Shared import is durable, idempotent and model-only; first reference attaches once through App schemas", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const imported = await importShared(h, f.cwd);
    assert.equal(imported.session.title, "Imported fixture");
    assert.deepEqual(imported.messages, []);
    assert.equal(f.requests.length, 0);
    assert.deepEqual(await importShared(h, f.cwd), imported);
    const changed = sharedHistory();
    changed.provenance.shareId = "conflict";
    await assert.rejects(importShared(h, f.cwd, "shared-A", changed), /conflicts/);
    let s = await sharedSnapshot(h);
    assert.equal(s.sharedContextImport.status, "pending");
    assert.deepEqual(s.rows.window, []);
    await sharedSnapshot(h, "shared-A", "phone", "web-remote-replayable");
    await h.command(h.envelope("sendText", "shared-A", { text: "without reference" }));
    await h.completed("shared-A");
    assert(!JSON.stringify(f.requests.at(-1)).includes("SHARED_CONTEXT_SECRET"));
    const before = h.messages.length;
    const command = h.envelope("sendText", "shared-A", {
      text: "use shared",
      context_refs: ref(" context-A "),
    });
    const ack = await h.command(command);
    assert.equal(ack.status, "accepted");
    await h.wait(
      (m) =>
        m.params?.topic === "conversation/shared-A" &&
        m.params.frame?.payload?.deltas?.some(
          (d: any) =>
            d.patch?.control?.phase === "completedSuccess" &&
            d.patch.revision >= ack.revisionAtDecision,
        ),
      before,
    );
    assert.equal((await h.command(command)).status, "duplicate");
    s = await sharedSnapshot(h);
    assert.equal(s.sharedContextImport.status, "attached");
    assert(!JSON.stringify(s.rows).includes("SHARED_CONTEXT_SECRET"));
    const messages = f.requests.at(-1)!.messages;
    const position = messages.findIndex((m: any) => m.content === markdown);
    assert(position > 0);
    assert.equal(messages[position + 1].content, "use shared");
    assert.equal(messages.filter((m: any) => m.content === markdown).length, 1);
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"), { readOnly: true });
    const metadata = db.prepare("SELECT body FROM rust_session WHERE id='shared-A'").get()!
      .body as string;
    db.close();
    assert(!metadata.includes("SHARED_CONTEXT_SECRET"));
    await assert.rejects(
      h.command(h.envelope("sendText", "shared-A", { text: "again", context_refs: ref() })),
      /NotAttachable/,
    );
    const last = h.messages.length;
    await h.command(h.envelope("sendText", "shared-A", { text: "continue" }));
    await h.completed("shared-A", last);
    assert.equal(f.requests.at(-1)!.messages.filter((m: any) => m.content === markdown).length, 1);
    await h.command(h.envelope("deleteSession", "shared-A"));
    assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "attached");
    await h.close();
    const cold = f.start();
    assert.equal((await sharedSnapshot(cold)).sharedContextImport.status, "attached");
    await cold.command(cold.envelope("sendText", "shared-A", { text: "cold" }));
    await cold.completed("shared-A");
    assert.equal(f.requests.at(-1)!.messages.filter((m: any) => m.content === markdown).length, 1);
    assert.deepEqual(cold.schemaErrors, []);
    await cold.close();
  } finally {
    await f.close();
  }
});

test("Shared references and provenance reject forged fields, cross-session IDs and altered content; discard stays durable", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const invalid = sharedHistory();
    invalid.markdown = "forged";
    await assert.rejects(importShared(h, f.cwd, "bad", invalid), /digest mismatch/);
    await importShared(h, f.cwd);
    await importShared(h, f.cwd, "shared-B", sharedHistory("context-B"));
    for (const refs of [
      null,
      {},
      ref("missing"),
      ref("context-B"),
      [...ref(), ...ref()],
      [{ ...ref()[0], markdown: "forged" }],
      ref("  "),
      [{ kind: "other", context_id: "context-A" }],
    ]) {
      await assert.rejects(
        h.command(h.envelope("sendText", "shared-A", { text: "invalid", context_refs: refs })),
      );
    }
    const wrongWorkspace = {
      sessionId: "cross",
      workspace: { workspacePath: f.cwd, workspaceIdentity: "other", workspaceKey: "other" },
      importedHistory: sharedHistory(),
    };
    await assert.rejects(
      h.client.request("session/create", wrongWorkspace, zcodeSessionStateSnapshotSchema),
      /identity mismatch/,
    );
    const s = await sharedSnapshot(h);
    assert.equal(s.sharedContextImport.status, "pending");
    const discard = h.envelope("discardSharedContext", "shared-A", { contextId: " context-A " });
    assert.equal((await h.command(discard)).status, "accepted");
    assert.equal((await h.command(discard)).status, "duplicate");
    assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "discarded");
    assert.equal(
      (await h.command(h.envelope("discardSharedContext", "shared-A", { contextId: "context-A" })))
        .status,
      "rejected",
    );
    await h.command(h.envelope("deleteSession", "shared-A"));
    assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "discarded");
    await h.close();
    const cold = f.start();
    await sharedSnapshot(cold);
    await cold.command(cold.envelope("sendText", "shared-A", { text: "discarded is not context" }));
    await cold.completed("shared-A");
    assert(!JSON.stringify(f.requests.at(-1)).includes("SHARED_CONTEXT_SECRET"));
    assert.deepEqual(cold.schemaErrors, []);
    await cold.close();
  } finally {
    await f.close();
  }
});

test("Shared import does not require an executable model or trigger account auth", async () => {
  const f = await fixture({ registry: true });
  try {
    await configureRegistry(f);
    const path = join(f.root, "personal.json");
    const config = JSON.parse(await readFile(path, "utf8"));
    config.config.providerConfigRules.providerRules = [];
    delete config.config.defaultModelSelection;
    await writeFile(path, JSON.stringify(config));
    const h = f.start();
    const imported = await importShared(h, f.cwd);
    assert.equal(imported.session.model, undefined);
    assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "pending");
    assert.equal(f.requests.length, 0);
    assert(!h.messages.some((m) => m.method === "provider/requestAuth"));
    await h.close();
  } finally {
    await f.close();
  }
});
