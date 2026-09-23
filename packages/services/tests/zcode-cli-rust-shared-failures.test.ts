import assert from "node:assert/strict";
import test from "node:test";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { zcodeSessionListResultSchema } from "@zcode/shared";
import { fixture, event, end } from "./zcode-cli-rust-fixture.js";
import { importShared, sharedSnapshot, ref, markdown } from "./zcode-cli-rust-shared-fixture.js";

for (const stage of ["import", "attach"] as const)
  test(`Shared ${stage} commit failure prevents publication and provider execution`, async () => {
    const f = await fixture();
    try {
      const h = f.start();
      await h.client.request("session/list", {}, zcodeSessionListResultSchema);
      if (stage === "attach") await importShared(h, f.cwd);
      const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
      db.exec(
        "CREATE TRIGGER fail_shared BEFORE INSERT ON rust_session BEGIN SELECT RAISE(ABORT,'injected shared commit failure'); END",
      );
      const before = h.messages.length;
      if (stage === "import") await assert.rejects(importShared(h, f.cwd), /fault.storage.commit/);
      else
        await assert.rejects(
          h.command(
            h.envelope("sendText", "shared-A", { text: "should not execute", context_refs: ref() }),
          ),
          /fault.storage.commit/,
        );
      await h.close(1);
      assert.equal(f.requests.length, 0);
      assert(
        !h.messages
          .slice(before)
          .some((m) =>
            m.params?.frame?.payload?.deltas?.some(
              (d: any) => d.patch?.sharedContextImport?.status === "attached",
            ),
          ),
      );
      const stored = db.prepare("SELECT body FROM rust_session WHERE id='shared-A'").get();
      if (stage === "import") assert.equal(stored, undefined);
      else {
        assert.equal(JSON.parse(stored!.body as string).sharedContext.provenance.status, "pending");
        assert.equal(db.prepare("SELECT count(*) AS n FROM rust_message").get()!.n, 0);
      }
      db.close();
    } finally {
      await f.close();
    }
  });

test("Compaction cannot see a pending candidate, and attached context is not reinjected after compaction", async () => {
  const f = await fixture({
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: req.tools?.length ? "done" : "SUMMARY preserves completed work" });
      end(res, "stop");
    },
  });
  try {
    const h = f.start();
    await importShared(h, f.cwd);
    await sharedSnapshot(h);
    await h.command(h.envelope("sendText", "shared-A", { text: "before attachment" }));
    await h.completed("shared-A");
    let offset = h.messages.length;
    await h.command(h.envelope("compact", "shared-A"));
    await h.completed("shared-A", offset);
    assert(!JSON.stringify(f.requests).includes("SHARED_CONTEXT_SECRET"));
    offset = h.messages.length;
    await h.command(h.envelope("sendText", "shared-A", { text: "attach", context_refs: ref() }));
    await h.completed("shared-A", offset);
    assert.equal(f.requests.at(-1)!.messages.filter((m: any) => m.content === markdown).length, 1);
    offset = h.messages.length;
    await h.command(h.envelope("compact", "shared-A"));
    await h.completed("shared-A", offset);
    await h.close();
    const cold = f.start();
    await sharedSnapshot(cold);
    await cold.command(cold.envelope("sendText", "shared-A", { text: "after compact" }));
    await cold.completed("shared-A");
    assert(!f.requests.at(-1)!.messages.some((m: any) => m.content === markdown));
    assert.equal((await sharedSnapshot(cold)).sharedContextImport.status, "attached");
    await cold.close();
  } finally {
    await f.close();
  }
});
