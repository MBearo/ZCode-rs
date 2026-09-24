import assert from "node:assert/strict";
import test from "node:test";
import { once } from "node:events";
import { createServer, type IncomingHttpHeaders } from "node:http";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { zcodeMcpListResultSchema, zcodePluginsListResultSchema } from "@zcode/shared";
import { fixture } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
type Mode = "ok" | "retry" | "plan";
const META_KEY = "com.zcode/official-mcp-auth";
const AUTH = { type: "zcode_official", provider: "jwt_token" };
const ZCODE_ORIGIN = "https://zcode.example";
const tools = [{ name: "search", description: "Search", inputSchema: { type: "object" } }];

async function put(path: string, value: unknown) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, typeof value === "string" ? value : JSON.stringify(value));
}

/** 官方 MCP 的 streamable HTTP 夹具：记录每个 POST 的方法与请求头。 */
async function officialServer(mode: Mode) {
  const posts: Array<{ method?: string; headers: IncomingHttpHeaders }> = [];
  let rejected = false;
  const server = createServer(async (req, res) => {
    if (req.method !== "POST") {
      res.writeHead(req.method === "GET" ? 405 : 204).end();
      return;
    }
    req.setEncoding("utf8");
    let body = "";
    for await (const part of req) body += part;
    const m = JSON.parse(body);
    posts.push({ method: m.method, headers: req.headers });
    if (mode === "retry" && !rejected && req.headers.authorization) {
      rejected = true;
      res.writeHead(401, { "Content-Type": "application/json" }).end("{}");
      return;
    }
    if (mode === "plan" && m.method === "initialize") {
      const error = { jsonrpc: "2.0", id: m.id, error: { code: 3101, message: "plan required" } };
      res.writeHead(403, { "Content-Type": "application/json", "X-Request-Id": "srv-1" });
      res.end(JSON.stringify(error));
      return;
    }
    if (m.id === undefined) {
      res.writeHead(202).end();
      return;
    }
    const result =
      m.method === "initialize"
        ? {
            protocolVersion: "2025-11-25",
            serverInfo: { name: "official", version: "1" },
            capabilities: { tools: {} },
          }
        : { tools };
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ jsonrpc: "2.0", id: m.id, result }));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const port = (server.address() as { port: number }).port;
  return {
    origin: `http://127.0.0.1:${port}`,
    url: `http://127.0.0.1:${port}/mcp`,
    posts,
    close: async () => {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

/** stdio MCP 夹具：把每条请求与通知的官方身份载荷写入文件。 */
async function stdioScript(root: string) {
  const script = join(root, "official-stdio.mjs");
  const log = join(root, "official-stdio.log");
  await put(
    script,
    `
import { createInterface } from "node:readline";
import { appendFileSync } from "node:fs";
const reply=(m,result)=>process.stdout.write(JSON.stringify({jsonrpc:"2.0",id:m.id,result})+"\\n");
createInterface({input:process.stdin}).on("line",line=>{const m=JSON.parse(line);
 appendFileSync(process.argv[2], JSON.stringify({method:m.method,auth:m.params?._meta?.[${JSON.stringify(META_KEY)}] ?? null})+"\\n");
 if(m.method==="initialize")reply(m,{protocolVersion:"2025-11-25",serverInfo:{name:"s",version:"1"},capabilities:{tools:{}}});
 else if(m.method==="tools/list")reply(m,{tools:${JSON.stringify(tools)}});});
`,
  );
  return { script, log };
}

test("Rust sends official MCP identity headers like Node", async () => {
  const ok = await officialServer("ok");
  const retry = await officialServer("retry");
  const plan = await officialServer("plan");
  const untrusted = await officialServer("ok");
  const dev = [ok, retry, plan].map((s) => s.origin).join(",");
  const f = await fixture({
    env: {
      ZCODE_OFFICIAL_MCP_DEV_TRUSTED_ORIGINS: dev,
      ZCODE_ENDPOINT_ORIGIN: ZCODE_ORIGIN,
      NO_PROXY: "127.0.0.1,localhost",
      no_proxy: "127.0.0.1,localhost",
    },
  });
  try {
    const stdio = await stdioScript(f.root);
    const plugin = join(f.root, "plugins-src/off");
    await put(join(plugin, ".zcode-plugin/plugin.json"), { name: "off", version: "1.0.0" });
    await put(join(plugin, ".mcp.json"), {
      mcpServers: {
        ok: {
          type: "http",
          url: ok.url,
          auth: AUTH,
          headers: { "X-Request-Id": "s", "X-Custom": "c" },
        },
        retry: { type: "http", url: retry.url, auth: AUTH },
        plan: { type: "http", url: plan.url, auth: AUTH },
        untrusted: { type: "http", url: untrusted.url, auth: AUTH },
        local: { command: process.execPath, args: [stdio.script, stdio.log], auth: AUTH },
        reserved: { type: "http", url: ok.url, auth: AUTH, headers: { Authorization: "x" } },
      },
    });
    await put(join(f.cwd, ".zcode/config.json"), { plugins: { dirs: [plugin] } });
    const h = f.start();
    const asked: Message[] = [];
    h.client.onRequest((request) => {
      if (request.method !== "interaction/requestOfficialMcpAuthHeaders") return;
      asked.push(request.params as Message);
      const headers = {
        Authorization: `Bearer tok-${asked.length}`,
        "X-Bigmodel-Authorization": "Bearer jwt",
      };
      // stdio server 验证失败载荷：原因原样下发给插件进程。
      const planRequired = (request.params as Message).mcpKey === "local";
      void h.client.respond(
        request.id,
        planRequired ? { ok: false, reason: "official_auth_plan_required" } : { ok: true, headers },
      );
    });
    const list = await h.client.request(
      "mcp/list",
      { workspace: { workspacePath: f.cwd }, mode: "connect" },
      zcodeMcpListResultSchema,
    );
    const status = (key: string) => list.statuses[`plugin:off:${key}`] as Message;

    assert.equal(status("ok")?.status, "connected");
    assert.equal(status("ok")?.toolCount, 1);
    assert.deepEqual(
      ok.posts.map((p) => p.method),
      ["initialize", "notifications/initialized", "tools/list"],
    );
    const tokens = ok.posts.map((p) => p.headers.authorization);
    assert(
      tokens.every((t) => /^Bearer tok-\d+$/.test(t ?? "")),
      "every request carries identity",
    );
    assert.equal(new Set(tokens).size, tokens.length, "identity is resolved per request");
    assert(ok.posts.every((p) => p.headers["x-bigmodel-authorization"] === "Bearer jwt"));
    assert(ok.posts.every((p) => p.headers["x-custom"] === "c" && !p.headers["x-request-id"]));
    const first = asked.find((a) => a.mcpKey === "ok")!;
    assert.match(first.requestId, /^official-mcp-auth:\d+$/);
    assert.deepEqual(
      { ...first, requestId: undefined },
      {
        requestId: undefined,
        mcpKey: "ok",
        pluginId: "off@inline",
        targetOrigin: ok.origin,
        workspace: { workspaceKey: f.cwd, workspacePath: f.cwd },
      },
    );

    // 401 后重新取身份头重试一次。
    assert.equal(status("retry")?.status, "connected");
    const [rejectedToken, retriedToken] = retry.posts.map((p) => p.headers.authorization);
    assert.notEqual(rejectedToken, retriedToken);
    assert.equal(retry.posts[1]?.method, "initialize");

    // 403：连接失败，诊断取响应体的 JSON-RPC 错误码与服务端 request id。
    assert.deepEqual(
      [status("plan")?.status, status("plan")?.failureKind, status("plan")?.serverRequestId],
      ["failed", "coding_plan_required", "srv-1"],
    );
    assert.match(status("plan")?.error, / - srv-1$/);

    // 不可信目标：零网络请求、零身份请求。
    assert.deepEqual(
      [status("untrusted")?.status, status("untrusted")?.failureKind],
      ["failed", "official_origin_untrusted"],
    );
    assert.equal(untrusted.posts.length, 0);
    assert.equal(asked.filter((a) => a.mcpKey === "untrusted").length, 0);

    // stdio：每条请求与通知的 _meta 携带身份载荷，目标 origin 由宿主给出。
    assert.equal(status("local")?.status, "connected");
    const lines = (await readFile(stdio.log, "utf8"))
      .trim()
      .split("\n")
      .map((l) => JSON.parse(l));
    assert.deepEqual(
      lines.map((l) => l.method),
      ["initialize", "notifications/initialized", "tools/list"],
    );
    assert.deepEqual(
      new Set(lines.map((l) => JSON.stringify(l.auth))),
      new Set([JSON.stringify({ ok: false, reason: "official_auth_plan_required" })]),
    );
    assert(asked.filter((a) => a.mcpKey === "local").every((a) => a.targetOrigin === ZCODE_ORIGIN));

    // 静态保留头：插件解析阶段禁用该 server。
    assert.equal(status("reserved"), undefined);
    const plugins = await h.client.request(
      "plugins/list",
      { workspace: { workspacePath: f.cwd } },
      zcodePluginsListResultSchema,
    );
    assert(
      plugins.diagnostics.some((d) =>
        d.message.includes("static headers must not contain reserved header(s): authorization"),
      ),
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
    await Promise.all([ok, retry, plan, untrusted].map((s) => s.close()));
  }
});
