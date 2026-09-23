import assert from "node:assert/strict";
import test from "node:test";
import { access } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { createHash } from "node:crypto";
import type { ServerResponse } from "node:http";
import { z } from "zod";
import { fixture, event, end, waitForFile } from "./zcode-cli-rust-fixture.js";

test("closing drafts clears both delivery subscriptions and is idempotent without creating history", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const sid = await h.create();
    const desktop = await h.subscribe(`conversation/${sid}`);
    const mobile = await h.subscribe(`conversation/${sid}`, "mobile", "web-remote-replayable");
    await h.subscribe(`sessions-index/${f.cwd}`);
    const command = h.envelope("deleteSession", sid);
    const after = h.messages.length;
    assert.equal((await h.command(command)).status, "accepted");
    assert.equal((await h.command(command)).status, "duplicate");
    await h.wait(
      (m) =>
        m.params?.frame?.payload?.deltas?.some(
          (d: any) => d.op === "session.removed" && d.sessionId === sid,
        ),
      after,
    );
    for (const [result, connectionId] of [
      [desktop, "fixture-desktop"],
      [mobile, "mobile"],
    ] as const) {
      await assert.rejects(
        h.client.request(
          "v4/conversation/resync",
          {
            topic: `conversation/${sid}`,
            connectionId,
            subscriptionId: result.ack.subscriptionId,
          },
          z.unknown(),
        ),
        /Subscription unavailable/,
      );
    }
    await assert.rejects(h.subscribe(`conversation/${sid}`), /Session unavailable/);
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"), { readOnly: true });
    assert.equal(db.prepare("SELECT count(*) AS n FROM rust_session WHERE id=?").get(sid)?.n, 0);
    assert.equal(db.prepare("SELECT count(*) AS n FROM rust_command").get()?.n, 0);
    db.close();
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("closing persisted history removes runtime only; cold subscribe restores a fresh epoch without model calls", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const sid = await h.create();
    const old = await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "hello" }));
    await h.completed(sid);
    const before = await h.rows(sid);
    const calls = f.requests.length;
    await h.subscribe(`sessions-index/${f.cwd}`, "index");
    await h.client.request(
      "v4/connection/flow",
      { connectionId: "index", state: "saturated" },
      z.unknown(),
    );
    const close = h.envelope("deleteSession", sid);
    assert.equal((await h.command(close)).status, "accepted");
    await assert.rejects(
      h.client.request("session/read", { sessionId: sid }, z.unknown()),
      /Session unavailable/,
    );
    const from = h.messages.length;
    await h.client.request(
      "v4/connection/flow",
      { connectionId: "index", state: "drained" },
      z.unknown(),
    );
    const frame = await h.wait(
      (m) =>
        m.params?.topic === `sessions-index/${f.cwd}` && m.params.frame.payload.kind === "snapshot",
      from,
    );
    assert.equal(
      frame.params.frame.payload.snapshot.sessions.some((s: any) => s.sessionId === sid),
      false,
    );
    const reopened = await h.subscribe(`conversation/${sid}`);
    assert.notEqual(reopened.ack.logEpoch, old.ack.logEpoch);
    assert.deepEqual((await h.rows(sid)).rows, before.rows);
    assert.equal(f.requests.length, calls);
    assert.equal((await h.command(close)).status, "duplicate");
    const next = h.messages.length;
    await h.command(h.envelope("sendText", sid, { text: "continued" }));
    await h.completed(sid, next);
    const saved = await h.rows(sid);
    await h.close();
    const restarted = f.start();
    await restarted.subscribe(`conversation/${sid}`);
    assert.deepEqual((await restarted.rows(sid)).rows, saved.rows);
    assert.equal((await restarted.command(close)).status, "duplicate");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test(
  "close cancels foreground Shell, discards queued ACK and preserves another session",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture();
    try {
      const h = f.start();
      const sid = await h.create();
      const other = await h.create();
      await h.subscribe(`conversation/${sid}`);
      await h.subscribe(`conversation/${other}`);
      await h.command(h.envelope("sendText", sid, { text: "slow-shell" }));
      await h.wait((m) =>
        m.params?.frame?.payload?.deltas?.some((d: any) => d.row?.toolName === "Bash"),
      );
      // 工具 start 事件早于 Shell spawn，等真实 pid 文件后才能证明取消了运行中的进程。
      const pid = Number(await waitForFile(join(f.cwd, "shell.pid")));
      const stale = { ...h.envelope("deleteSession", sid), baseRevision: 999999 };
      assert.equal((await h.command(stale)).status, "stale");
      process.kill(pid, 0);
      const queued = h.envelope("sendText", sid, { text: "write" });
      assert.equal((await h.command(queued)).result?.type, "inputAccepted");
      assert.equal((await h.command(h.envelope("deleteSession", sid))).status, "accepted");
      assert.throws(() => process.kill(pid, 0));
      const ack = await h.command(queued);
      assert.equal(ack.status, "failed");
      assert.equal(ack.reasonCode, "fault.input.discardedOnClose");
      await h.subscribe(`conversation/${sid}`);
      const rows = await h.rows(sid);
      assert.equal(rows.rows.filter((r) => r.kind === "toolCall").at(-1)?.status, "cancelled");
      assert.equal(
        rows.rows.some((r) => r.kind === "userInput" && r.text === "write"),
        false,
      );
      await h.command(h.envelope("sendText", other, { text: "hello" }));
      await h.completed(other);
      await assert.rejects(access(join(f.cwd, "result.txt")));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  },
);

test("close clears session upload transactions while retaining committed history attachments", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    const content = Buffer.from("saved attachment");
    const p = { connectionId: "mobile", sessionId: sid, uploadId: "saved" };
    const meta = {
      ...p,
      fileName: "saved.txt",
      mime: "text/plain",
      totalBytes: content.length,
      totalChunks: 1,
      checksum: `sha256:${createHash("sha256").update(content).digest("hex")}`,
    };
    await h.client.request("v4/attachment/begin", meta, z.unknown());
    await h.client.request(
      "v4/attachment/chunk",
      { ...p, chunkIndex: 0, dataBase64: content.toString("base64") },
      z.unknown(),
    );
    const { ref } = await h.client.request(
      "v4/attachment/commit",
      p,
      z.object({ ref: z.string() }),
    );
    await h.command(
      h.envelope("sendText", sid, {
        text: "attached",
        attachments: [{ ref, fileName: "saved.txt", mime: "text/plain", bytes: content.length }],
      }),
    );
    await h.completed(sid);
    await h.command(h.envelope("deleteSession", sid));
    await h.subscribe(`conversation/${sid}`);
    await assert.rejects(
      h.client.request("v4/attachment/commit", p, z.unknown()),
      /uploadNotFound/,
    );
    const row = (await h.rows(sid)).rows.find((r) => r.kind === "userInput");
    assert.ok(row);
    const read = await h.client.request(
      "v4/conversation/attachmentRead",
      {
        sessionId: sid,
        ref,
        target: { rowId: row.rowId, entityId: row.entityId },
        attachmentIndex: 0,
        offset: 0,
        limit: 1024,
      },
      z.object({ dataBase64: z.string() }).passthrough(),
    );
    assert.equal(Buffer.from(read.dataBase64, "base64").toString(), content.toString());
    const restarted = await h.client.request(
      "v4/attachment/begin",
      meta,
      z.object({ nextChunkIndex: z.number() }).passthrough(),
    );
    assert.equal(restarted.nextChunkIndex, 0);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test(
  "close waits for background Shell cleanup and keeps its durable terminal state",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture({
      respond(request, response) {
        response.writeHead(200, { "Content-Type": "text/event-stream" });
        if (request.messages.at(-1).role !== "tool") {
          event(response, {
            tool_calls: [
              {
                index: 0,
                id: "background-close",
                type: "function",
                function: {
                  name: "Bash",
                  arguments: JSON.stringify({
                    command: "echo $$ > background.pid; sleep 30",
                    run_in_background: true,
                  }),
                },
              },
            ],
          });
          end(response, "tool_calls");
        } else {
          event(response, { content: "background registered" });
          end(response, "stop");
        }
      },
    });
    try {
      const h = f.start();
      const sid = await h.create();
      await h.subscribe(`conversation/${sid}`);
      await h.command(h.envelope("sendText", sid, { text: "background" }));
      await h.completed(sid);
      const pid = Number(await waitForFile(join(f.cwd, "background.pid")));
      process.kill(pid, 0);
      assert.equal((await h.command(h.envelope("deleteSession", sid))).status, "accepted");
      assert.throws(() => process.kill(pid, 0));
      const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"), { readOnly: true });
      const saved = JSON.parse(
        String(db.prepare("SELECT body FROM rust_session WHERE id=?").get(sid)?.body),
      );
      assert.deepEqual(
        Object.values(saved.background).map((v: any) => v.status),
        ["cancelled"],
      );
      db.close();
    } finally {
      await f.close();
    }
  },
);

test("a failed close commit never acknowledges success or emits removal and stops the actor", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "hello" }));
    await h.completed(sid);
    await h.subscribe(`sessions-index/${f.cwd}`);
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    db.exec(
      "CREATE TRIGGER reject_close BEFORE INSERT ON rust_command WHEN json_extract(new.ack,'$.commandId')='close-failure' BEGIN SELECT RAISE(FAIL,'injected close failure'); END;",
    );
    const before = db.prepare("SELECT body FROM rust_session WHERE id=?").get(sid)?.body;
    const from = h.messages.length;
    await assert.rejects(
      h.command({ ...h.envelope("deleteSession", sid), commandId: "close-failure" }),
      /fault.storage.commit/,
    );
    await h.close(1);
    assert.equal(
      h.messages
        .slice(from)
        .some((m) =>
          m.params?.frame?.payload?.deltas?.some((d: any) => d.op === "session.removed"),
        ),
      false,
    );
    assert.equal(db.prepare("SELECT body FROM rust_session WHERE id=?").get(sid)?.body, before);
    assert.equal(
      db
        .prepare(
          "SELECT count(*) AS n FROM rust_command WHERE json_extract(ack,'$.commandId')='close-failure'",
        )
        .get()?.n,
      0,
    );
    db.close();
  } finally {
    await f.close();
  }
});

test("closing a partial stream preserves displayed content and isolates late provider output after reopening", async () => {
  const pending: ServerResponse[] = [];
  const f = await fixture({
    respond(_request, response) {
      pending.push(response);
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      event(response, { content: pending.length === 1 ? "before close" : "fresh response" });
    },
  });
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "old" }));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some((d: any) => d.row?.text === "before close"),
    );
    assert.equal((await h.command(h.envelope("deleteSession", sid))).status, "accepted");
    await h.subscribe(`conversation/${sid}`);
    const old = (await h.rows(sid)).rows.find((r) => r.kind === "assistantText");
    assert.equal(old?.text, "before close");
    assert.equal(old?.state, "interrupted");
    const after = h.messages.length;
    await h.command(h.envelope("sendText", sid, { text: "new" }));
    await h.wait(
      (m) => m.params?.frame?.payload?.deltas?.some((d: any) => d.row?.text === "fresh response"),
      after,
    );
    event(pending[0]!, { content: "late pollution" });
    end(pending[0]!, "stop");
    end(pending[1]!, "stop");
    await h.completed(sid, after);
    const rows = (await h.rows(sid)).rows;
    assert.equal(
      rows.some((r) => JSON.stringify(r).includes("late pollution")),
      false,
    );
    assert.equal(rows.filter((r) => r.kind === "assistantText").at(-1)?.text, "fresh response");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
