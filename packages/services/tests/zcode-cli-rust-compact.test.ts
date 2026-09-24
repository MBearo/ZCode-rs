import assert from "node:assert/strict";
import test from "node:test";
import { realpath, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ServerResponse } from "node:http";
import { fixture, event, end, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
/** Node 的摘要请求：末条 user 消息是 buildCompactPrompt 的全文。 */
const summaryRequest = (req: Message) =>
  String(req.messages.at(-1).content).startsWith("CRITICAL: Respond with TEXT ONLY");
function reply(res: ServerResponse, content: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content });
  end(res, "stop");
}
function tooLong(res: ServerResponse, message: string) {
  res.writeHead(400, { "content-type": "application/json" });
  res.end(JSON.stringify({ error: { code: "context_length_exceeded", message } }));
}
function call(res: ServerResponse, name: string, input: Message) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, {
    tool_calls: [
      {
        index: 0,
        id: `call-${name}`,
        type: "function",
        function: { name, arguments: JSON.stringify(input) },
      },
    ],
  });
  end(res, "tool_calls");
}
async function send(h: Harness, id: string, text: string) {
  const after = h.messages.length;
  await h.command(h.envelope("sendText", id, { text }));
  await h.completed(id, after);
}
const users = (req: Message): string[] =>
  req.messages.filter((m: Message) => m.role === "user").map((m: Message) => String(m.content));

test("Rust reactive compaction preserves more recent rounds when the summary is too long like Node", async () => {
  let turns = 0;
  const summaries: Message[] = [];
  const f = await fixture({
    respond(req, res) {
      if (summaryRequest(req)) {
        summaries.push(req);
        if (summaries.length === 1)
          return tooLong(res, "prompt is too long: 205000 tokens > 200000 maximum");
        return reply(res, "durable summary");
      }
      turns += 1;
      if (turns === 5) return tooLong(res, "prompt is too long: 201000 tokens > 200000 maximum");
      reply(res, `answer ${turns} ${"x".repeat(3000)}`);
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    for (const text of ["one", "two", "three", "four", "five"]) await send(h, id, text);
    assert.equal(summaries.length, 2);
    const kept = (req: Message) =>
      users(req).filter((u) => /^(one|two|three|four)$/.test(u)).length;
    assert.ok(kept(summaries[1]!) < kept(summaries[0]!), "the retry summarizes fewer rounds");
    const last = f.requests.at(-1)!;
    assert.equal(last.messages.at(-1).content, "five");
    assert.ok(users(last).some((u) => u.startsWith("This session is being continued")));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust manual compaction drops the oldest rounds when the summary is too long like Node", async () => {
  const summaries: Message[] = [];
  const f = await fixture({
    respond(req, res) {
      if (summaryRequest(req)) {
        summaries.push(req);
        if (summaries.length === 1) return tooLong(res, "context window exceeded");
        return reply(res, "durable summary");
      }
      reply(res, "answer");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    for (const text of ["one", "two", "three"]) await send(h, id, text);
    const after = h.messages.length;
    await h.command(h.envelope("compact", id));
    await h.completed(id, after);
    assert.equal(summaries.length, 2);
    assert.ok(users(summaries[0]!).includes("one"));
    const retry = users(summaries[1]!);
    assert.ok(!retry.includes("one"), "the oldest round went");
    assert.ok(retry.includes("[earlier conversation truncated for compaction retry]"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust re-attaches recently read files after compaction and forgets the read state like Node", async () => {
  let step = 0;
  const f = await fixture({
    respond(req, res) {
      if (summaryRequest(req)) return reply(res, "durable summary");
      step += 1;
      if (step === 1) return call(res, "Read", { file_path: "a.txt" });
      if (step === 3)
        return call(res, "Edit", { file_path: "a.txt", old_string: "hello", new_string: "bye" });
      reply(res, "done");
    },
  });
  try {
    await writeFile(join(f.cwd, "a.txt"), "hello\nworld\n");
    const path = join(await realpath(f.cwd), "a.txt");
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "read it");
    const after = h.messages.length;
    await h.command(h.envelope("compact", id));
    await h.completed(id, after);
    await send(h, id, "edit it");
    const request = f.requests.find((r) => users(r).includes("edit it"))!;
    const reminder = users(request).find((u) => u.includes("Called the Read tool"));
    assert.equal(
      reminder,
      [
        "<system-reminder>",
        `Called the Read tool with the following input: ${JSON.stringify({ file_path: path })}`,
        "Result of calling the Read tool:",
        "1\thello",
        "2\tworld",
        // 与 Read 输出相同：文件末尾换行留下一个空的第 3 行。
        "3\t",
        "</system-reminder>",
      ].join("\n"),
    );
    const edited = f.requests.at(-1)!.messages.find((m: Message) => m.role === "tool");
    assert.match(String(edited.content), /has not been read yet/);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
