// 本地模型替身（docs/specs/rust-cli/export.md §6）：Anthropic Messages 的最小服务端，回复由剧本给出。
//
// 剧本驱动：主请求按序消费 `main` 里的响应；子代理与旁路请求（标题生成等）交给 `side`。
// 每个请求都记录 method、URL、原始 header 对（保留顺序与大小写）与原始 body 文本。
// 另提供一个只记录、不转发的代理 sink：被测进程的代理指向它，收到的请求数必须为 0。
// Node 互操作（node-interop-runtime-harness.mjs）与性能基准直接用它；别的脚本可以在它上面加自己的分类与计数。
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { EventEmitter, once } from "node:events";

/** 纯文本响应。 */
export const text = (value) => ({
  content: [{ type: "text", text: value }],
  stop_reason: "end_turn",
});

/** 单个 tool_use 响应。 */
export const toolUse = (id, name, input) => ({
  content: [{ type: "tool_use", id, name, input }],
  stop_reason: "tool_use",
});

/** thinking 块（带签名，回放时原样发回）：放进 `reply()` 的多块回复里。 */
export const thinking = (value, signature) => ({ type: "thinking", thinking: value, signature });

/** 文本块与 tool_use 块：放进 `reply()` 的多块回复里。 */
export const textBlock = (value) => ({ type: "text", text: value });
export const toolUseBlock = (id, name, input) => ({ type: "tool_use", id, name, input });

/**
 * 多块回复（thinking、文本与多个 tool_use 按给出的顺序）：有 tool_use 时 `stop_reason` 是 `tool_use`，
 * 否则 `end_turn`。
 */
export const reply = (...content) => ({
  content,
  stop_reason: content.some((block) => block.type === "tool_use") ? "tool_use" : "end_turn",
});

/**
 * API 错误响应（不可重试的 400 等），例如上下文超限：`apiError(400, "invalid_request_error", "prompt is too long: …")`。
 * `headers` 是额外的响应头（`x-should-retry`、`retry-after`）。
 */
export const apiError = (status, type, message, headers = {}) => ({
  error: { status, type, message, headers },
});

/**
 * 只发响应头（200、`text/event-stream`）就停住、不再发任何字节，直到客户端断开（流空闲看门狗的场景，
 * docs/specs/rust-cli/stall-visibility.md §7）。
 */
export const stall = () => ({ stall: true });

/** 同一个回复，换一个停止原因（`refusal`、`model_context_window_exceeded` 等，provider-errors.md §8.4）。 */
export const stopped = (reply, stopReason) => ({ ...reply, stop_reason: stopReason });

/**
 * 流式时只写前 `afterEvents` 个 SSE 事件，再写一个 `event: error`（`type` / `message`）后结束（流内失败的场景，
 * provider-errors.md §8.4）。
 */
export const streamError = (reply, afterEvents, type, message) => ({
  ...reply,
  streamError: { afterEvents, type, message },
});

/** 流式时只写前 `afterEvents` 个 SSE 事件，再直接断开连接（不发 `message_stop`）。 */
export const disconnect = (reply, afterEvents) => ({ ...reply, disconnect: { afterEvents } });

/** 压缩摘要提示的开头（rust-cli 的 `prompts/compact/` 逐字相同）。 */
export const COMPACT_PROMPT = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.";

/** 子代理的 system prompt 都带这一句（agents.md §4.3）。 */
export const SUBAGENT_AUTHORITY = "Messages from the agent that launched you";

/**
 * 请求类别 main / subagent / side：最后一条 user 消息带压缩摘要提示的是 side；否则 system 含子代理权限句的是
 * subagent；再否则带普通工具（没有 `type` 的工具定义）的是 main，其余是 side。场景可以传自己的 classify 覆盖。
 */
export function classifyRequest(_headers, body) {
  // 压缩的摘要请求与主请求同 system、同工具（compact.md §3），靠最后一条 user 消息里的摘要提示认出
  //（后面可能还接着 `role: system` 的提醒）。
  const lastUser = (body?.messages ?? []).findLast((message) => message?.role === "user");
  if (JSON.stringify(lastUser ?? "").includes(COMPACT_PROMPT)) return "side";
  const system = Array.isArray(body?.system) ? body.system : [body?.system];
  if (system.some((block) => String(block?.text ?? block ?? "").includes(SUBAGENT_AUTHORITY)))
    return "subagent";
  // 只带服务端工具（有 `type`，如 WebSearch 旁路请求的 web_search）的请求是旁路请求。
  const tools = Array.isArray(body?.tools) ? body.tools : [];
  return tools.some((tool) => tool?.type === undefined || tool.type === "custom") ? "main" : "side";
}

/** 续跑子代理时并进第一条消息的“留下的消息”（agents.md §5.1 第 9 条）的开头。 */
const STRANDED_MESSAGE = "The coordinator sent a message while you were working:";

/**
 * 子代理的第一条消息作分组键：只取文本，不看 cache_control（断点随历史增长后移，第一条消息只在第一个请求里
 * 带断点）与字符串 / 文本块的写法。用留下的消息续跑子代理时它被并进第一条消息（前一块补一个换行），分组时
 * 不看这一块，续跑请求与第一次请求仍是同一个子代理。
 */
export function threadKey(message) {
  const content = message?.content;
  const texts =
    typeof content === "string"
      ? [content]
      : Array.isArray(content)
        ? content.map((block) => block?.text ?? JSON.stringify(block))
        : [];
  if (texts.length > 1 && String(texts.at(-1)).startsWith(STRANDED_MESSAGE))
    texts.splice(-2, 2, String(texts.at(-2)).replace(/\n$/, ""));
  return JSON.stringify([message?.role, texts]);
}

/**
 * count_tokens 请求的键（`/context` 的计数）：工具名与第一条消息的第一行。`normalize` 先改写第一条消息的文字
 * （缺省不改）。只登记、不实现的 Workflow（tools-more.md §13）不进键。
 */
export function countKey(body = {}, normalize = (value) => value) {
  const tools = (Array.isArray(body?.tools) ? body.tools : [])
    .map((tool) => tool?.name)
    .filter((name) => name !== "Workflow")
    .join(",");
  const first = body?.messages?.[0]?.content;
  const value =
    typeof first === "string"
      ? first
      : Array.isArray(first)
        ? first.map((block) => (typeof block?.text === "string" ? block.text : "")).join("")
        : "";
  return `${tools}\u0000${normalize(value).trim().split("\n")[0]}`;
}

/** count_tokens 的回复：按键的哈希给固定值（100–9999），同一项计数每次拿到同一个数。 */
export function countTokens(body, key = countKey) {
  const digest = createHash("sha256").update(key(body)).digest();
  return 100 + (digest.readUInt32BE(0) % 9900);
}

const USAGE = {
  input_tokens: 10,
  cache_creation_input_tokens: 0,
  cache_read_input_tokens: 0,
  output_tokens: 3,
};

function listen(server) {
  server.listen(0, "127.0.0.1");
  return once(server, "listening").then(() => server.address().port);
}

function close(server) {
  server.closeAllConnections?.();
  return new Promise((resolveClose) => server.close(() => resolveClose()));
}

async function readBody(request) {
  // Buffer 边界可能切开多字节字符：按 UTF-8 流解码，不让替身制造 U+FFFD。
  request.setEncoding("utf8");
  let body = "";
  for await (const chunk of request) body += chunk;
  return body;
}

function headerMap(rawHeaders) {
  const map = {};
  for (let index = 0; index < rawHeaders.length; index += 2)
    map[rawHeaders[index].toLowerCase()] = rawHeaders[index + 1];
  return map;
}

function routeOf(method, pathname) {
  if (method === "POST" && pathname === "/v1/messages") return "messages";
  if (method === "POST" && pathname === "/v1/messages/count_tokens") return "count_tokens";
  if ((method === "HEAD" || method === "GET") && pathname === "/api/hello") return "hello";
  return "unknown";
}

function sendJson(response, status, requestId, value, headers = {}) {
  response.writeHead(status, {
    ...headers,
    "Content-Type": "application/json",
    "request-id": requestId,
  });
  response.end(JSON.stringify(value));
}

/** 内容块开始时的形状（Anthropic SSE）：文本与 thinking 是空的，tool_use 的 input 是空对象。 */
function blockStart(block) {
  if (block.type === "text") return { type: "text", text: "" };
  if (block.type === "thinking") return { type: "thinking", thinking: "" };
  return { ...block, input: {} };
}

/** 内容块的增量：文本一段、thinking 一段加签名（`thinking_delta`、`signature_delta`）、tool_use 一段 JSON。 */
function blockDeltas(block) {
  if (block.type === "text") return [{ type: "text_delta", text: block.text }];
  if (block.type === "thinking")
    return [
      { type: "thinking_delta", thinking: block.thinking },
      { type: "signature_delta", signature: block.signature },
    ];
  return [{ type: "input_json_delta", partial_json: JSON.stringify(block.input) }];
}

/**
 * Anthropic Messages 响应：stream:true 时按 SSE 事件序列，否则一次性 JSON。回复可带 `usage` 覆盖固定用量
 * （例如压缩场景让下一步的估算越过阈值），可带 `holdMs`（流式时最后的 message_delta / message_stop 晚这么久写）。
 */
function sendMessage(response, requestId, model, messageId, reply) {
  const message = { id: messageId, type: "message", role: "assistant", model };
  const usage = { ...USAGE, ...reply.usage };
  if (reply.stream === false) {
    sendJson(response, 200, requestId, {
      ...message,
      content: reply.content,
      stop_reason: reply.stop_reason,
      stop_sequence: null,
      usage,
    });
    return;
  }
  const events = [
    [
      "message_start",
      {
        message: {
          ...message,
          content: [],
          stop_reason: null,
          stop_sequence: null,
          usage: { ...usage, output_tokens: 1 },
        },
      },
    ],
    ...reply.content.flatMap((block, index) => [
      ["content_block_start", { index, content_block: blockStart(block) }],
      ...blockDeltas(block).map((delta) => ["content_block_delta", { index, delta }]),
      ["content_block_stop", { index }],
    ]),
    [
      "message_delta",
      {
        delta: { stop_reason: reply.stop_reason, stop_sequence: null },
        usage: { output_tokens: usage.output_tokens },
      },
    ],
    ["message_stop", {}],
  ];
  response.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    "request-id": requestId,
  });
  const frames = events.map(
    ([type, data]) => `event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`,
  );
  // 流内失败：前几个事件之后来一个错误事件，或者直接断开（provider-errors.md §8.4）。
  if (reply.streamError) {
    const { afterEvents, type, message: text } = reply.streamError;
    const error = { type: "error", error: { type, message: text } };
    response.end(
      frames.slice(0, afterEvents).join("") + `event: error\ndata: ${JSON.stringify(error)}\n\n`,
    );
    return;
  }
  if (reply.disconnect) {
    response.write(frames.slice(0, reply.disconnect.afterEvents).join(""), () =>
      response.socket?.destroy(),
    );
    return;
  }
  // `holdMs`：写到最后一个 content_block_stop 先发出去，等一会儿再收尾，造出“流还开着、工具已经完成”的时刻
  // （工作线 F81）。
  if (reply.holdMs > 0) {
    const tail = frames.splice(-2);
    response.write(frames.join(""));
    setTimeout(() => response.end(tail.join("")), reply.holdMs);
    return;
  }
  response.end(frames.join(""));
}

/**
 * 启动模型替身。
 * - main：主请求按序返回的响应（text() / toolUse()）。用完后再来的主请求回 400（不可重试），并记 overrun。
 *   响应可带 `before`（async () => void）：回复前先执行，用于在两次工具调用之间从外部改动工作区文件；
 *   可带 `resolve`（(body) => 响应）：按这次请求的 body 生成响应（例如从提醒里取计划文件路径）。
 * - side：(record, body) => 响应，回复 subagent 与 side 两类请求；默认回一段固定文本。
 * - classify：(headers, body) => "main" | "subagent" | "side"。
 * - countTokens：(body) => count_tokens 回的数。
 */
export async function startModelFixture({
  main = [],
  side = () => text("Fixture side reply"),
  classify = classifyRequest,
  countTokens: count = countTokens,
} = {}) {
  const records = [];
  // 请求与回复的事件（TUI 的 PTY 驱动按它们同步）：`request` 在记下请求时，`reply` 在这个请求的
  // 响应写完时（`finish`），参数都是记录本身。
  const events = new EventEmitter();
  let mainCursor = 0;
  // 消息 ID 按类别编号，子代理再按“哪个子代理”分组：子代理与主线程交错到达时 ID 不变，请求里引用的上一条
  // 消息 ID 与 stream-json 的消息 ID 因此稳定。
  const messageCounters = new Map();
  const subagentThreads = new Map();
  const messageId = (record, body) => {
    let group = record.class;
    if (group === "subagent") {
      const key = threadKey(body?.messages?.[0]);
      if (!subagentThreads.has(key)) subagentThreads.set(key, subagentThreads.size + 1);
      group = `subagent${subagentThreads.get(key)}`;
    }
    const index = (messageCounters.get(group) ?? 0) + 1;
    messageCounters.set(group, index);
    return `msg_fixture_${group}_${String(index).padStart(4, "0")}`;
  };
  const server = createServer(async (request, response) => {
    const body = await readBody(request);
    const url = new URL(request.url ?? "/", "http://fixture.invalid");
    const route = routeOf(request.method, url.pathname);
    const record = {
      seq: records.length + 1,
      route,
      method: request.method,
      url: request.url,
      rawHeaders: [...request.rawHeaders],
      body,
    };
    records.push(record);
    response.once("finish", () => {
      record.replied = true;
      events.emit("reply", record);
    });
    events.emit("request", record);
    const requestId = `req_fixture_${String(record.seq).padStart(4, "0")}`;
    if (route === "hello") {
      response.writeHead(200, { "Content-Type": "application/json", "request-id": requestId });
      response.end(request.method === "HEAD" ? undefined : JSON.stringify({ message: "hello" }));
      return;
    }
    if (route === "unknown") {
      sendJson(response, 404, requestId, {
        type: "error",
        error: {
          type: "not_found_error",
          message: `fixture: no route ${request.method} ${url.pathname}`,
        },
      });
      return;
    }
    let value;
    try {
      value = JSON.parse(body);
    } catch {
      record.invalidJson = true;
      sendJson(response, 400, requestId, {
        type: "error",
        error: { type: "invalid_request_error", message: "fixture: body is not JSON" },
      });
      return;
    }
    if (route === "count_tokens") {
      sendJson(response, 200, requestId, { input_tokens: count(value) });
      return;
    }
    const headers = headerMap(request.rawHeaders);
    record.class = classify(headers, value);
    let reply;
    if (record.class === "main") {
      record.mainIndex = mainCursor + 1;
      reply = main[mainCursor++];
      if (!reply) {
        record.overrun = true;
        sendJson(response, 400, requestId, {
          type: "error",
          error: { type: "invalid_request_error", message: "fixture: main script exhausted" },
        });
        return;
      }
    } else {
      reply = side(record, value);
    }
    try {
      if (typeof reply.before === "function") await reply.before();
      if (typeof reply.resolve === "function")
        reply = { ...reply.resolve(value), ...(reply.usage && { usage: reply.usage }) };
    } catch (error) {
      // 剧本按请求生成回复失败（例如请求里没有剧本要找的 ID）：记下原因，回不可重试的 400，
      // 让这次运行照常结束、由比较报出差异，而不是让整个进程崩溃。
      record.scriptError = String(error?.message ?? error);
      sendJson(response, 400, requestId, {
        type: "error",
        error: { type: "invalid_request_error", message: `fixture: ${record.scriptError}` },
      });
      return;
    }
    if (reply.stall) {
      response.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        "request-id": requestId,
      });
      response.flushHeaders();
      return;
    }
    if (reply.error) {
      sendJson(
        response,
        reply.error.status,
        requestId,
        { type: "error", error: { type: reply.error.type, message: reply.error.message } },
        reply.error.headers,
      );
      return;
    }
    const stream = value.stream === true ? reply : { ...reply, stream: false };
    sendMessage(response, requestId, value.model, messageId(record, value), stream);
  });
  const port = await listen(server);
  return {
    port,
    // 客户端自己拼 `/v1/messages`，所以基地址不带 `/v1`。
    baseUrl: `http://127.0.0.1:${port}`,
    records,
    events,
    close: () => close(server),
  };
}

/**
 * 只记录、不转发的代理 sink。普通代理请求回 502（正文是 `body`），CONNECT 回 403 后断开。
 * 只记 method、目标与 host，不记 body。
 */
export async function startSink({ body = "fixture sink: not forwarded" } = {}) {
  const hits = [];
  const server = createServer((request, response) => {
    hits.push({ method: request.method, target: request.url, host: request.headers.host });
    request.resume();
    response.writeHead(502, { "Content-Type": "text/plain" }).end(body);
  });
  server.on("connect", (request, socket) => {
    hits.push({ method: "CONNECT", target: request.url, host: request.headers.host });
    socket.end("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
  });
  const port = await listen(server);
  return { port, url: `http://127.0.0.1:${port}`, hits, close: () => close(server) };
}
