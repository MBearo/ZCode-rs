import assert from "node:assert/strict";
import test from "node:test";
import type { ServerResponse } from "node:http";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { sse } from "./zcode-cli-rust-protocol-fixture.js";

type Message = Record<string, any>;
const SEARCH_SYSTEM = "You are an assistant for performing a web search tool use.";

/** One Anthropic message with the given content blocks (`[block, deltas]`). */
function message(res: ServerResponse, blocks: [Message, Message[]][], stop: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  sse(res, {
    type: "message_start",
    message: {
      id: "m",
      role: "assistant",
      content: [],
      usage: { input_tokens: 9, output_tokens: 1 },
    },
  });
  blocks.forEach(([block, deltas], index) => {
    sse(res, { type: "content_block_start", index, content_block: block });
    for (const delta of deltas) sse(res, { type: "content_block_delta", index, delta });
    sse(res, { type: "content_block_stop", index });
  });
  sse(res, { type: "message_delta", delta: { stop_reason: stop }, usage: { output_tokens: 4 } });
  sse(res, { type: "message_stop" });
  res.end();
}
const text = (value: string): [Message, Message[]] => [
  { type: "text", text: "" },
  [{ type: "text_delta", text: value }],
];

async function run(native: boolean) {
  const searches: { body: Message; headers: Message }[] = [];
  const f = await fixture({
    config: {
      apiType: "anthropic-messages",
      reasoningParameters: {},
      supportsNativeWebSearch: native,
    },
    respond(req, res) {
      const system = JSON.stringify(req.system ?? "");
      if (system.includes(SEARCH_SYSTEM)) {
        searches.push({ body: req, headers: f.requestHeaders.at(-1)! });
        message(
          res,
          [
            [
              { type: "server_tool_use", id: "srv", name: "web_search", input: {} },
              [{ type: "input_json_delta", partial_json: '{"query":"rust async"}' }],
            ],
            [{ type: "web_search_tool_result", tool_use_id: "srv", content: [] }, []],
            [
              { type: "text", text: "" },
              [
                { type: "text_delta", text: "Use [Tokio](https://tokio.rs) " },
                { type: "citations_delta", citation: { type: "web_search_result_location" } },
                { type: "text_delta", text: "and ![logo](https://img.test/x.png)." },
              ],
            ],
          ],
          "end_turn",
        );
        return;
      }
      const last = req.messages.at(-1);
      const results = Array.isArray(last.content)
        ? last.content.filter((part: Message) => part.type === "tool_result")
        : [];
      if (results.length > 0) {
        message(res, [text("answered")], "end_turn");
        return;
      }
      message(
        res,
        [
          [
            { type: "tool_use", id: "call-search", name: "WebSearch", input: {} },
            [
              {
                type: "input_json_delta",
                partial_json: JSON.stringify({ query: "rust async", blocked_domains: ["x.com"] }),
              },
            ],
          ],
        ],
        "tool_use",
      );
    },
  });
  return { f, searches };
}

test("Rust WebSearch runs a provider-native search and returns Node's text", async () => {
  const { f, searches } = await run(true);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "search" }));
    await h.completed(id, after);
    const first = f.requests[0]!;
    const offered = first.tools.find((t: Message) => t.name === "WebSearch");
    assert.match(offered.description, /The current month is [A-Z][a-z]+ \d{4} — /);
    assert.equal(searches.length, 1);
    const search = searches[0]!;
    assert.equal(search.headers["anthropic-beta"], "code-execution-web-tools-2026-02-09");
    assert.deepEqual(search.body.tools, [
      { type: "web_search_20260209", name: "web_search", max_uses: 8, blocked_domains: ["x.com"] },
    ]);
    assert.ok(search.body.max_tokens <= 4096);
    assert.deepEqual(search.body.messages.at(-1).content.at(-1), {
      type: "text",
      text: "Perform a web search for the query: rust async",
    });
    const final = f.requests.at(-1)!;
    const toolResult = final.messages
      .at(-1)
      .content.find((part: Message) => part.type === "tool_result");
    const content =
      typeof toolResult.content === "string" ? toolResult.content : toolResult.content[0].text;
    assert.equal(
      content,
      [
        'Web search results for query: "rust async"',
        "",
        "Summary:",
        "Use [Tokio](https://tokio.rs) and ![logo](https://img.test/x.png).",
        "",
        "Links:",
        "- [Tokio](https://tokio.rs)",
        "",
        "REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.",
      ].join("\n"),
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust hides WebSearch from models without native search", async () => {
  const { f } = await run(false);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "search" }));
    await h.completed(id, after);
    const first = f.requests[0]!;
    assert.ok(!first.tools.some((t: Message) => t.name === "WebSearch"));
    assert.ok(first.tools.some((t: Message) => t.name === "WebFetch"));
    // 模型仍调用 WebSearch 时按不支持失败（Node handler 的可恢复错误）。
    const toolResult = f.requests
      .at(-1)!
      .messages.at(-1)
      .content.find((part: Message) => part.type === "tool_result");
    const content =
      typeof toolResult.content === "string" ? toolResult.content : toolResult.content[0].text;
    assert.equal(content, "Current model does not support native WebSearch");
  } finally {
    await f.close();
  }
});
