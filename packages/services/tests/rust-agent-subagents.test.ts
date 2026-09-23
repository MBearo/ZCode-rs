import assert from "node:assert/strict";
import test from "node:test";
import { event, end, fixture, waitForFile, type Harness } from "./rust-agent-fixture.js";
import { mkdir, writeFile, access } from "node:fs/promises";
import { join } from "node:path";
import { zcodeSessionSubagentsResultSchema, zcodeSessionStateSnapshotSchema } from "@zcode/shared";

function call(response: any, calls: { id: string; name: string; args: unknown }[]) {
  event(response, {
    tool_calls: calls.map((c, index) => ({
      index,
      id: c.id,
      type: "function",
      function: { name: c.name, arguments: JSON.stringify(c.args) },
    })),
  });
  end(response, "tool_calls");
}
function text(response: any, content: string) {
  event(response, { content });
  end(response, "stop");
}
function listing(h: Harness, sid: string) {
  return h.client.request(
    "session/subagents",
    { sessionId: sid },
    zcodeSessionSubagentsResultSchema,
  );
}

test("Agent creates isolated real child sessions, runs foreground siblings concurrently and commits results in call order", async () => {
  const children: any[] = [];
  const f = await fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const last = request.messages.at(-1);
      if (last.content === "parent secret") {
        call(response, [
          {
            id: "a",
            name: "Agent",
            args: { description: "read one", prompt: "child-one", subagent_type: "Explore" },
          },
          { id: "b", name: "Agent", args: { description: "read two", prompt: "child-two" } },
        ]);
      } else if (last.content === "child-one" || last.content === "child-two") {
        assert.ok(!JSON.stringify(request.messages).includes("parent secret"));
        if (last.content === "child-one")
          assert.ok(!request.tools.some((t: any) => t.function.name === "Write"));
        children.push({ response, prompt: last.content });
        if (children.length === 2)
          for (const child of children.toReversed()) text(child.response, `${child.prompt} result`);
      } else {
        const results = request.messages.filter((m: any) => m.role === "tool");
        assert.match(results[0].content, /child-one result/);
        assert.match(results[1].content, /child-two result/);
        text(response, "parent done");
      }
    },
  });
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "parent secret" }));
    await h.completed(sid);
    const agents = await listing(h, sid);
    assert.equal(agents.ended.total, 2);
    assert.equal(agents.running.length, 0);
    const rows = (await h.rows(sid)).rows;
    const projected = rows.filter(r => r.kind === "subagent");
    assert.equal(projected.length, 2, "Agent summary requires a paired child row for the App click target");
    for (const child of agents.ended.items) {
      const row = projected.find(r => r.kind === "subagent" && r.childSessionId === child.childSessionId);
      assert.ok(row?.kind === "subagent");
      assert.equal(row.parentToolCallId, child.toolCallId);
      assert.equal(row.status, "success");
      assert.ok(rows.some(r => r.kind === "toolCall" && r.toolCallId === row.parentToolCallId && r.turnId === row.turnId));
    }
    for (const child of agents.ended.items) {
      assert.equal(child.status, "success");
      await h.subscribe(`conversation/${child.childSessionId}`);
      assert.ok(
        (await h.rows(child.childSessionId)).rows.some((r: any) => r.kind === "assistantText"),
      );
      const detail = await h.client.request(
        "session/read",
        { sessionId: child.childSessionId },
        zcodeSessionStateSnapshotSchema,
      );
      assert.equal(detail.session.sessionKind, "subagent_child");
      assert.equal(detail.session.parentSessionId, sid);
    }
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
    const cold = f.start();
    assert.equal((await listing(cold, sid)).ended.total, 2);
    assert.deepEqual((await cold.rows(sid)).rows.filter(r => r.kind === "subagent"), projected);
    await cold.close();
  } finally {
    for (const child of children) if (!child.response.writableEnded) child.response.end();
    await f.close();
  }
});

test("Subagent profile constrains dispatched tools and maxTurns, and foreign sessions cannot address it", async () => {
  let agentId = "";
  const f = await fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const last = request.messages.at(-1);
      if (last.content === "profile") {
        assert.match(
          request.tools.find((t: any) => t.function.name === "Agent").function.description,
          /limited/,
        );
        call(response, [
          {
            id: "profile-agent",
            name: "Agent",
            args: {
              description: "limited profile",
              prompt: "limited task",
              subagent_type: "limited",
            },
          },
        ]);
      } else if (last.content === "limited task") {
        assert.deepEqual(
          request.tools.map((t: any) => t.function.name),
          ["Read"],
        );
        assert.ok(request.messages.some((m: any) => m.content === "PROFILE INSTRUCTION"));
        call(response, [
          {
            id: "forbidden-write",
            name: "Write",
            args: { file_path: "forbidden.txt", content: "must not exist" },
          },
        ]);
      } else if (last.content === "foreign")
        call(response, [
          {
            id: "foreign-message",
            name: "SendMessage",
            args: { to: agentId, summary: "foreign", message: "do not inject" },
          },
        ]);
      else {
        if (last.tool_call_id === "profile-agent") {
          assert.match(last.content, /maxTurns/);
          agentId = last.content.match(/agentId: (agent_[^\s]+)/)?.[1] ?? "";
        } else assert.match(last.content, /Task unavailable in this session/);
        text(response, "done");
      }
    },
  });
  try {
    await mkdir(join(f.cwd, ".zcode/agents"), { recursive: true });
    await writeFile(
      join(f.cwd, ".zcode/agents/limited.md"),
      "---\nname: limited\ndescription: limited test profile\ntools:\n  - Read\nmaxTurns: 1\n---\nPROFILE INSTRUCTION",
    );
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "profile" }));
    await h.completed(sid);
    assert.match(agentId, /^agent_/);
    await assert.rejects(access(join(f.cwd, "forbidden.txt")));
    const other = await h.create();
    await h.subscribe(`conversation/${other}`);
    await h.command(h.envelope("sendText", other, { text: "foreign" }));
    await h.completed(other);
    assert.equal((await listing(h, other)).childSessionIds.length, 0);
    assert.equal((await listing(h, sid)).ended.items[0]?.status, "failed");
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});

test(
  "Stopping the parent waits for its foreground child Shell to exit and cold history marks the child cancelled",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture({
      respond(request, response) {
        response.writeHead(200, { "Content-Type": "text/event-stream" });
        const last = request.messages.at(-1);
        if (last.content === "delegate shell")
          call(response, [
            {
              id: "shell-agent",
              name: "Agent",
              args: { description: "long shell", prompt: "shell child" },
            },
          ]);
        else if (last.content === "shell child")
          call(response, [
            {
              id: "long-shell",
              name: "Bash",
              args: { command: "echo $$ > subagent.pid; sleep 30" },
            },
          ]);
        else text(response, "done");
      },
    });
    try {
      const h = f.start();
      const sid = await h.create();
      await h.subscribe(`conversation/${sid}`);
      await h.command(h.envelope("sendText", sid, { text: "delegate shell" }));
      const pid = Number(await waitForFile(join(f.cwd, "subagent.pid")));
      process.kill(pid, 0);
      assert.equal((await listing(h, sid)).running.length, 1);
      await h.command(h.envelope("stop", sid));
      await h.wait((m) =>
        m.params?.frame?.payload?.deltas?.some(
          (d: any) => d.patch?.control?.phase === "completedInterrupted",
        ),
      );
      assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
      assert.equal((await listing(h, sid)).ended.items[0]?.status, "cancelled");
      assert.equal((await h.rows(sid)).rows.find(r => r.kind === "subagent")?.status, "cancelled");
      assert.equal(f.requests.length, 2);
      await h.close();
      const cold = f.start();
      assert.equal((await listing(cold, sid)).ended.items[0]?.status, "cancelled");
      await cold.close();
    } finally {
      await f.close();
    }
  },
);

test("Background Agent completion is delivered through the parent continuation and SendMessage resumes the same child", async () => {
  let agentId = "";
  let resumed = false;
  const f = await fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const last = request.messages.at(-1);
      if (last.content === "delegate")
        call(response, [
          {
            id: "launch",
            name: "Agent",
            args: {
              description: "background check",
              prompt: "background child",
              run_in_background: true,
            },
          },
        ]);
      else if (last.content === "background child") text(response, "child evidence");
      else if (last.content === "continue child")
        call(response, [
          {
            id: "message",
            name: "SendMessage",
            args: { to: agentId, summary: "follow up", message: "continue with prior evidence" },
          },
        ]);
      else if (last.content.includes?.("continue with prior evidence")) {
        resumed = true;
        assert.ok(request.messages.some((m: any) => m.content === "child evidence"));
        text(response, "follow up evidence");
      } else {
        const launch = request.messages.find(
          (m: any) => m.role === "tool" && m.tool_call_id === "launch",
        );
        if (launch) agentId = launch.content.match(/agentId: (agent_[^\s]+)/)?.[1] ?? agentId;
        text(
          response,
          last.content.includes?.("task-notification") ? "reported child result" : "waiting",
        );
      }
    },
  });
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "delegate" }));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some((d: any) => d.row?.text === "reported child result"),
    );
    assert.match(agentId, /^agent_/);
    const before = await listing(h, sid);
    assert.equal(before.ended.total, 1);
    const originalRow = (await h.rows(sid)).rows.find(r => r.kind === "subagent");
    assert.ok(originalRow?.kind === "subagent");
    await h.command(h.envelope("sendText", sid, { text: "continue child" }));
    const notification = await h.wait(
      (m) =>
        m.params?.topic === `conversation/${sid}` &&
        m.params.frame?.payload?.deltas?.some(
          (d: any) => d.row?.kind === "userInput" && d.row.text.includes("follow up evidence"),
        ),
    );
    const turn = notification.params.frame.payload.deltas.find(
      (d: any) => d.row?.kind === "userInput" && d.row.text.includes("follow up evidence"),
    ).row.turnId;
    await h.wait(
      (m) =>
        m.params?.topic === `conversation/${sid}` &&
        m.params.frame?.payload?.deltas?.some(
          (d: any) =>
            d.row?.kind === "turnHeader" &&
            d.row.turnId === turn &&
            d.row.state === "completedSuccess",
        ),
    );
    assert.ok(resumed);
    assert.deepEqual((await listing(h, sid)).childSessionIds, before.childSessionIds);
    const resumedRows = (await h.rows(sid)).rows.filter(r => r.kind === "subagent");
    assert.equal(resumedRows.length, 1);
    assert.equal(resumedRows[0]?.rowId, originalRow.rowId);
    assert.equal(resumedRows[0]?.parentToolCallId, "launch");
    assert.equal(resumedRows[0]?.status, "success");
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});

test("Custom child inherits one live MCP connection and preloads only its declared Skill and memory", async () => {
  const { stdioServer } = await import("./rust-agent-mcp-fixture.js");
  const { readFile } = await import("node:fs/promises");
  const f = await fixture({
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      const last = req.messages.at(-1);
      if (last.content === "integrated parent")
        call(res, [
          {
            id: "launch",
            name: "Agent",
            args: {
              description: "integrated child",
              prompt: "integrated child",
              subagent_type: "worker",
            },
          },
        ]);
      else if (last.content === "integrated child") {
        assert(JSON.stringify(req.messages).includes("DECLARED_SKILL_BODY"));
        assert(JSON.stringify(req.messages).includes("EXISTING_AGENT_MEMORY"));
        assert(req.tools.some((t: any) => t.function.name === "mcp__local_echo__echo"));
        assert(!req.tools.some((t: any) => t.function.name === "Bash"));
        call(res, [
          { id: "echo", name: "mcp__local_echo__echo", args: { text: "child MCP evidence" } },
        ]);
      } else if (last.tool_call_id === "echo") {
        assert.match(last.content, /child MCP evidence/);
        text(res, "child extension result");
      } else {
        text(res, "integrated done");
      }
    },
  });
  try {
    const server = await stdioServer(f.root);
    await mkdir(join(f.cwd, ".zcode/agents"), { recursive: true });
    await mkdir(join(f.cwd, ".zcode/skills/declared"), { recursive: true });
    await mkdir(join(f.cwd, ".zcode/agent-memory/worker"), { recursive: true });
    await writeFile(
      join(f.cwd, ".zcode/agents/worker.md"),
      "---\nname: worker\ndescription: integrated worker\ntools: [mcp__local_echo__echo]\nskills: [declared]\nmemory: project\n---\nUse the declared instructions.",
    );
    await writeFile(
      join(f.cwd, ".zcode/skills/declared/SKILL.md"),
      "---\nname: declared\ndescription: declared skill\n---\nDECLARED_SKILL_BODY",
    );
    await writeFile(join(f.cwd, ".zcode/agent-memory/worker/MEMORY.md"), "EXISTING_AGENT_MEMORY");
    const h = f.start();
    const ack = await h.command(
      h.envelope("createSession", null, { workspaceId: f.cwd, mcpServers: [server.config] }),
    );
    const sid = (ack.result as any).sessionId;
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "integrated parent" }));
    await h.completed(sid);
    assert.equal((await readFile(server.started, "utf8")).trim().split("\n").length, 1);
    assert.equal((await listing(h, sid)).ended.items[0]?.status, "success");
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});

test("Parent file rewind includes its owned child write checkpoint", async () => {
  const { readFile } = await import("node:fs/promises");
  const { v4ConversationFileRewindPreviewResultSchema } =
    await import("@zcode/shared/zcode-protocol-v4");
  const f = await fixture({
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      const last = req.messages.at(-1);
      if (last.content === "delegate write")
        call(res, [
          {
            id: "launch",
            name: "Agent",
            args: { description: "write file", prompt: "child write" },
          },
        ]);
      else if (last.content === "child write")
        call(res, [
          {
            id: "write",
            name: "Write",
            args: { file_path: "child.txt", content: "owned child file" },
          },
        ]);
      else {
        text(res, "done");
      }
    },
  });
  try {
    const h = f.start(),
      sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("sendText", sid, { text: "delegate write" }));
    await h.completed(sid);
    assert.equal(await readFile(join(f.cwd, "child.txt"), "utf8"), "owned child file");
    const rows = await h.rows(sid),
      user = rows.rows.find((r) => r.kind === "userInput")!;
    const p = {
      sessionId: sid,
      target: { rowId: user.rowId, entityId: user.entityId },
      baseRevision: rows.atRevision,
      baseLogEpoch: rows.atLogEpoch,
    };
    const preview = await h.client.request(
      "v4/conversation/fileRewindPreview",
      p,
      v4ConversationFileRewindPreviewResultSchema,
    );
    assert.equal(preview.safeFiles.length, 1);
    const ack = await h.command({
      ...h.envelope("applyFileRewind", sid, { target: p.target }),
      baseRevision: p.baseRevision,
      baseLogEpoch: p.baseLogEpoch,
    });
    assert.equal((ack.result as any).applied, true);
    await assert.rejects(access(join(f.cwd, "child.txt")));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
