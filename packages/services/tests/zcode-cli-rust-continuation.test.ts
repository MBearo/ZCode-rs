import assert from "node:assert/strict";
import test from "node:test";
import type { ServerResponse } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
const protocols = ["openai-chat-completions", "openai-responses", "anthropic-messages"];
const continuePrompt = "Output token limit hit. Resume directly";
function response(
  protocol: string,
  res: ServerResponse,
  text: string,
  limited: boolean,
  tool = false,
  inputTokens = 5,
) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  const send = (value: Message) => res.write(`data: ${JSON.stringify(value)}\n\n`);
  const args = JSON.stringify({ file_path: "truncated.txt", content: "never execute" });
  if (protocol === "openai-chat-completions") {
    send({
      choices: [
        {
          delta: tool
            ? {
                tool_calls: [
                  {
                    index: 0,
                    id: "call",
                    type: "function",
                    function: { name: "Write", arguments: args },
                  },
                ],
              }
            : { content: text },
        },
      ],
    });
    send({
      choices: [{ delta: {}, finish_reason: limited ? "length" : "stop" }],
      usage: { prompt_tokens: inputTokens, completion_tokens: 3 },
    });
    res.write("data: [DONE]\n\n");
  } else if (protocol === "openai-responses") {
    const item = tool
      ? { type: "function_call", id: "fc", call_id: "call", name: "Write", arguments: args }
      : {
          type: "message",
          id: "msg",
          role: "assistant",
          content: [{ type: "output_text", text }],
          status: limited ? "incomplete" : "completed",
        };
    send({
      type: "response.output_item.added",
      output_index: 0,
      item: tool ? { ...item, arguments: "" } : { ...item, content: [], status: "in_progress" },
    });
    send({
      type: tool ? "response.function_call_arguments.delta" : "response.output_text.delta",
      item_id: item.id,
      output_index: 0,
      delta: tool ? args : text,
    });
    send({ type: "response.output_item.done", output_index: 0, item });
    send({
      type: limited ? "response.incomplete" : "response.completed",
      response: {
        status: limited ? "incomplete" : "completed",
        output: [item],
        incomplete_details: limited ? { reason: "max_output_tokens" } : null,
        usage: { input_tokens: inputTokens, output_tokens: 3 },
      },
    });
  } else {
    send({ type: "message_start", message: { usage: { input_tokens: inputTokens } } });
    send({
      type: "content_block_start",
      index: 0,
      content_block: tool
        ? { type: "tool_use", id: "call", name: "Write", input: {} }
        : { type: "text", text: "" },
    });
    send({
      type: "content_block_delta",
      index: 0,
      delta: tool ? { type: "input_json_delta", partial_json: args } : { type: "text_delta", text },
    });
    send({ type: "content_block_stop", index: 0 });
    send({
      type: "message_delta",
      delta: { stop_reason: limited ? "max_tokens" : "end_turn" },
      usage: { output_tokens: 3 },
    });
    send({ type: "message_stop" });
  }
  res.end();
}
async function send(h: Harness, id: string, text: string) {
  const after = h.messages.length;
  await h.command(h.envelope("sendText", id, { text }));
  await h.completed(id, after);
}
async function failed(h: Harness, id: string) {
  return h.wait(
    (m) =>
      m.params?.topic === `conversation/${id}` &&
      m.params.frame?.payload?.deltas?.some((d: Message) => d.patch?.control?.phase === "error"),
  );
}
for (const protocol of protocols) {
  const config = { apiType: protocol, reasoningParameters: {}, retry: { maxRetries: 0 } };
  test(`Rust ${protocol} continues committed partial output without persisting synthetic user input`, async () => {
    const f = await fixture({
      config,
      respond(_req, res, attempt) {
        response(protocol, res, `part ${attempt}`, attempt <= 2);
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await send(h, id, "long response");
      assert.equal(f.requests.length, 3);
      assert(JSON.stringify(f.requests[1]).includes("part 1"));
      assert(JSON.stringify(f.requests[1]).includes(continuePrompt));
      const rows = (await h.rows(id)).rows;
      assert.equal(rows.filter((r: Message) => r.kind === "userInput").length, 1);
      assert(!JSON.stringify(rows).includes(continuePrompt));
      await send(h, id, "ordinary next input");
      assert(!JSON.stringify(f.requests[3]).includes(continuePrompt));
      assert(JSON.stringify(f.requests[3]).includes("part 1"));
      await h.close();
      const resumed = f.start();
      await resumed.subscribe(`conversation/${id}`);
      await send(resumed, id, "cold next input");
      assert(!JSON.stringify(f.requests[4]).includes(continuePrompt));
      assert.deepEqual([...h.schemaErrors, ...resumed.schemaErrors], []);
    } finally {
      await f.close();
    }
  });
  test(`Rust ${protocol} caps empty output-limit recovery at three continuations`, async () => {
    const f = await fixture({
      config,
      respond(_req, res, attempt) {
        response(protocol, res, attempt <= 4 ? "" : "recovered", attempt <= 4);
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "empty capped output" }));
      const error = await failed(h, id);
      assert(JSON.stringify(error).includes("model_output_limit_exceeded"));
      assert.equal(f.requests.length, 4);
      await send(h, id, "recover with new user input");
      assert.equal(f.requests.length, 5);
      assert(!JSON.stringify(f.requests[4]).includes(continuePrompt));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  });
  test(`Rust ${protocol} does not execute tools from an output-limit response`, async () => {
    const f = await fixture({
      config,
      respond(_req, res) {
        response(protocol, res, "", true, true);
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "truncated tool" }));
      await failed(h, id);
      assert.equal(f.requests.length, 1);
      await assert.rejects(readFile(join(f.cwd, "truncated.txt")));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  });
}

test("Rust continuation survives auto compact without including temporary prompts in the canonical boundary", async () => {
  const protocol = "openai-chat-completions";
  const f = await fixture({
    config: { contextWindow: 40000, maxOutputTokens: 1000, contextBufferTokens: 1000 },
    respond(_req, res, attempt) {
      response(
        protocol,
        res,
        attempt === 2 ? "partial retained" : "short complete summary",
        attempt === 2,
        false,
        attempt === 2 ? 50000 : 5,
      );
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "older input ".repeat(1000));
    await send(h, id, "continue current task");
    assert.equal(f.requests.length, 4);
    assert.equal(f.requests[2]!.tools, undefined);
    assert(JSON.stringify(f.requests[3]).includes("partial retained"));
    assert(JSON.stringify(f.requests[3]).includes(continuePrompt));
    assert(!JSON.stringify(f.requests[3]).includes("older input ".repeat(1000)));
    await h.close();
    const resumed = f.start();
    await resumed.subscribe(`conversation/${id}`);
    await send(resumed, id, "cold input after compact");
    assert.equal(f.requests.length, 5);
    assert(JSON.stringify(f.requests[4]).includes("partial retained"));
    assert(!JSON.stringify(f.requests[4]).includes(continuePrompt));
    assert.deepEqual([...h.schemaErrors, ...resumed.schemaErrors], []);
  } finally {
    await f.close();
  }
});

test("Rust never commits a truncated compact summary or starts a continuation for it", async () => {
  const f = await fixture({
    respond(_req, res, attempt) {
      response(
        "openai-chat-completions",
        res,
        attempt === 2 ? "cut summary" : "answer",
        attempt === 2,
      );
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "preserve original context");
    await h.command(h.envelope("compact", id));
    await failed(h, id);
    assert.equal(f.requests.length, 2);
    await send(h, id, "continue after failed compact");
    assert.equal(f.requests.length, 3);
    assert(JSON.stringify(f.requests[2]).includes("preserve original context"));
    assert(!JSON.stringify(f.requests[2]).includes("cut summary"));
    assert(!JSON.stringify(f.requests[2]).includes(continuePrompt));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
