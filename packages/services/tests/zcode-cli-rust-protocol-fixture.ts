import type { ServerResponse } from "node:http";
type Message = Record<string, any>;
export function sse(res: ServerResponse, value: Message) {
  res.write(`data: ${JSON.stringify(value)}\n\n`);
}
const input = JSON.stringify({ file_path: "protocol.txt", content: "native protocol" });
export function responses(res: ServerResponse, tool = false) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  sse(res, { type: "response.created", response: { id: "resp-1" } });
  const reasoning = {
    type: "reasoning",
    id: "rs-1",
    summary: [{ type: "summary_text", text: "推理" }],
    encrypted_content: "opaque-fixture",
  };
  sse(res, {
    type: "response.output_item.added",
    output_index: 0,
    item: { type: "reasoning", id: "rs-1", summary: [] },
  });
  sse(res, {
    type: "response.reasoning_summary_text.delta",
    output_index: 0,
    item_id: "rs-1",
    delta: "推理",
  });
  sse(res, { type: "response.output_item.done", output_index: 0, item: reasoning });
  const item = tool
    ? {
        type: "function_call",
        id: "fc-1",
        call_id: "call-1",
        name: "Write",
        arguments: input,
        status: "completed",
      }
    : {
        type: "message",
        id: "msg-1",
        role: "assistant",
        status: "completed",
        content: [{ type: "output_text", text: "protocol answer", annotations: [] }],
      };
  sse(res, {
    type: "response.output_item.added",
    output_index: 1,
    item: tool
      ? { ...item, arguments: "", status: "in_progress" }
      : { ...item, content: [], status: "in_progress" },
  });
  if (tool) {
    for (const delta of [input.slice(0, 15), input.slice(15)])
      sse(res, {
        type: "response.function_call_arguments.delta",
        output_index: 1,
        item_id: "fc-1",
        delta,
      });
    sse(res, {
      type: "response.function_call_arguments.done",
      output_index: 1,
      item_id: "fc-1",
      arguments: input,
    });
  } else
    sse(res, {
      type: "response.output_text.delta",
      output_index: 1,
      item_id: "msg-1",
      content_index: 0,
      delta: "protocol answer",
    });
  sse(res, { type: "response.output_item.done", output_index: 1, item });
  sse(res, {
    type: "response.completed",
    response: {
      id: "resp-1",
      status: "completed",
      output: [reasoning, item],
      usage: { input_tokens: 12, output_tokens: 6, input_tokens_details: { cached_tokens: 3 } },
    },
  });
  res.end();
}
export function anthropic(res: ServerResponse, tool = false) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  sse(res, {
    type: "message_start",
    message: {
      id: "msg-1",
      role: "assistant",
      content: [],
      usage: { input_tokens: 12, output_tokens: 1, cache_read_input_tokens: 3 },
    },
  });
  sse(res, {
    type: "content_block_start",
    index: 0,
    content_block: { type: "thinking", thinking: "", signature: "" },
  });
  sse(res, {
    type: "content_block_delta",
    index: 0,
    delta: { type: "thinking_delta", thinking: "推理" },
  });
  sse(res, {
    type: "content_block_delta",
    index: 0,
    delta: { type: "signature_delta", signature: "signature-fixture" },
  });
  sse(res, { type: "content_block_stop", index: 0 });
  sse(res, {
    type: "content_block_start",
    index: 1,
    content_block: tool
      ? { type: "tool_use", id: "call-1", name: "Write", input: {} }
      : { type: "text", text: "" },
  });
  if (tool)
    for (const partial_json of [input.slice(0, 15), input.slice(15)])
      sse(res, {
        type: "content_block_delta",
        index: 1,
        delta: { type: "input_json_delta", partial_json },
      });
  else
    sse(res, {
      type: "content_block_delta",
      index: 1,
      delta: { type: "text_delta", text: "protocol answer" },
    });
  sse(res, { type: "content_block_stop", index: 1 });
  sse(res, {
    type: "message_delta",
    delta: { stop_reason: tool ? "tool_use" : "end_turn" },
    usage: { output_tokens: 6 },
  });
  sse(res, { type: "message_stop" });
  res.end();
}
