import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { z } from "zod";
import { event, end, fixture, waitForFile } from "./zcode-cli-rust-fixture.js";
import { httpServer, listMcp, stdioServer } from "./zcode-cli-rust-mcp-fixture.js";

function respond(request: any, response: any) {
  response.writeHead(200, { "Content-Type": "text/event-stream" });
  if (request.messages.at(-1).role !== "tool") {
    const name = request.tools.find((t: any) => t.function.name.startsWith("mcp__"))?.function.name;
    event(response, {
      tool_calls: [
        {
          index: 0,
          id: "mcp-call",
          type: "function",
          function: {
            name: name ?? "missing_mcp",
            arguments: JSON.stringify({ text: "echo input" }),
          },
        },
      ],
    });
    end(response, "tool_calls");
  } else {
    event(response, { content: "done" });
    end(response, "stop");
  }
}

test("MCP stdio discovery and execution use session isolation, explicit workspace reuse and status-only observation", async () => {
  const f = await fixture({ respond });
  try {
    const server = await stdioServer(f.root);
    const h = f.start();
    const statuses = await listMcp(h, f.cwd, [server.config]);
    assert.equal(statuses.statuses[server.config.name]?.status, "connected");
    const before = await readFile(server.started, "utf8");
    assert.equal(
      (await listMcp(h, f.cwd, [], "status")).statuses[server.config.name]?.toolCount,
      1,
    );
    assert.equal(await readFile(server.started, "utf8"), before);
    const outputs: any[] = [];
    for (const isolation of ["session", "session", "workspace", "workspace"]) {
      const ack = await h.command(
        h.envelope("createSession", null, {
          workspaceId: f.cwd,
          mcpServers: [{ ...server.config, isolation }],
        }),
      );
      const sid = (ack.result as { sessionId: string }).sessionId;
      await h.subscribe(`conversation/${sid}`);
      await h.command(h.envelope("sendText", sid, { text: "call MCP" }));
      await h.completed(sid);
      outputs.push(JSON.parse(f.requests.at(-1)!.messages.at(-1).content));
    }
    assert.notEqual(outputs[0].pid, outputs[1].pid);
    assert.equal(outputs[2].pid, outputs[3].pid);
    assert.equal(outputs[0].probe, "scoped");
    assert.equal(
      f.requests[0]!.tools.find((t: any) => t.function.name.startsWith("mcp__")).function.name,
      "mcp__local_echo__echo",
    );
    assert.deepEqual((await listMcp(h, f.cwd, [])).statuses, {});
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
    for (const pid of (await readFile(server.started, "utf8")).trim().split("\n").map(Number))
      assert.throws(() => process.kill(pid, 0));
  } finally {
    await f.close();
  }
});

for (const transport of ["http", "sse"] as const)
  test(`MCP ${transport} uses configured headers and carries tool results into the next model request`, async () => {
    const f = await fixture({ respond });
    const server = await httpServer(transport);
    try {
      const h = f.start();
      const ack = await h.command(
        h.envelope("createSession", null, { workspaceId: f.cwd, mcpServers: [server.config] }),
      );
      const sid = (ack.result as { sessionId: string }).sessionId;
      await h.subscribe(`conversation/${sid}`);
      await h.command(h.envelope("sendText", sid, { text: "MCP HTTP" }));
      await h.completed(sid);
      assert.match(f.requests[1]!.messages.at(-1).content, /HTTP echo input/);
      assert.match(f.requests[1]!.messages.at(-1).content, /echoed/);
      assert.ok(server.calls.every((c) => c.headers["x-fixture"] === "configured"));
      assert.equal(server.calls.filter((c) => c.method === "tools/call").length, 1);
      assert.deepEqual(h.schemaErrors, []);
      await h.close();
    } finally {
      try {
        await f.close();
      } finally {
        await server.close();
      }
    }
  });

test("MCP handshake does not block the actor; stop cancels discovery and reaps the server before finishing", async () => {
  const f = await fixture({ respond });
  try {
    const server = await stdioServer(f.root, "slow-connect");
    const h = f.start();
    const ack = await h.command(
      h.envelope("createSession", null, {
        workspaceId: f.cwd,
        mcpServers: [{ ...server.config, protocolVersion: "legacy" }],
      }),
    );
    const sid = (ack.result as { sessionId: string }).sessionId;
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "connect" }));
    const pid = Number((await waitForFile(server.started)).trim());
    const start = Date.now();
    await h.client.request("runtime/capabilities", {}, z.unknown());
    assert.ok(Date.now() - start < 1000);
    await h.command(h.envelope("stop", sid));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) => d.patch?.control?.phase === "completedInterrupted",
      ),
    );
    assert.equal(f.requests.length, 0);
    assert.throws(() => process.kill(pid, 0));
    await h.close();
  } finally {
    await f.close();
  }
});

test("MCP cancellation after dispatch does not replay the tool and waits for process cleanup", async () => {
  const f = await fixture({ respond });
  try {
    const server = await stdioServer(f.root, "slow-call");
    const h = f.start();
    const ack = await h.command(
      h.envelope("createSession", null, {
        workspaceId: f.cwd,
        mcpServers: [{ ...server.config, protocolVersion: "legacy" }],
      }),
    );
    const sid = (ack.result as { sessionId: string }).sessionId;
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "run once" }));
    await waitForFile(server.calls);
    const pid = Number((await readFile(server.started, "utf8")).trim());
    await h.command(h.envelope("stop", sid));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) => d.patch?.control?.phase === "completedInterrupted",
      ),
    );
    assert.equal(f.requests.length, 1);
    assert.throws(() => process.kill(pid, 0));
    assert.equal((await readFile(server.calls, "utf8")).trim().split("\n").length, 1);
    await h.close();
    const cold = f.start();
    await cold.subscribe(`conversation/${sid}`);
    assert.equal(f.requests.length, 1);
    await cold.close();
    assert.equal((await readFile(server.calls, "utf8")).trim().split("\n").length, 1);
  } finally {
    await f.close();
  }
});

test("MCP status classifies unsupported authentication without exposing credentials or connecting", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const result = await listMcp(h, f.cwd, [
      {
        name: "oauth",
        type: "http",
        url: "http://127.0.0.1:1/mcp",
        headers: [],
        oauth: { type: "authorization_code", clientSecret: "fixture-secret-never-echo" },
      },
    ]);
    assert.equal(result.statuses.oauth?.failureKind, "not_authenticated");
    assert.ok(!JSON.stringify(result).includes("fixture-secret-never-echo"));
    assert.ok(!h.stderr.includes("fixture-secret-never-echo"));
    await h.close();
  } finally {
    await f.close();
  }
});

test("Invalid persisted MCP configuration reports config_invalid without blocking the normal Agent loop", async () => {
  const { mkdir, writeFile } = await import("node:fs/promises");
  const { join } = await import("node:path");
  const f = await fixture({
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: "normal model remains available" });
      end(res, "stop");
    },
  });
  try {
    await mkdir(join(f.cwd, ".zcode"));
    await writeFile(
      join(f.cwd, ".zcode/config.json"),
      JSON.stringify({
        mcp: {
          servers: {
            broken: { type: "http", url: "${MISSING_URL}" },
            disabled: { type: "stdio", enabled: false },
          },
        },
      }),
    );
    const h = f.start(),
      sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "normal" }));
    await h.completed(sid);
    const status = await listMcp(h, f.cwd, undefined, "status");
    assert.equal(status.statuses.broken?.failureKind, "config_invalid");
    assert.equal(status.statuses.disabled?.status, "disabled");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
