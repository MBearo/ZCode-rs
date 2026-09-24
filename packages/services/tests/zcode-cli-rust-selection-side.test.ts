import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { fixture } from "./zcode-cli-rust-fixture.js";

// Node `createSelectionSideSession`：从父会话已落库的活动对话建立副屏（原子 fork 包），
// 继承历史只给模型参考，副屏从空白开始；首条输入只进入子会话。
test("Selection side chat inherits hidden parent history, runs its first input alone and survives restart", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const parent = await h.create();
    await h.subscribe(`conversation/${parent}`);
    await h.command(h.envelope("sendText", parent, { text: "parent question" }));
    await h.completed(parent);
    const before = (await h.rows(parent)).rows.length;
    const command = h.envelope("createSelectionSideSession", parent, {
      firstInput: { text: "explain the selection" },
    });
    const ack = await h.command(command);
    assert.equal(ack.status, "accepted");
    const result = ack.result as any;
    assert.equal(result.type, "createSelectionSideSession");
    assert.deepEqual(result.input, { delivery: "startNow", inputId: command.commandId });
    const child = result.sessionId as string;
    await h.subscribe(`conversation/${child}`);
    await h.completed(child);
    const request = f.requests.at(-1)!.messages;
    const texts = request.map((m: any) => (typeof m.content === "string" ? m.content : ""));
    const inherited = texts.indexOf("parent question");
    const boundary = texts.findIndex((t: string) => t.includes("inherited from the parent task"));
    assert(inherited > 0 && boundary > inherited, JSON.stringify(texts));
    assert.equal(texts.at(-1), "explain the selection");
    // 副屏只显示自己的输入与回答；父会话不变。
    const rows = (await h.rows(child)).rows;
    assert.deepEqual(
      rows.filter((r) => r.kind === "userInput").map((r: any) => r.text),
      ["explain the selection"],
    );
    assert.equal((await h.rows(parent)).rows.length, before);
    assert.equal((await h.command(command)).status, "duplicate");
    const db = new DatabaseSync(f.db, { readOnly: true });
    const row = db.prepare("SELECT task_type, parent_id, title FROM session WHERE id=?").get(child);
    db.close();
    assert.deepEqual(
      { ...row },
      {
        task_type: "selection_side_chat",
        parent_id: parent,
        title: "Selection side chat",
      },
    );
    await h.close();
    const cold = f.start();
    await cold.subscribe(`conversation/${child}`);
    assert.deepEqual(
      (await cold.rows(child)).rows.filter((r) => r.kind === "userInput").map((r: any) => r.text),
      ["explain the selection"],
    );
    // 重启后回执从父会话的命令事实反查（Node `v4_command_fact:child`）。
    const replay = await cold.command(command);
    assert.equal(replay.status, "duplicate");
    assert.equal((replay.result as any).sessionId, child);
    await cold.command(cold.envelope("sendText", child, { text: "follow up" }));
    await cold.completed(child);
    assert.match(JSON.stringify(f.requests.at(-1)!.messages), /parent question/);
    assert.deepEqual([...h.schemaErrors, ...cold.schemaErrors], []);
  } finally {
    await f.close();
  }
});

test("Selection side chat without a first input stays empty and leaves a running parent untouched", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const parent = await h.create();
    await h.subscribe(`conversation/${parent}`);
    await h.command(h.envelope("sendText", parent, { text: "slow" }));
    const ack = await h.command(h.envelope("createSelectionSideSession", parent, {}));
    assert.equal(ack.status, "accepted");
    const child = (ack.result as any).sessionId as string;
    assert.equal((ack.result as any).input, undefined);
    await h.subscribe(`conversation/${child}`);
    assert.deepEqual((await h.rows(child)).rows, []);
    await h.completed(parent);
    assert.equal(f.requests.length, 1);
    // 父会话运行中：只继承已提交的本轮输入。
    await h.command(h.envelope("sendText", child, { text: "side question" }));
    await h.completed(child);
    const texts = JSON.stringify(f.requests.at(-1)!.messages);
    assert.match(texts, /"slow"/);
    assert.doesNotMatch(texts, /你好 Rust/);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
