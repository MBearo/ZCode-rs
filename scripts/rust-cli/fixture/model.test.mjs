// 本地模型替身的测试（docs/specs/rust-cli/export.md §6）：
//   node --test scripts/rust-cli/fixture/model.test.mjs
// 缺省分类只看 system 与工具；count_tokens 的键缺省不改写文字；剧本按序回复，用完回 400。
import assert from "node:assert/strict";
import test from "node:test";

import {
  COMPACT_PROMPT,
  SUBAGENT_AUTHORITY,
  classifyRequest,
  countKey,
  countTokens,
  startModelFixture,
  startSink,
  text,
} from "./model.mjs";

test("缺省分类：压缩提示是 side，子代理权限句是 subagent，带普通工具是 main，请求头不看", () => {
  const tools = [{ name: "Read" }];
  assert.equal(classifyRequest({}, { tools }), "main");
  assert.equal(classifyRequest({ "x-request-class": "side" }, { tools }), "main");
  assert.equal(
    classifyRequest({}, { tools: [{ type: "web_search_20250305", name: "web_search" }] }),
    "side",
  );
  assert.equal(
    classifyRequest({}, { system: [{ text: `x ${SUBAGENT_AUTHORITY}` }], tools }),
    "subagent",
  );
  const compact = {
    messages: [{ role: "user", content: [{ type: "text", text: COMPACT_PROMPT }] }],
    tools,
  };
  assert.equal(classifyRequest({}, compact), "side");
});

test("count_tokens：键是工具名与第一行，缺省不改写；给了 normalize 就先改写", () => {
  const body = {
    tools: [{ name: "Read" }, { name: "Workflow" }],
    messages: [{ role: "user", content: "A b\nrest" }],
  };
  assert.equal(countKey(body), "Read\u0000A b");
  assert.equal(
    countKey(body, (value) => value.replace("A", "Z")),
    "Read\u0000Z b",
  );
  const value = countTokens(body);
  assert.ok(value >= 100 && value < 10000);
  assert.equal(countTokens(body), value);
});

test("剧本按序回复，用完回 400；count_tokens 用给出的计数；sink 不转发", async (t) => {
  const fixture = await startModelFixture({ main: [text("one")], countTokens: () => 42 });
  const sink = await startSink();
  t.after(() => Promise.all([fixture.close(), sink.close()]));
  const post = (path, body) =>
    fetch(`${fixture.baseUrl}${path}`, { method: "POST", body: JSON.stringify(body) });
  const body = { model: "m", tools: [{ name: "Read" }], messages: [] };
  const first = await (await post("/v1/messages", body)).json();
  assert.equal(first.content[0].text, "one");
  assert.equal(first.id, "msg_fixture_main_0001");
  assert.equal((await post("/v1/messages", body)).status, 400);
  assert.equal(fixture.records.at(-1).overrun, true);
  assert.deepEqual(await (await post("/v1/messages/count_tokens", body)).json(), {
    input_tokens: 42,
  });
  const proxied = await fetch(sink.url);
  assert.equal(proxied.status, 502);
  assert.equal(await proxied.text(), "fixture sink: not forwarded");
});
