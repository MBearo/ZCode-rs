import assert from "node:assert/strict";
import test from "node:test";
import { once } from "node:events";
import { createServer as createHttpServer, type IncomingMessage } from "node:http";
import { createServer as createHttpsServer } from "node:https";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { end, event, fixture } from "./zcode-cli-rust-fixture.js";
import { anthropic } from "./zcode-cli-rust-protocol-fixture.js";

type Message = Record<string, any>;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

async function listen<T extends ReturnType<typeof createHttpServer>>(server: T) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert(address && typeof address !== "string");
  return address.port;
}

async function stop(server: ReturnType<typeof createHttpServer>) {
  server.closeAllConnections();
  await new Promise<void>((r) => server.close(() => r()));
}

async function send(h: any, id: string, text: string) {
  const after = h.messages.length;
  const ack = await h.command(h.envelope("sendText", id, { text }));
  await h.completed(id, after);
  return ack.result.inputId as string;
}

test("Rust model requests carry Node identity and attribution headers", async () => {
  const f = await fixture({
    env: {
      ZCODE_APP_VERSION: "9.8.7",
      ZCODE_ENV: "test",
      ZCODE_BASE_URL: "https://zcode.example.test/path",
      LC_ALL: "zh_CN.UTF-8",
      // 与 Node 一致：shell 代理不作用于模型请求，指向不可达端口也必须直连成功。
      HTTP_PROXY: "http://127.0.0.1:9",
      http_proxy: "http://127.0.0.1:9",
    },
  });
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const first = await send(h, id, "one");
    const second = await send(h, id, "two");
    const [a, b] = f.requestHeaders;
    assert.equal(a!["user-agent"], "ZCode/9.8.7");
    assert.equal(a!["x-zcode-app-version"], "9.8.7");
    assert.equal(a!["x-title"], "Z Code@electron");
    assert.equal(a!["x-release-channel"], "test");
    assert.equal(a!["http-referer"], "https://zcode.example.test");
    assert.equal(a!["x-client-language"], "zh-CN");
    assert.equal(a!["x-zcode-agent"], "glm");
    assert(a!["x-os-category"]);
    assert.match(String(a!["x-platform"]), /^(darwin|linux|win32)-/);
    assert.equal(a!["x-zcode-session-type"], "main");
    assert.equal(a!["x-query-id"], first);
    assert.equal(b!["x-query-id"], second);
    assert.equal(a!["x-session-id"], id.replace(/^sess_/, ""));
    assert.match(String(a!["x-zcode-trace-id"]), UUID);
    // 同一会话运行时共用 root trace；每个物理请求有自己的 request id。
    assert.equal(a!["x-zcode-trace-id"], b!["x-zcode-trace-id"]);
    assert.match(String(a!["x-request-id"]), UUID);
    assert.notEqual(a!["x-request-id"], b!["x-request-id"]);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust Anthropic requests send the device id shared with Desktop", async () => {
  const f = await fixture({
    config: { apiType: "anthropic-messages", reasoningParameters: {} },
    respond(_req, res) {
      anthropic(res, false);
    },
  });
  try {
    const config = JSON.parse(await readFile(f.config, "utf8"));
    config.baseUrl = config.baseUrl.replace(/\/v1$/, "");
    await writeFile(f.config, JSON.stringify(config));
    const state = join(f.root, ".zcode", "v2");
    await mkdir(state, { recursive: true });
    await writeFile(
      join(state, "telemetry-state.json"),
      JSON.stringify({ deviceMid: "device-fixture", other: true }),
    );
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "hello");
    assert.equal(
      f.requests[0]!.metadata.user_id,
      JSON.stringify({
        device_id: "device-fixture",
        account_uuid: "",
        session_id: id.replace(/^sess_/, ""),
      }),
    );
  } finally {
    await f.close();
  }
});

test("Rust routes model requests through the explicit proxy and honors no_proxy", async () => {
  const proxied: string[] = [];
  const proxy = createHttpServer(async (req: IncomingMessage, res) => {
    proxied.push(req.url ?? "");
    for await (const _ of req);
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    event(res, { content: "via proxy" });
    end(res, "stop");
  });
  const port = await listen(proxy);
  try {
    for (const bypass of [false, true]) {
      proxied.length = 0;
      const f = await fixture({
        env: {
          ZCODE_HTTP_PROXY: `127.0.0.1:${port}`,
          ...(bypass ? { ZCODE_NO_PROXY: "localhost, 127.0.0.1" } : {}),
        },
      });
      try {
        const h = f.start(),
          id = await h.create();
        await h.subscribe(`conversation/${id}`);
        await send(h, id, "proxy");
        if (bypass) {
          assert.equal(proxied.length, 0);
          assert.equal(f.requests.length, 1);
        } else {
          // HTTP 目标经代理时使用 absolute-form 请求行。
          assert.deepEqual(proxied, [`${f.baseUrl}/chat/completions`]);
          assert.equal(f.requests.length, 0);
        }
      } finally {
        await f.close();
      }
    }
  } finally {
    await stop(proxy);
  }
});

test(
  "Rust Bash children see the egress environment instead of runtime network variables",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture({
      env: {
        ZCODE_HTTP_PROXY: "proxy.test:3128",
        ZCODE_NO_PROXY: "127.0.0.1",
        HTTP_PROXY: "http://inherited:1",
        NODE_ENV: "development",
        npm_config_proxy: "npm-proxy",
        ZCODE_CUA_PERMISSION_BROKER_SOCKET: "/tmp/broker.sock",
        OTEL_SERVICE_NAME: "leak",
      },
      respond(request, res) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        if (request.messages.at(-1).role !== "tool") {
          event(res, {
            tool_calls: [
              {
                index: 0,
                id: "call-env",
                type: "function",
                function: { name: "Bash", arguments: JSON.stringify({ command: "env | sort" }) },
              },
            ],
          });
          end(res, "tool_calls");
          return;
        }
        event(res, { content: "done" });
        end(res, "stop");
      },
    });
    try {
      const h = f.start(),
        id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await send(h, id, "env");
      // Node Bash 结果是纯文本（合并输出）。
      const output = f.requests[1]!.messages.findLast((m: Message) => m.role === "tool")
        .content as string;
      const env = new Map(
        output
          .split("\n")
          .map((line) => line.split(/=(.*)/s))
          .filter((pair) => pair.length > 1)
          .map(([k, v]) => [k, v] as [string, string]),
      );
      for (const key of ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy"])
        assert.equal(env.get(key), "http://proxy.test:3128", key);
      assert.equal(env.get("NO_PROXY"), "127.0.0.1");
      assert.equal(env.get("no_proxy"), "127.0.0.1");
      assert.equal(env.get("npm_config_proxy"), "npm-proxy");
      for (const key of [
        "NODE_ENV",
        "ZCODE_CUA_PERMISSION_BROKER_SOCKET",
        "OTEL_SERVICE_NAME",
        "ZCODE_TOOL_ENV_PASSTHROUGH_JSON",
      ])
        assert(!env.has(key), key);
    } finally {
      await f.close();
    }
  },
);

test(
  "Rust model TLS trusts the configured CA bundle",
  { skip: process.platform === "win32" },
  async () => {
    const temp = await mkdtemp(join(tmpdir(), "rust-ca-"));
    const run = promisify(execFile);
    const path = (name: string) => join(temp, name);
    // 测试期生成 CA 与服务端证书；不在仓库保存私钥，也不请求外部服务。
    const openssl = (args: string) =>
      run(
        "openssl",
        args.split(" ").map((arg) => arg.replace(/^@(.+)$/, (_, n) => path(n))),
      );
    await openssl(
      "req -x509 -newkey rsa:2048 -nodes -days 1 -keyout @ca.key -out @ca.pem -subj /CN=zcode-test-ca",
    );
    await openssl(
      "req -newkey rsa:2048 -nodes -subj /CN=localhost -keyout @key.pem -out @leaf.csr",
    );
    await writeFile(
      path("ext.cnf"),
      "basicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n",
    );
    await openssl(
      "x509 -req -days 1 -in @leaf.csr -CA @ca.pem -CAkey @ca.key -CAcreateserial -out @cert.pem -extfile @ext.cnf",
    );
    let served = 0;
    const server = createHttpsServer(
      { key: await readFile(path("key.pem")), cert: await readFile(path("cert.pem")) },
      async (req, res) => {
        served++;
        for await (const _ of req);
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        event(res, { content: "trusted" });
        end(res, "stop");
      },
    );
    const port = await listen(server as any);
    for (const ca of [path("ca.pem"), path("missing.pem")]) {
      const f = await fixture({ env: { ZCODE_AGENT_CA_CERT: ca } });
      try {
        const config = JSON.parse(await readFile(f.config, "utf8"));
        await writeFile(
          f.config,
          JSON.stringify({ ...config, baseUrl: `https://127.0.0.1:${port}/v1` }),
        );
        const h = f.start(),
          id = await h.create();
        await h.subscribe(`conversation/${id}`);
        const after = h.messages.length;
        await h.command(h.envelope("sendText", id, { text: "tls" }));
        if (ca.endsWith("ca.pem")) {
          await h.completed(id, after);
          assert.equal(served, 1);
          continue;
        }
        const failure = await h.wait((m) =>
          m.params?.frame?.payload?.deltas?.some((d: any) => d.patch?.control?.phase === "error"),
        );
        const error = failure.params.frame.payload.deltas.find(
          (d: any) => d.patch?.control?.lastError,
        ).patch.control.lastError;
        assert.equal(error.attribution.reason, "tls_error");
        assert.equal(served, 1);
      } finally {
        await f.close();
      }
    }
    await stop(server as any);
    await rm(temp, { recursive: true, force: true });
  },
);
