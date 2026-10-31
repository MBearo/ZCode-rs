// rust-cli 与 Node CLI 的模型请求头（docs/specs/rust-cli/model.md §2.4，工作线 F64）：node-interop-check.mjs 的第十七段。
// Node 一侧是 Node CLI 的真实代码（`buildCliZCodeSourceHeaders`、`createModelRequestAttributionHeaders`），rust-cli 一侧是
// 真实的 `zcode -p` 进程，模型是本地假服务；HOME 与 `ZCODE_DATA_BASE_DIR` 指向临时目录（Node 调用期间本进程的 HOME
// 也指过去，`withHome`），不读本机的 ~/.zcode，不联网。
//
// A. 对照夹具没有过时：`headers-golden.mjs --check` 用 Node 的函数重新生成，与仓库里的逐字相同。
// B. 同一组环境（LANG、TZ、ZCODE_ENV）下，rust-cli 线上的来源头与 Node 在同样的 Intl、平台与系统值下组装的相同（名字
//    不分大小写）；每请求头与 Node 对同一会话、回合与 ID 组装的逐项相同。
import { spawn, spawnSync } from "node:child_process";
import { mkdir } from "node:fs/promises";
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

const SESSION = "5b1c2f7e-9d4a-4e1b-8c3f-2a6d9e0b1c4d";
const ENV = { LANG: "ja_JP.UTF-8", TZ: "Europe/Berlin", ZCODE_ENV: "test" };

/** 本地模型：记下每个 messages 请求的头（名字小写）。 */
async function modelServer() {
  const requests = [];
  const server = createServer((request, response) => {
    request.resume();
    request.on("end", () => {
      if (request.url?.startsWith("/v1/messages") && !request.url.includes("count_tokens"))
        requests.push(request.headers);
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(SSE);
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, requests, url: `http://127.0.0.1:${server.address().port}` };
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

/** 只带这些变量（和 PATH）的 node 进程读到的 Intl 缺省值。 */
function intl(env) {
  const result = spawnSync(
    process.execPath,
    ["-p", "JSON.stringify(Intl.DateTimeFormat().resolvedOptions())"],
    { env: { PATH: process.env.PATH, ...env }, encoding: "utf8" },
  );
  return JSON.parse(result.stdout);
}

/** Node 在固定的 Intl 值下组装的来源头（平台与系统值就是本机的，与 rust-cli 读到的相同）。 */
function nodeSourceHeaders(build, env, appVersion, resolved) {
  const saved = Intl.DateTimeFormat;
  Intl.DateTimeFormat = function DateTimeFormat() {
    return { resolvedOptions: () => resolved };
  };
  try {
    return build(env, { appVersion, sourceTitle: "cli" });
  } finally {
    Intl.DateTimeFormat = saved;
  }
}

const lower = (headers) =>
  Object.fromEntries(Object.entries(headers).map(([name, value]) => [name.toLowerCase(), value]));

export async function checkHeaders({ nodeRoot, repoRoot, base, expectEqual, zcode }) {
  // A. 夹具没有过时。
  const golden = spawnSync(
    process.execPath,
    [join(repoRoot, "scripts/rust-cli/headers-golden.mjs"), "--node-root", nodeRoot, "--check"],
    // 子进程自己经 tsx 重新执行（本进程的标记不传下去）。
    { encoding: "utf8", env: { ...process.env, RUST_CLI_NODE_TSX: "0" } },
  );
  expectEqual("headers golden is up to date", golden.status, 0);
  if (golden.status !== 0) console.error(golden.stdout + golden.stderr);

  // B. 真实的 `zcode -p`。
  const home = join(base, "home");
  const work = join(base, "work");
  await mkdir(join(work, ".git"), { recursive: true });
  await mkdir(home, { recursive: true });
  const { server, requests, url } = await modelServer();
  try {
    const env = {
      PATH: "/usr/bin:/bin",
      HOME: home,
      ZCODE_DATA_BASE_DIR: home,
      ZCODE_MODEL_BASE_URL: url,
      ZCODE_API_KEY: "sk-interop",
      ZCODE_MODEL: GLM_5_3,
      ...ENV,
    };
    const version = (await run(zcode, ["--version"], env, work)).stdout.split(/\s+/)[1];
    const result = await run(zcode, ["-p", "hi", "--session-id", SESSION], env, work);
    if (result.code !== 0) throw new Error(`zcode -p failed (${result.code}): ${result.stderr}`);
    const sent = requests[0];
    if (!sent) throw new Error("zcode -p sent no model request");
    const node = await withHome(home, async () => {
      const { buildCliZCodeSourceHeaders } = await importNode(nodeRoot, "bootstrap/src/model-config.ts");
      const { createModelRequestAttributionHeaders } = await importNode(
        nodeRoot,
        "adapters/src/model/runner-attribution.ts",
      );
      const resolved = {
        locale: intl({ LANG: ENV.LANG }).locale,
        timeZone: intl({ TZ: ENV.TZ }).timeZone,
      };
      return {
        source: lower(nodeSourceHeaders(buildCliZCodeSourceHeaders, ENV, version, resolved)),
        attribution: createModelRequestAttributionHeaders({
          baseURL: url,
          // rust-cli 的回合 `run_<x>` 发成 `<x>`，对应 Node 的 query id `query_<x>`。
          queryId: `query_${sent["x-query-id"]}`,
          querySource: "main_turn",
          requestId: sent["x-request-id"],
          sessionId: `sess_${SESSION}`,
          traceId: sent["x-zcode-trace-id"],
          modelRequestSessionType: "main",
        }),
      };
    });
    const pick = (names) => Object.fromEntries(names.map((name) => [name, sent[name]]));
    expectEqual("source headers", pick(Object.keys(node.source)), node.source);
    expectEqual("request headers", pick(Object.keys(node.attribution)), node.attribution);
    return { version, locale: node.source["x-client-language"], zone: node.source["x-client-timezone"] };
  } finally {
    server.close();
  }
}
