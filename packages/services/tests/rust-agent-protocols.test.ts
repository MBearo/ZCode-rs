import assert from "node:assert/strict";
import test from "node:test";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fixture, type Harness } from "./rust-agent-fixture.js";
import type { ServerResponse } from "node:http";
type Message = Record<string, any>;
import { sse, responses, anthropic } from "./rust-agent-protocol-fixture.js";
async function turn(h: Harness, id: string, text: string) {
  const after = h.messages.length;
  await h.command(h.envelope("sendText", id, { text }));
  await h.completed(id, after);
}
for (const protocol of ["openai-responses", "anthropic-messages"] as const) {
  test(`Rust ${protocol} streams tools, preserves reasoning metadata and resumes through App schemas`, async () => {
    const f = await fixture({
      config: {
        apiType: protocol,
        apiKeyEnv: "RUST_FIXTURE_KEY",
        reasoningParameters:
          protocol === "openai-responses"
            ? { reasoning: { effort: "low", summary: "auto" } }
            : { thinking: { type: "enabled", budget_tokens: 1024 } },
      },
      env: { RUST_FIXTURE_KEY: "fixture-key" },
      respond(_req, res, attempt) {
        (protocol === "openai-responses" ? responses : anthropic)(res, attempt === 1);
      },
    });
    try {
      if (protocol === "anthropic-messages") {
        const config = JSON.parse(await readFile(f.config, "utf8"));
        config.baseUrl = config.baseUrl.replace(/\/v1$/, "");
        await writeFile(f.config, JSON.stringify(config));
      }
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await turn(h, id, "write");
      assert.equal(await readFile(join(f.cwd, "protocol.txt"), "utf8"), "native protocol");
      assert.equal(f.requests.length, 2);
      assert.equal(
        f.requestPaths[0],
        protocol === "openai-responses" ? "/v1/responses" : "/v1/messages",
      );
      const serialized = JSON.stringify(f.requests[1]);
      assert(
        serialized.includes(
          protocol === "openai-responses" ? "opaque-fixture" : "signature-fixture",
        ),
      );
      assert(
        serialized.includes(
          protocol === "openai-responses" ? "function_call_output" : "tool_result",
        ),
      );
      assert(!serialized.includes("_zcode_"));
      if (protocol === "openai-responses") {
        assert.equal(f.requests[0]!.store, false);
        assert.equal(f.requests[0]!.tools[0].type, "function");
        assert.equal(f.requestHeaders[0]!.authorization, "Bearer fixture-key");
      } else {
        assert.equal(f.requestHeaders[0]!["x-api-key"], "fixture-key");
        assert.equal(f.requestHeaders[0]!.authorization, "Bearer fixture-key");
        assert.equal(f.requestHeaders[0]!["anthropic-version"], "2023-06-01");
        assert.equal(f.requests[0]!.system.length, 3);
        assert.match(f.requests[0]!.system[0].text, /ZCode/);
        assert(
          f.requests[0]!.system.every((block: any) => block.cache_control.type === "ephemeral"),
        );
      }
      assert(!JSON.stringify(h.messages).includes("signature-fixture"));
      assert(!JSON.stringify(h.messages).includes("opaque-fixture"));
      await h.close();
      const resumed = f.start();
      await resumed.subscribe(`conversation/${id}`);
      await turn(resumed, id, "continue");
      assert(
        JSON.stringify(f.requests.at(-1)).includes(
          protocol === "openai-responses" ? "opaque-fixture" : "signature-fixture",
        ),
      );
      assert.deepEqual([...h.schemaErrors, ...resumed.schemaErrors], []);
    } finally {
      await f.close();
    }
  });
}

function prefix(res: ServerResponse, protocol: string, visible: boolean) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  if (protocol === "openai-responses") {
    sse(res, {
      type: "response.output_item.added",
      output_index: 0,
      item: visible
        ? { type: "message", id: "msg-partial", content: [] }
        : {
            type: "function_call",
            id: "fc-partial",
            call_id: "call-partial",
            name: "Write",
            arguments: "",
          },
    });
    sse(res, {
      type: visible ? "response.output_text.delta" : "response.function_call_arguments.delta",
      output_index: 0,
      item_id: visible ? "msg-partial" : "fc-partial",
      delta: visible ? "visible partial" : '{"file_path":',
    });
  } else {
    sse(res, { type: "message_start", message: { usage: { input_tokens: 2 } } });
    sse(res, {
      type: "content_block_start",
      index: 0,
      content_block: visible
        ? { type: "text", text: "" }
        : { type: "tool_use", id: "call-partial", name: "Write", input: {} },
    });
    sse(res, {
      type: "content_block_delta",
      index: 0,
      delta: visible
        ? { type: "text_delta", text: "visible partial" }
        : { type: "input_json_delta", partial_json: '{"file_path":' },
    });
  }
  res.end();
}
function frames(protocol: string, tool: boolean): Message[] {
  const events: Message[] = [];
  const capture = {
    writeHead() {},
    write(data: string) {
      events.push(JSON.parse(data.slice(6)));
      return true;
    },
    end() {},
  } as unknown as ServerResponse;
  (protocol === "openai-responses" ? responses : anthropic)(capture, tool);
  return events;
}
async function failure(h: Harness, id: string, after = 0) {
  return h.wait(
    (m) =>
      m.params?.topic === `conversation/${id}` &&
      m.params.frame?.payload?.deltas?.some((d: Message) => d.patch?.control?.phase === "error"),
    after,
  );
}
for (const protocol of ["openai-responses", "anthropic-messages"] as const) {
  const config = {
    apiType: protocol,
    reasoningParameters: {},
    retry: { maxRetries: 1, baseDelayMs: 1, maxDelayMs: 1, jitter: false },
  };
  test(`Rust ${protocol} retries only before visible output with identical encoded bytes`, async () => {
    for (const visible of [false, true]) {
      const f = await fixture({
        config,
        respond(_req, res, attempt) {
          if (attempt === 1) prefix(res, protocol, visible);
          else (protocol === "openai-responses" ? responses : anthropic)(res);
        },
      });
      try {
        const h = f.start();
        const id = await h.create();
        await h.subscribe(`conversation/${id}`);
        await h.command(h.envelope("sendText", id, { text: "stream interruption" }));
        if (visible) {
          await failure(h, id);
          assert.equal(f.requests.length, 1);
          assert(JSON.stringify((await h.rows(id)).rows).includes("visible partial"));
        } else {
          await h.completed(id);
          assert.equal(f.requests.length, 2);
          assert.equal(f.requestBodies[0], f.requestBodies[1]);
        }
        await assert.rejects(readFile(join(f.cwd, "protocol.txt")));
        assert.deepEqual(h.schemaErrors, []);
      } finally {
        await f.close();
      }
    }
  });
  test(`Rust ${protocol} rejects invalid tool termination before effects and compacts using the same protocol`, async () => {
    const f = await fixture({
      config,
      respond(_req, res) {
        const events = frames(protocol, true);
        if (protocol === "openai-responses")
          events.find(
            (e) => e.type === "response.output_item.done" && e.item.type === "function_call",
          )!.item.call_id = "wrong-call";
        else events.find((e) => e.type === "message_delta")!.delta.stop_reason = "end_turn";
        res.writeHead(200, { "content-type": "text/event-stream" });
        for (const e of events) sse(res, e);
        res.end();
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "invalid tools" }));
      await failure(h, id);
      await assert.rejects(readFile(join(f.cwd, "protocol.txt")));
      assert.equal(f.requests.length, 1);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
    const good = await fixture({
      config,
      respond(_req, res) {
        (protocol === "openai-responses" ? responses : anthropic)(res);
      },
    });
    try {
      const h = good.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await turn(h, id, "first");
      const after = h.messages.length;
      await h.command(h.envelope("compact", id));
      await h.completed(id, after);
      assert.equal(good.requests[1]!.tools, undefined);
      await turn(h, id, "after compact");
      assert(!JSON.stringify(good.requests[2]).includes("opaque-fixture"));
      assert(!JSON.stringify(good.requests[2]).includes("signature-fixture"));
      assert(JSON.stringify(good.requests[2]).includes("earlier conversation was compacted"));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await good.close();
    }
  });
}

for (const protocol of ["openai-responses", "anthropic-messages"] as const) {
  test(`Rust ${protocol} cancels a pending stream without dispatching tools`, async () => {
    let observed!: () => void;
    const requested = new Promise<void>((r) => {
      observed = r;
    });
    const f = await fixture({
      config: { apiType: protocol, reasoningParameters: {} },
      respond(_req, res) {
        res.writeHead(200, { "content-type": "text/event-stream" });
        res.flushHeaders();
        const events = frames(protocol, true);
        // 只发送工具参数前的事件，取消必须打断读流且不能提交未结束的调用。
        for (const e of events.slice(0, 2)) sse(res, e);
        observed();
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "cancel request" }));
      await requested;
      const at = performance.now();
      await h.command(h.envelope("stop", id));
      await h.wait((m) =>
        m.params?.frame?.payload?.deltas?.some(
          (d: Message) => d.patch?.control?.phase === "completedInterrupted",
        ),
      );
      assert(performance.now() - at < 500);
      assert.equal(f.requests.length, 1);
      await assert.rejects(readFile(join(f.cwd, "protocol.txt")));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  });
  test(`Rust ${protocol} applies the shared one-retry budget to empty completions`, async () => {
    const f = await fixture({
      config: {
        apiType: protocol,
        reasoningParameters: {},
        retry: { maxRetries: 5, baseDelayMs: 1, maxDelayMs: 1, jitter: false },
      },
      respond(_req, res) {
        res.writeHead(200, { "content-type": "text/event-stream" });
        if (protocol === "openai-responses")
          sse(res, { type: "response.completed", response: { status: "completed", output: [] } });
        else {
          sse(res, { type: "message_start", message: { usage: {} } });
          sse(res, { type: "message_delta", delta: { stop_reason: "end_turn" } });
          sse(res, { type: "message_stop" });
        }
        res.end();
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "empty request" }));
      await failure(h, id);
      assert.equal(f.requests.length, 2);
      assert.equal(f.requestBodies[0], f.requestBodies[1]);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  });
}

test("Rust Responses rejects an item still in progress even when its response says completed", async () => {
  const f = await fixture({
    config: { apiType: "openai-responses", reasoningParameters: {}, retry: { maxRetries: 0 } },
    respond(_req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      const events = frames("openai-responses", true);
      events.find(
        (e) => e.type === "response.output_item.done" && e.item.type === "function_call",
      )!.item.status = "in_progress";
      for (const e of events) sse(res, e);
      res.end();
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "contradictory terminal state" }));
    await failure(h, id);
    await assert.rejects(readFile(join(f.cwd, "protocol.txt")));
    assert.equal(f.requests.length, 1);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
