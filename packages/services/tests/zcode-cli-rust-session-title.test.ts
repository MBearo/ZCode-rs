import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { event, end, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

const read = (db: string, sql: string, ...params: string[]) => {
  const conn = new DatabaseSync(db, { readOnly: true });
  try {
    return conn.prepare(sql).get(...params) as Record<string, any> | undefined;
  } finally {
    conn.close();
  }
};

async function titled(h: Harness, id: string, title: string) {
  await h.wait((m) =>
    m.params?.frame?.payload?.deltas?.some(
      (d: any) =>
        (d.op === "session.upserted" && d.session?.sessionId === id && d.session.title === title) ||
        d.patch?.meta?.title === title,
    ),
  );
}

// Node `session-title.ts`：协议会话首条输入落库后异步生成标题，写回会话行
// （titleSource = generated、titleMessageID），用量按 session_title 归属。
test("First prompt generates the session title like Node and short prompts keep theirs", async () => {
  const f = await fixture({ title: "修复登录跳转" });
  try {
    const h = f.start();
    await h.subscribe(`sessions-index/${f.cwd}`);
    const id = await h.create("please fix the login redirect bug");
    await h.subscribe(`conversation/${id}`);
    await h.completed(id);
    await titled(h, id, "修复登录跳转");
    assert.equal(f.titleRequests.length, 1);
    const [system, user] = f.titleRequests[0]!.messages;
    assert.match(system.content, /^Generate a concise title for this coding session\./);
    assert.deepEqual(user, { role: "user", content: "please fix the login redirect bug" });
    assert.equal(f.titleRequests[0]!.tools, undefined);
    const row = read(
      f.db,
      "SELECT title, title_source, title_message_id FROM session WHERE id=?",
      id,
    )!;
    assert.equal(row.title, "修复登录跳转");
    assert.equal(row.title_source, "generated");
    const first = read(
      f.db,
      "SELECT id FROM message WHERE session_id=? AND json_extract(data,'$.role')='user' ORDER BY sequence",
      id,
    )!;
    assert.equal(row.title_message_id, first.id);
    const usage = read(
      f.db,
      "SELECT status, provider_id FROM model_usage WHERE session_id=? AND query_source='session_title'",
      id,
    );
    assert.deepEqual({ ...usage }, { status: "completed", provider_id: "fixture" });
    // 后续轮次不再生成。
    await h.command(h.envelope("sendText", id, { text: "and also the logout page" }));
    await h.completed(id, h.messages.length - 1);
    const short = await h.create("hello");
    await h.subscribe(`conversation/${short}`);
    await h.completed(short);
    assert.equal(f.titleRequests.length, 1);
    assert.equal(
      read(f.db, "SELECT title_source FROM session WHERE id=?", short)!.title_source,
      "first_input",
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Goal titles set the session title and the goal summary title", async () => {
  const f = await fixture({
    title: "Parser refactor goal",
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const verify = request.messages.at(-1).content.includes("Verify whether");
      event(response, {
        content: verify ? '{"passed":true,"reason":"done","nextAction":""}' : "ok",
      });
      end(response, "stop");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendGoalCommand", id, { text: "refactor the parser module" }));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) => d.patch?.goal?.summaryTitle === "Parser refactor goal",
      ),
    );
    assert.equal(f.titleRequests.length, 1);
    assert.equal(f.titleRequests[0]!.messages[1].content, "refactor the parser module");
    await titled(h, id, "Parser refactor goal");
    const target = read(f.db, "SELECT summary_title FROM session_target WHERE session_id=?", id)!;
    assert.equal(target.summary_title, "Parser refactor goal");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Legacy session/create can disable title generation", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const session = await h.client.request(
      "session/create",
      {
        workspace: { workspacePath: f.cwd, workspaceKey: f.cwd },
        persistence: "immediate",
        titleGenerationEnabled: false,
      },
      (await import("@zcode/shared")).zcodeSessionStateSnapshotSchema,
    );
    const id = session.session.sessionId;
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "please summarize the repository layout" }));
    await h.completed(id);
    assert.equal(f.titleRequests.length, 0);
  } finally {
    await f.close();
  }
});
