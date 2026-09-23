import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import { writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { z } from "zod";
import { fixture, event, end, type Harness } from "./zcode-cli-rust-fixture.js";
import { AttachmentUploadRegistry } from "../../../apps/zcode-cli/packages/bootstrap/src/zcode-protocol-v4/attachment-upload-registry.js";
import {
  v4AttachmentBeginResultSchema,
  v4AttachmentChunkResultSchema,
  v4AttachmentCommitResultSchema,
  v4ConversationAttachmentReadResultSchema,
} from "@zcode/shared/zcode-protocol-v4";

const meta = (sessionId: string, bytes: Buffer, totalChunks = 1) => ({
  connectionId: "fixture-desktop",
  sessionId,
  uploadId: "upload-1",
  fileName: "attached.txt",
  mime: "text/plain",
  totalBytes: bytes.length,
  totalChunks,
  checksum: `sha256:${createHash("sha256").update(bytes).digest("hex")}`,
});
const terminal = (p: ReturnType<typeof meta>) => ({
  connectionId: p.connectionId,
  sessionId: p.sessionId,
  uploadId: p.uploadId,
});
async function upload(h: Harness, id: string, bytes: Buffer, extra = {}) {
  const p = { ...meta(id, bytes, bytes.length ? 1 : 0), ...extra };
  await h.client.request("v4/attachment/begin", p, v4AttachmentBeginResultSchema);
  if (bytes.length)
    await h.client.request(
      "v4/attachment/chunk",
      { ...terminal(p), chunkIndex: 0, dataBase64: bytes.toString("base64") },
      v4AttachmentChunkResultSchema,
    );
  return h.client.request("v4/attachment/commit", terminal(p), v4AttachmentCommitResultSchema);
}

test("Rust attachment transactions match the TS registry for retry, conflicts, gaps, integrity and connection isolation", async () => {
  const f = await fixture();
  let commits = 0;
  const ts = new AttachmentUploadRegistry({
    now: Date.now,
    putSessionAttachment: async (_id, input) => {
      assert.equal(Buffer.from(input.bytes).toString(), "你好 Rust");
      commits++;
      return { ref: "committed-ref" };
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    const bytes = Buffer.from("你好 Rust");
    const p = meta(id, bytes, 2);
    for (let i = 0; i < 2; i++)
      assert.deepEqual(
        await h.client.request("v4/attachment/begin", p, v4AttachmentBeginResultSchema),
        ts.begin(p),
      );
    const steps = [
      { chunkIndex: 1, dataBase64: bytes.subarray(4).toString("base64") },
      { chunkIndex: 0, dataBase64: bytes.subarray(0, 4).toString("base64") },
      { chunkIndex: 0, dataBase64: bytes.subarray(0, 4).toString("base64") },
      { chunkIndex: 0, dataBase64: Buffer.from("conflict").toString("base64") },
      { chunkIndex: 1, dataBase64: bytes.subarray(4).toString("base64") },
    ];
    for (const step of steps) {
      const request = { ...terminal(p), ...step };
      let expected;
      try {
        expected = ts.chunk(request);
      } catch (error) {
        await assert.rejects(
          h.client.request("v4/attachment/chunk", request, v4AttachmentChunkResultSchema),
          { message: (error as Error).message },
        );
        continue;
      }
      assert.deepEqual(
        await h.client.request("v4/attachment/chunk", request, v4AttachmentChunkResultSchema),
        expected,
      );
    }
    await assert.rejects(
      h.client.request(
        "v4/attachment/commit",
        { ...terminal(p), connectionId: "other" },
        v4AttachmentCommitResultSchema,
      ),
      /uploadNotFound/,
    );
    const committed = await h.client.request(
      "v4/attachment/commit",
      terminal(p),
      v4AttachmentCommitResultSchema,
    );
    await ts.commit(terminal(p));
    assert.deepEqual(
      await h.client.request("v4/attachment/commit", terminal(p), v4AttachmentCommitResultSchema),
      committed,
    );
    assert.equal(
      (await h.client.request("v4/attachment/begin", p, v4AttachmentBeginResultSchema)).state,
      "committed",
    );
    await assert.rejects(
      h.client.request(
        "v4/attachment/begin",
        { ...p, fileName: "different.txt" },
        v4AttachmentBeginResultSchema,
      ),
      /beginConflict/,
    );
    assert.equal(commits, 1);
    for (const suffix of ["abort", "closed"]) {
      const pending = { ...p, uploadId: suffix };
      await h.client.request("v4/attachment/begin", pending, v4AttachmentBeginResultSchema);
      if (suffix === "abort")
        await h.client.request("v4/attachment/abort", terminal(pending), z.object({}));
      else
        await h.client.request(
          "v4/connection/flow",
          { connectionId: p.connectionId, state: "closed" },
          z.object({}),
        );
      await assert.rejects(
        h.client.request("v4/attachment/commit", terminal(pending), v4AttachmentCommitResultSchema),
        /uploadNotFound/,
      );
    }
    const bad = { ...p, uploadId: "bad", checksum: `sha256:${"0".repeat(64)}`, totalChunks: 1 };
    await h.client.request("v4/attachment/begin", bad, v4AttachmentBeginResultSchema);
    await assert.rejects(
      h.client.request("v4/attachment/commit", terminal(bad), v4AttachmentCommitResultSchema),
      /uploadIncomplete/,
    );
    await h.client.request(
      "v4/attachment/chunk",
      { ...terminal(bad), chunkIndex: 0, dataBase64: bytes.toString("base64") },
      v4AttachmentChunkResultSchema,
    );
    await assert.rejects(
      h.client.request("v4/attachment/commit", terminal(bad), v4AttachmentCommitResultSchema),
      /checksumMismatch/,
    );
    assert.equal(f.requests.length, 0);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust accepts uploaded attachment-only input, authorizes row previews and retains immutable bytes after restart", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const bytes = Buffer.from("ATTACHED_你好");
    const { ref } = await upload(h, id, bytes);
    const attachment = { ref, fileName: "attached.txt", mime: "text/plain", bytes: bytes.length };
    await assert.rejects(
      h.command(
        h.envelope("sendText", id, {
          text: "invalid metadata",
          attachments: [{ ...attachment, bytes: bytes.length + 1 }],
        }),
      ),
      /metadata does not match/,
    );
    assert.equal((await h.rows(id)).rows.length, 0);
    const ack = await h.command(
      h.envelope("sendText", id, { text: "", attachments: [attachment] }),
    );
    assert.equal(ack.status, "accepted");
    await h.completed(id);
    assert.match(JSON.stringify(f.requests[0]), /ATTACHED_你好/);
    assert(!JSON.stringify(f.requests[0]).includes("_zcode_attachment"));
    const row = (await h.rows(id)).rows.find((r: any) => r.kind === "userInput")! as any;
    assert.deepEqual(row.attachments, [attachment]);
    const read = {
      sessionId: id,
      target: { rowId: row.rowId, entityId: row.entityId },
      attachmentIndex: 0,
      ref,
      offset: 0,
      limit: 512,
    };
    assert.equal(
      Buffer.from(
        (
          await h.client.request(
            "v4/conversation/attachmentRead",
            read,
            v4ConversationAttachmentReadResultSchema,
          )
        ).dataBase64,
        "base64",
      ).toString(),
      bytes.toString(),
    );
    const other = await h.create();
    await assert.rejects(
      h.command(h.envelope("sendText", other, { text: "stolen", attachments: [attachment] })),
      /[Aa]ttachment/,
    );
    await assert.rejects(
      h.client.request(
        "v4/conversation/attachmentRead",
        { ...read, sessionId: other },
        v4ConversationAttachmentReadResultSchema,
      ),
    );
    await h.close();
    const restarted = f.start();
    assert.equal(
      Buffer.from(
        (
          await restarted.client.request(
            "v4/conversation/attachmentRead",
            read,
            v4ConversationAttachmentReadResultSchema,
          )
        ).dataBase64,
        "base64",
      ).toString(),
      bytes.toString(),
    );
    await restarted.subscribe(`conversation/${id}`);
    await restarted.command(restarted.envelope("sendText", id, { text: "continue" }));
    await restarted.completed(id);
    assert.match(JSON.stringify(f.requests.at(-1)), /ATTACHED_你好/);
    assert.deepEqual(h.schemaErrors, []);
    assert.deepEqual(restarted.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust snapshots local firstInput and queued attachments before admission; queued promotion never rereads the source", async () => {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const f = await fixture({
    respond: async (_req, res, attempt) => {
      if (attempt === 1) await gate;
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: "ok" });
      end(res, "stop");
    },
  });
  try {
    const path = join(f.cwd, "input.txt");
    await writeFile(path, "FROZEN_FIRST");
    const h = f.start();
    const attachment = { ref: path, fileName: "input.txt", mime: "text/plain", bytes: 12 };
    const create = await h.command(
      h.envelope("createSession", null, {
        workspaceId: f.cwd,
        firstInput: { text: "", attachments: [attachment] },
      }),
    );
    const id = (create.result as any).sessionId;
    await h.subscribe(`conversation/${id}`);
    await writeFile(path, "FROZEN_QUEUED");
    const ack = await h.command(
      h.envelope("sendText", id, { text: "queued", attachments: [{ ...attachment, bytes: 13 }] }),
    );
    assert.equal((ack.result as any).delivery, "queue");
    await rm(path);
    release();
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) => d.row?.kind === "userInput" && d.row.text === "queued",
      ),
    );
    await h.wait(() => f.requests.length === 2);
    assert.match(JSON.stringify(f.requests[0]), /FROZEN_FIRST/);
    assert.match(JSON.stringify(f.requests[1]), /FROZEN_QUEUED/);
    await h.completed(id, h.messages.length - 1);
    const rows = (await h.rows(id)).rows.filter((r: any) => r.kind === "userInput") as any[];
    assert.notEqual(rows[0].attachments[0].ref, rows[1].attachments[0].ref);
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"), { readOnly: true });
    const canonical = db
      .prepare("SELECT body FROM rust_message WHERE session=?")
      .all(id)
      .map((r) => r.body)
      .join("");
    db.close();
    assert(canonical.includes("_zcode_attachment"));
    assert(!canonical.includes("FROZEN_FIRST"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    release();
    await f.close();
  }
});
