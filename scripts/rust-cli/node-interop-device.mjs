// rust-cli 与 Node CLI 共用设备身份（docs/specs/rust-cli/platform.md §10、model.md §3.3，工作线 F52）。
// node-interop-check.mjs 的第十六段。Node 一侧是 Node CLI 的真实代码（`ensureCliDeviceMid`、
// `createAnthropicRequestMetadataUserId`），rust-cli 一侧是真实的 `zcode -p` 进程，模型是本地假服务；HOME 与
// `ZCODE_DATA_BASE_DIR` 都指向临时目录（Node 调用期间本进程的 HOME 也指过去，`withHome`），不读本机的 ~/.zcode。
//
// A. Node 先生成：`telemetry-state.json` 里的 deviceMid 由 Node 写，rust-cli 的请求里 `metadata.user_id` 用同一个值，
//    文本与 Node 对同一个会话算出的逐字节相同；文件不被改写。
// B. rust-cli 先生成：rust-cli 写进同一个文件（保留其他字段），Node 读到同一个值。
import { spawn } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { importNode, withHome } from "./node-root.mjs";
import { GLM_5_3 } from "./fixture/model.mjs";

const SSE = [
  'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}}\n\n',
  'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
  'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}\n\n',
  'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\n',
  'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}\n\n',
  'event: message_stop\ndata: {"type":"message_stop"}\n\n',
].join("");

/** 本地模型：记下每个 messages 请求的请求体。 */
async function modelServer() {
  const bodies = [];
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      if (request.url?.startsWith("/v1/messages") && !request.url.includes("count_tokens"))
        bodies.push(JSON.parse(body));
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(SSE);
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, bodies, url: `http://127.0.0.1:${server.address().port}` };
}

function run(binary, args, env, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(binary, args, { env, cwd });
    let stderr = "";
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => resolve({ code, stderr }));
    child.stdin.end();
  });
}

/** `zcode -p hi --session-id <uuid>`，返回第一个请求的 `metadata.user_id`（解析前的文本）。 */
async function rustUserId({ zcode, home, url, session, bodies }) {
  const work = join(home, "work");
  await mkdir(work, { recursive: true });
  const env = {
    PATH: "/usr/bin:/bin",
    HOME: home,
    ZCODE_DATA_BASE_DIR: home,
    ZCODE_MODEL_BASE_URL: url,
    ZCODE_API_KEY: "sk-interop",
    ZCODE_MODEL: GLM_5_3,
  };
  const before = bodies.length;
  const result = await run(zcode, ["-p", "hi", "--session-id", session], env, work);
  if (result.code !== 0) throw new Error(`zcode -p failed (${result.code}): ${result.stderr}`);
  const body = bodies[before];
  if (!body) throw new Error("zcode -p sent no model request");
  return body.metadata?.user_id;
}

const stateFile = (home) => join(home, ".zcode/v2/telemetry-state.json");

export async function checkDevice({ nodeRoot, base, expectEqual, zcode }) {
  const { ensureCliDeviceMid } = await importNode(nodeRoot, "adapters/src/device/cli-device-mid.ts");
  const { createAnthropicRequestMetadataUserId } = await importNode(
    nodeRoot,
    "adapters/src/model/anthropic-request-metadata.ts",
  );
  const { server, bodies, url } = await modelServer();
  const findings = {};
  try {
    // A. Node 先生成。
    const nodeFirst = join(base, "node-first");
    await mkdir(nodeFirst, { recursive: true });
    const nodeMid = await withHome(nodeFirst, () =>
      ensureCliDeviceMid({ env: { ZCODE_DATA_BASE_DIR: nodeFirst } }),
    );
    const written = await readFile(stateFile(nodeFirst), "utf8");
    const session = "2d480878-38af-4bae-a54f-096e2fb0f4d7";
    const userId = await rustUserId({ zcode, home: nodeFirst, url, session, bodies });
    expectEqual(
      "device: rust-cli sends the deviceMid Node generated (metadata.user_id text)",
      createAnthropicRequestMetadataUserId({ deviceMid: nodeMid, sessionId: `sess_${session}` }),
      userId,
    );
    expectEqual(
      "device: rust-cli leaves Node's telemetry-state.json as it is",
      written,
      await readFile(stateFile(nodeFirst), "utf8"),
    );
    findings.nodeFirst = JSON.parse(userId ?? "{}").device_id === nodeMid;

    // B. rust-cli 先生成（文件里已有 telemetry 的别的字段）。
    const rustFirst = join(base, "rust-first");
    await mkdir(join(rustFirst, ".zcode/v2"), { recursive: true });
    await writeFile(stateFile(rustFirst), JSON.stringify({ lastDailyActiveDate: "2026-10-21" }));
    const rustSent = JSON.parse(
      (await rustUserId({ zcode, home: rustFirst, url, session, bodies })) ?? "{}",
    );
    const stored = JSON.parse(await readFile(stateFile(rustFirst), "utf8"));
    expectEqual(
      "device: rust-cli keeps the other telemetry fields",
      "2026-10-21",
      stored.lastDailyActiveDate,
    );
    const nodeReads = await withHome(rustFirst, () =>
      ensureCliDeviceMid({ env: { ZCODE_DATA_BASE_DIR: rustFirst } }),
    );
    expectEqual("device: Node reads the deviceMid rust-cli generated", nodeReads, rustSent.device_id);
    expectEqual("device: rust-cli stored what it sent", stored.deviceMid, rustSent.device_id);
    findings.rustFirst = nodeReads === rustSent.device_id;
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
  return findings;
}
