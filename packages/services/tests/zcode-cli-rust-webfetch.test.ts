import assert from "node:assert/strict";
import test from "node:test";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { createServer as createHttpServer } from "node:http";
import { createServer as createHttpsServer } from "node:https";
import { connect, type AddressInfo } from "node:net";
import { join } from "node:path";
import forgeModule from "node-forge";
import { end, event, fixture } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

// 包内声明只覆盖 appCaCert.ts 用到的子集；测试需要的 SAN 与签发者属性按 any 使用。
const forge = forgeModule as any;

/** A throwaway CA and a leaf certificate for the test hosts, generated per run. */
function certificate() {
  const issue = (
    subject: string,
    issuer: { name: string; key: any } | undefined,
    extensions: object[],
  ) => {
    const keys = forge.pki.rsa.generateKeyPair(2048);
    const cert = forge.pki.createCertificate();
    cert.publicKey = keys.publicKey;
    cert.serialNumber = String(Date.now());
    cert.validity.notBefore = new Date(Date.now() - 60_000);
    cert.validity.notAfter = new Date(Date.now() + 3_600_000);
    cert.setSubject([{ name: "commonName", value: subject }]);
    cert.setIssuer([{ name: "commonName", value: issuer?.name ?? subject }]);
    cert.setExtensions(extensions);
    cert.sign(issuer?.key ?? keys.privateKey, forge.md.sha256.create());
    return { cert, key: keys.privateKey };
  };
  const caName = "ZCode WebFetch Test CA";
  const ca = issue(caName, undefined, [
    { name: "basicConstraints", cA: true },
    { name: "keyUsage", keyCertSign: true, cRLSign: true },
  ]);
  const hosts = ["example.test", "www.example.test", "other.test"];
  const leaf = issue("example.test", { name: caName, key: ca.key }, [
    { name: "basicConstraints", cA: false },
    { name: "keyUsage", digitalSignature: true, keyEncipherment: true },
    { name: "extKeyUsage", serverAuth: true },
    { name: "subjectAltName", altNames: hosts.map((value) => ({ type: 2, value })) },
  ]);
  return {
    ca: forge.pki.certificateToPem(ca.cert) as string,
    cert: forge.pki.certificateToPem(leaf.cert) as string,
    key: forge.pki.privateKeyToPem(leaf.key) as string,
  };
}

/** The site behind a CONNECT proxy: WebFetch reaches it through the captured shell proxy. */
async function site() {
  const tls = certificate();
  const hits: string[] = [];
  const https = createHttpsServer({ cert: tls.cert, key: tls.key }, (req, res) => {
    hits.push(req.url ?? "");
    if (req.url === "/page") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end('<h1>Title</h1><p>Hello <a href="/x">x</a></p><script>no()</script>');
    } else if (req.url === "/same") {
      res.writeHead(302, { location: "/page" });
      res.end();
    } else if (req.url === "/other") {
      res.writeHead(301, { location: "https://other.test/p" });
      res.end();
    } else if (req.url === "/image") {
      res.writeHead(200, { "content-type": "image/png" });
      res.end("png");
    } else {
      res.writeHead(404, { "retry-after": "30" });
      res.end("missing");
    }
  });
  https.listen(0, "127.0.0.1");
  await once(https, "listening");
  const httpsPort = (https.address() as AddressInfo).port;
  const proxy = createHttpServer();
  const tunnels: string[] = [];
  proxy.on("connect", (req, socket, head) => {
    tunnels.push(req.url ?? "");
    const upstream = connect(httpsPort, "127.0.0.1", () => {
      socket.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      upstream.write(head);
      upstream.pipe(socket);
      socket.pipe(upstream);
    });
    upstream.on("error", () => socket.destroy());
    socket.on("error", () => upstream.destroy());
  });
  proxy.listen(0, "127.0.0.1");
  await once(proxy, "listening");
  return {
    ca: tls.ca,
    proxy: `http://127.0.0.1:${(proxy.address() as AddressInfo).port}`,
    hits,
    tunnels,
    close() {
      https.closeAllConnections();
      proxy.closeAllConnections();
      https.close();
      proxy.close();
    },
  };
}

test("Rust WebFetch fetches through the shell proxy, processes pages and keeps Node's texts", async () => {
  const web = await site();
  const certDir = await mkdtemp(join(tmpdir(), "zcode-webfetch-ca-"));
  const certPath = join(certDir, "site.pem");
  await writeFile(certPath, web.ca);
  const processed: Message[] = [];
  const f = await fixture({
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      const first = req.messages[0];
      if (req.messages.length === 1 && String(first.content).startsWith("\nWeb page content:")) {
        processed.push(req);
        event(res, { content: `  processed ${processed.length}  ` });
        end(res, "stop");
        return;
      }
      const last = req.messages.at(-1);
      if (last.role === "tool") {
        event(res, { content: "done" });
        end(res, "stop");
        return;
      }
      // 运行时可能在其后追加提醒消息：取最后一条 "fetch <url>" 用户消息。
      const asked = req.messages.findLast(
        (m: Message) => m.role === "user" && String(m.content).startsWith("fetch "),
      );
      const url = String(asked?.content ?? "").slice("fetch ".length);
      const args = url === "bad" ? { url: 1 } : { url, prompt: "Summarize it" };
      event(res, {
        tool_calls: [
          {
            index: 0,
            id: "call-fetch",
            type: "function",
            function: { name: "WebFetch", arguments: JSON.stringify(args) },
          },
        ],
      });
      end(res, "tool_calls");
    },
    env: {
      ZCODE_TOOL_ENV_PASSTHROUGH_JSON: JSON.stringify({ https_proxy: web.proxy }),
      ZCODE_AGENT_CA_CERT: certPath,
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const toolResult = async (text: string) => {
      const after = h.messages.length;
      await h.command(h.envelope("sendText", id, { text }));
      await h.completed(id, after);
      return f.requests.at(-1)!.messages.at(-1).content as string;
    };

    assert.equal(await toolResult("fetch http://example.test/page"), "processed 1");
    const prompt = processed[0]!.messages[0].content as string;
    assert.ok(
      prompt.startsWith(
        "\nWeb page content:\n---\n# Title\nHello [x](/x)\n---\n\nSummarize it\n\n",
      ),
    );
    assert.ok(prompt.includes("Enforce a strict 125-character maximum"));
    assert.equal(processed[0]!.tools, undefined);
    assert.deepEqual(web.tunnels, ["example.test:443"]);

    // 同一原始 URL 命中进程缓存：不再出网，仍经模型处理。
    assert.equal(await toolResult("fetch http://example.test/page"), "processed 2");
    assert.deepEqual(web.hits, ["/page"]);

    assert.equal(await toolResult("fetch https://www.example.test/same"), "processed 3");
    assert.deepEqual(web.hits.slice(1), ["/same", "/page"]);

    assert.equal(
      await toolResult("fetch https://example.test/other"),
      [
        "REDIRECT DETECTED: The URL redirects to a different host.",
        "",
        "Original URL: https://example.test/other",
        "Redirect URL: https://other.test/p",
        "Status: 301 Moved Permanently",
        "",
        "To complete your request, I need to fetch content from the redirected URL. Please use WebFetch again with these parameters:",
        '- url: "https://other.test/p"',
        '- prompt: "Summarize it"',
      ].join("\n"),
    );
    assert.equal(
      await toolResult("fetch https://example.test/missing"),
      "The server returned HTTP 404 Not Found.\nRetry-After: 30\n\nThe response body was not retrieved. If this URL requires authentication, use an authenticated tool (e.g. `gh` for GitHub, or an MCP-provided fetch tool) instead of WebFetch.",
    );
    assert.equal(
      await toolResult("fetch https://example.test/image"),
      "Unsupported WebFetch content type: image/png",
    );
    assert.equal(
      await toolResult("fetch https://127.0.0.1/"),
      "WebFetch cannot access private or local IP addresses",
    );
    assert.equal(await toolResult("fetch https://intranet/"), "Invalid URL");
    assert.equal(
      await toolResult("fetch not a url"),
      '[ { "validation": "url", "code": "invalid_string", "message": "Invalid url", "path": [ "url" ] } ]',
    );
    assert.equal(
      await toolResult("fetch bad"),
      "<tool_use_error>InputValidationError: WebFetch failed due to the following issues:\nThe required parameter `prompt` is missing\nThe parameter `url` type is expected as `string` but provided as `number`</tool_use_error>",
    );
    assert.equal(processed.length, 3);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    web.close();
    await f.close();
    await rm(certDir, { recursive: true, force: true });
  }
});
