// rust-cli 写的模型轨迹，App 的读者能读（docs/specs/rust-cli/model-io.md §2.3、§4.3、§8，工作线 F91）：
// node-interop-check.mjs 的第二十三段。rust-cli 一侧是真实的 `zcode -p` 进程（开发构建，写 `cli/debug`），模型是本地假服务：
// 第一次 500（重发），然后一个调用 Glob 的回复，再一个文字回复。读的一侧是 App 的 `readModelTrajectory`
// （`packages/services/src/zcode-agent/modelTrajectory.ts`）：它读 `~/.zcode/cli/{debug,rollout}` 与
// `<dataBaseDir>/.zcode/cli/{debug,rollout}`。HOME 与 `ZCODE_DATA_BASE_DIR` 都指向临时目录；读的期间本进程的 HOME
// 与 App 的数据目录也指过去（`withHome`、`setDataBaseDir`），不读本机的 ~/.zcode，不联网。
//
// 证明：读出三条记录、不抛异常；会话、排序、来源分类、模型、回复的汇总与失败那次的错误都按 App 的映射读出；
// 工作线 F93（model-io.md §4.2）：Node 形状的回复字段——App 读出回复文字、工具调用（名字与参数）、停止原因（AI SDK 的
// 统一值）与用量（IN / OUT）。消息与工具名本工程不写（App 显示为空，model-io.md §4.3）。文件里有 `full`（失败的那次）与
// `delta`（重发与下一步）。
import { spawn } from "node:child_process";
import { mkdir, readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { GLM_5_3 } from "./fixture/model.mjs";
import { withHome } from "./node-root.mjs";

const SESSION = "6c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f";

const event = (data) => `event: ${data.type}\ndata: ${JSON.stringify(data)}\n\n`;
const start = (id) =>
  event({
    type: "message_start",
    message: {
      id,
      type: "message",
      role: "assistant",
      model: "m",
      content: [],
      stop_reason: null,
      usage: { input_tokens: 1, output_tokens: 1 },
    },
  });
const finish = (reason) =>
  event({ type: "message_delta", delta: { stop_reason: reason }, usage: { output_tokens: 1 } }) +
  event({ type: "message_stop" });

const GLOB =
  start("msg_glob") +
  event({
    type: "content_block_start",
    index: 0,
    content_block: { type: "tool_use", id: "toolu_glob", name: "Glob", input: {} },
  }) +
  event({
    type: "content_block_delta",
    index: 0,
    delta: { type: "input_json_delta", partial_json: '{"pattern":"*.md"}' },
  }) +
  event({ type: "content_block_stop", index: 0 }) +
  finish("tool_use");

const DONE =
  start("msg_done") +
  event({ type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }) +
  event({ type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "done" } }) +
  event({ type: "content_block_stop", index: 0 }) +
  finish("end_turn");

/** 本地模型：第一次 500，之后依次是调用 Glob 与文字。 */
async function modelServer() {
  let seen = 0;
  const server = createServer((request, response) => {
    request.resume();
    request.on("end", () => {
      seen += 1;
      if (seen === 1) {
        response.writeHead(500, { "content-type": "application/json" });
        response.end(JSON.stringify({ type: "error", error: { type: "api_error", message: "boom" } }));
        return;
      }
      response.writeHead(200, { "content-type": "text/event-stream", "request-id": `req_${seen}` });
      response.end(seen === 2 ? GLOB : DONE);
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, url: `http://127.0.0.1:${server.address().port}` };
}

function run(binary, args, env, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(binary, args, { env, cwd });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => resolve({ code, stdout, stderr }));
    child.stdin.end();
  });
}

export async function checkModelIo({ nodeRoot, base, expectEqual, zcode }) {
  const home = join(base, "home");
  const work = join(base, "work");
  await mkdir(join(work, ".git"), { recursive: true });
  await mkdir(home, { recursive: true });
  const { server, url } = await modelServer();
  let result;
  try {
    const env = {
      PATH: "/usr/bin:/bin",
      HOME: home,
      ZCODE_DATA_BASE_DIR: home,
      ZCODE_MODEL_BASE_URL: url,
      ZCODE_API_KEY: "sk-interop-model-io",
      // GLM 按 opus 的卡片形状（支持对话中间的 system，工作线 F121）：环境与日期在 system 消息里，缓存断点落在它上面，第一条
      // user 消息到下一步不变，重发与下一步都是增量（最长公共前缀 ≥ 1，model-io.md §3.3）。没有这个形状的模型只有一条
      // user 消息、断点在它的最后一块，下一步断点移走、没有公共前缀，那一条是全文（F121 实测）。
      ZCODE_MODEL: GLM_5_3,
    };
    result = await run(zcode, ["-p", "list the docs", "--session-id", SESSION], env, work);
  } finally {
    server.close();
  }
  if (result.code !== 0) throw new Error(`zcode -p failed (${result.code}): ${result.stderr}`);
  const taskId = `sess_${SESSION}`;
  const file = join(home, ".zcode/cli/debug", `model-io-${taskId}.jsonl`);
  const raw = (await readFile(file, "utf8")).trim().split("\n").map((line) => JSON.parse(line));
  expectEqual(
    "model-io: the raw records are compacted like Node's (the failed one in full, then deltas)",
    raw.map((record) => record.request.bodyMessagesKind),
    ["full", "delta", "delta"],
  );
  const services = (path) =>
    import(pathToFileURL(join(nodeRoot, "packages/services/src", path)).href);
  const { setDataBaseDir } = await services("paths.ts");
  setDataBaseDir(home);
  let trajectory;
  try {
    trajectory = await withHome(home, async () => {
      const { readModelTrajectory } = await services("zcode-agent/modelTrajectory.ts");
      return readModelTrajectory(taskId);
    });
  } finally {
    setDataBaseDir(null);
  }
  expectEqual("model-io: the App reads this session's file", trajectory.sourceFiles, [file]);
  expectEqual("model-io: not truncated", trajectory.truncated, false);
  const shape = trajectory.records.map((record) => ({
    attempt: record.attempt,
    callSource: record.callSource,
    model: record.model.modelId,
    provider: record.model.providerId,
    messages: record.request.messages.length,
    tools: record.request.toolNames.length,
    finishReason: record.response?.finishReason,
    responseId: record.response?.responseId,
    error: record.error?.name,
  }));
  const common = {
    callSource: { kind: "main", querySource: "main_turn" },
    model: GLM_5_3,
    provider: "anthropic",
    messages: 0,
    tools: 0,
  };
  expectEqual("model-io: the App maps the rust-cli records", shape, [
    { ...common, attempt: 1, finishReason: undefined, responseId: undefined, error: "server" },
    { ...common, attempt: 2, finishReason: "tool-calls", responseId: "msg_glob", error: undefined },
    { ...common, attempt: 1, finishReason: "stop", responseId: "msg_done", error: undefined },
  ]);
  // 工作线 F93：App 的面板读出回复文字与工具调用（F91 时两者都是空的）；失败的那次只有错误正文，没有回复字段。
  const replies = trajectory.records.map((record) => ({
    text: record.response?.text,
    toolCalls: record.response?.toolCalls,
    usage: record.response?.usage && {
      input: record.response.usage.inputTokens,
      output: record.response.usage.outputTokens,
    },
  }));
  expectEqual("model-io: the App reads the reply text and the tool calls", replies, [
    { text: undefined, toolCalls: [], usage: undefined },
    {
      text: "",
      toolCalls: [
        { kind: "tool-call", toolCallId: "toolu_glob", toolName: "Glob", input: { pattern: "*.md" } },
      ],
      usage: { input: 1, output: 1 },
    },
    { text: "done", toolCalls: [], usage: { input: 1, output: 1 } },
  ]);
  expectEqual(
    "model-io: every record has a request id and a start time",
    trajectory.records.every((record) => record.requestId && record.startedAt),
    true,
  );
  return { records: trajectory.records.length };
}
