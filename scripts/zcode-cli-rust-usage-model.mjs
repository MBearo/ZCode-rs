// The local Anthropic Messages model of the usage differential check
// (`zcode-cli-rust-usage-diff.mjs`): the latest user text picks the reply; replies
// report cache reads/writes and, for the internal search, `server_tool_use`.
import { createServer } from "node:http";
import { once } from "node:events";
import { contentText } from "./zcode-cli-rust-interop-runtime.mjs";

const TITLE = "Generate a concise title for this coding session.";
const SEARCH_SYSTEM = "You are an assistant for performing a web search tool use.";
const COMPACT = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.";

/** A local Anthropic Messages model; the latest user text picks the reply. */
export async function anthropicModel() {
  const requests = [];
  const server = createServer(async (request, response) => {
    let body = "";
    for await (const chunk of request) body += chunk;
    const parsed = JSON.parse(body);
    requests.push(parsed);
    const system = JSON.stringify(parsed.system ?? "");
    const last = parsed.messages.at(-1);
    const lastText = contentText(last?.content);
    const firstUser = contentText(parsed.messages.find((m) => m.role === "user")?.content);
    const toolResult =
      Array.isArray(last?.content) && last.content.some((part) => part.type === "tool_result");
    const reply = (blocks, stop, extra = {}) => {
      if (parsed.stream !== true) {
        // 非流式请求（Node 的标题等旁路）：一次返回完整消息。
        const content = blocks.map(([block, deltas]) =>
          block.type === "text"
            ? { type: "text", text: deltas.map((d) => d.text ?? "").join("") }
            : {
                ...block,
                input: JSON.parse(deltas.map((d) => d.partial_json ?? "").join("") || "{}"),
              },
        );
        response.writeHead(200, { "content-type": "application/json" });
        return response.end(
          JSON.stringify({
            id: "msg_fixture",
            type: "message",
            role: "assistant",
            model: "model-a",
            content,
            stop_reason: stop,
            usage: {
              input_tokens: 100,
              cache_creation_input_tokens: 10,
              cache_read_input_tokens: 20,
              output_tokens: 7,
              ...extra.usage,
            },
          }),
        );
      }
      response.writeHead(200, { "content-type": "text/event-stream" });
      const sse = (event) =>
        response.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
      sse({
        type: "message_start",
        message: {
          id: "msg_fixture",
          type: "message",
          role: "assistant",
          model: "model-a",
          content: [],
          usage: {
            input_tokens: 100,
            cache_creation_input_tokens: 10,
            cache_read_input_tokens: 20,
            output_tokens: 1,
          },
        },
      });
      blocks.forEach(([block, deltas], index) => {
        sse({ type: "content_block_start", index, content_block: block });
        for (const delta of deltas) sse({ type: "content_block_delta", index, delta });
        sse({ type: "content_block_stop", index });
      });
      if (extra.hang) return;
      sse({
        type: "message_delta",
        delta: { stop_reason: stop },
        usage: { output_tokens: 7, ...extra.usage },
      });
      sse({ type: "message_stop" });
      response.end();
    };
    const text = (value) => [{ type: "text", text: "" }, [{ type: "text_delta", text: value }]];
    const tool = (id, name, input) => [
      { type: "tool_use", id, name, input: {} },
      [{ type: "input_json_delta", partial_json: JSON.stringify(input) }],
    ];
    if (body.includes(TITLE)) return reply([text('{"title":"Usage Diff"}')], "end_turn");
    if (system.includes(SEARCH_SYSTEM)) {
      return reply(
        [
          [
            { type: "server_tool_use", id: "srvtoolu_1", name: "web_search", input: {} },
            [{ type: "input_json_delta", partial_json: '{"query":"rust usage"}' }],
          ],
          [{ type: "web_search_tool_result", tool_use_id: "srvtoolu_1", content: [] }, []],
          text("Search result text."),
        ],
        "end_turn",
        { usage: { server_tool_use: { web_search_requests: 1 } } },
      );
    }
    if (lastText.startsWith(COMPACT)) {
      return reply([text("<summary>compacted history</summary>")], "end_turn");
    }
    if (toolResult) return reply([text(`done: ${firstUser}`)], "end_turn");
    if (lastText.includes("child task")) return reply([text("child done")], "end_turn");
    if (lastText.includes("fail now")) {
      response.writeHead(400, { "content-type": "application/json" });
      return response.end(
        JSON.stringify({
          type: "error",
          error: { type: "invalid_request_error", message: "fixture rejects this request" },
        }),
      );
    }
    if (lastText.includes("slow stream"))
      return reply([text("partial ")], "end_turn", { hang: true });
    if (lastText.includes("use tool")) {
      return reply(
        [tool("toolu_bash", "Bash", { command: "echo usage-diff", description: "Echo a word" })],
        "tool_use",
      );
    }
    if (lastText.includes("read missing")) {
      return reply([tool("toolu_read", "Read", { file_path: "missing.txt" })], "tool_use");
    }
    if (lastText.includes("write file")) {
      return reply(
        [
          tool("toolu_touch", "Bash", {
            command: "touch denied.txt",
            description: "Create a file",
          }),
        ],
        "tool_use",
      );
    }
    if (lastText.includes("web search")) {
      return reply([tool("toolu_search", "WebSearch", { query: "rust usage" })], "tool_use");
    }
    if (lastText.includes("spawn agent")) {
      return reply(
        [
          tool("toolu_agent", "Agent", {
            description: "Child task",
            prompt: "child task alpha",
            subagent_type: "general-purpose",
          }),
        ],
        "tool_use",
      );
    }
    reply([text(`answer: ${lastText.slice(0, 40)}`)], "end_turn");
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return {
    requests,
    baseUrl: `http://127.0.0.1:${server.address().port}`,
    close: () => {
      server.closeAllConnections();
      server.close();
    },
  };
}
