import assert from "node:assert/strict";
import test from "node:test";
import { readFile, writeFile, access } from "node:fs/promises";
import { join } from "node:path";
import { fixture, event, end, waitForFile } from "./zcode-cli-rust-fixture.js";
import { DatabaseSync } from "node:sqlite";

function call(res: any, name: string, args: unknown, id = name) {
  event(res, {
    tool_calls: [
      { index: 0, id, type: "function", function: { name, arguments: JSON.stringify(args) } },
    ],
  });
  end(res, "tool_calls");
}
function done(res: any) {
  event(res, { content: "done" });
  end(res, "stop");
}
function prompt(req: any) {
  return req.messages.findLast(
    // fixture 根据真实输入选择工具；后台通知和 Todo 提醒不能被当成新的测试指令。
    (m: any) => m.role === "user" && !/^<(?:task-notification|system-reminder)>/.test(m.content),
  )?.content;
}

test(
  "Rust coding path searches, reads, edits and runs a background test through App schemas",
  { skip: process.platform === "win32" },
  async () => {
    let step = 0;
    let taskId = "";
    const f = await fixture({
      respond(req, res) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        const last = req.messages.at(-1);
        assert(!last?.content?.startsWith("Tool failed"), last?.content);
        switch (step++) {
          case 0:
            call(res, "Glob", { pattern: "**/*.txt" });
            break;
          case 1:
            assert.match(last.content, /sample.txt/);
            call(res, "Grep", { pattern: "wrong", glob: "*.txt", output_mode: "content" });
            break;
          case 2:
            assert.match(last.content, /sample.txt:1:wrong/);
            call(res, "Read", { file_path: "sample.txt" });
            break;
          case 3:
            assert.match(last.content, /1\twrong/);
            call(res, "Edit", {
              file_path: "sample.txt",
              old_string: "wrong",
              new_string: "right",
            });
            break;
          case 4:
            call(res, "Bash", {
              command: 'test "$(cat sample.txt)" = right && printf passed',
              run_in_background: true,
              description: "Check edited file",
            });
            break;
          case 5:
            taskId = JSON.parse(last.content).backgroundTaskId;
            assert(taskId);
            call(res, "TaskOutput", { task_id: taskId, block: true, timeout: 5000 });
            break;
          default: {
            const result = JSON.parse(last.content);
            assert.equal(result.retrieval_status, "success");
            assert.equal(result.task.exitCode, 0);
            assert.match(result.task.output, /passed/);
            done(res);
          }
        }
      },
    });
    try {
      await writeFile(join(f.cwd, "sample.txt"), "wrong\n");
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.subscribe(`conversation/${id}`, "mobile", "web-remote-replayable");
      await h.command(h.envelope("sendText", id, { text: "fix and test", mode: "yolo" }));
      await h.completed(id);
      assert.equal(await readFile(join(f.cwd, "sample.txt"), "utf8"), "right\n");
      const rows = (await h.rows(id)).rows;
      assert(rows.some((r) => r.kind === "toolCall" && r.output?.display?.kind === "file_diff"));
      assert(rows.some((r) => r.kind === "toolCall" && r.output?.display?.kind === "task_output"));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  },
);

test(
  "Rust background tasks survive foreground completion, isolate sessions and stop process trees",
  { skip: process.platform === "win32" },
  async () => {
    let taskId = "";
    let outputFile = "";
    let output: any;
    const f = await fixture({
      respond(req, res) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        const last = req.messages.at(-1);
        const action = prompt(req);
        if (last.role !== "tool") {
          if (action === "start")
            call(res, "Bash", {
              command: "echo $$ > background.pid; echo started; sleep 30",
              run_in_background: true,
            });
          else if (action === "stop") call(res, "TaskStop", { task_id: taskId });
          else call(res, "TaskOutput", { task_id: taskId, block: action === "wait", timeout: 25 });
        } else {
          if (action === "start") {
            const result = JSON.parse(last.content);
            taskId = result.backgroundTaskId;
            outputFile = result.persistedOutputPath;
          } else output = last.content;
          done(res);
        }
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      const send = async (session: string, text: string) => {
        const after = h.messages.length;
        await h.command(h.envelope("sendText", session, { text }));
        await h.completed(session, after);
      };
      await send(id, "start");
      assert(taskId);
      await access(outputFile);
      const pid = Number(await readFile(join(f.cwd, "background.pid"), "utf8"));
      process.kill(pid, 0);
      await send(id, "wait");
      assert.equal(JSON.parse(output).retrieval_status, "timeout");
      const other = await h.create();
      await h.subscribe(`conversation/${other}`);
      await send(other, "foreign");
      assert.match(output, /Task unavailable in this session/);
      await send(id, "stop");
      assert.match(output, /stopped/);
      assert(
        h.messages.some((m) =>
          m.params?.frame?.payload?.deltas?.some(
            (d: any) => d.patch?.backgroundWorks?.length === 0,
          ),
        ),
      );
      assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
      await send(id, "stop");
      assert.match(output, /stopped/);
      await send(id, "poll");
      assert.equal(JSON.parse(output).task.status, "killed");
      await send(id, "start");
      const nextPid = Number(await readFile(join(f.cwd, "background.pid"), "utf8"));
      const afterCancel = h.messages.length;
      assert.equal(
        (await h.command(h.envelope("cancelBackgroundWork", id, { workId: taskId }))).status,
        "accepted",
      );
      await h.wait(
        (m) =>
          m.params?.frame?.payload?.deltas?.some(
            (d: any) => d.patch?.backgroundWorks?.length === 0,
          ),
        afterCancel,
      );
      assert.throws(() => process.kill(nextPid, 0), { code: "ESRCH" });

      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  },
);

test("Rust old native build sessions require explicit yolo selection after cold recovery", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create("hello");
    await h.subscribe(`conversation/${id}`);
    await h.completed(id);
    await h.close();
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    try {
      db.exec("UPDATE rust_session SET body = json_remove(body, '$.mode')");
    } finally {
      db.close();
    }
    const restored = f.start();
    await restored.subscribe(`conversation/${id}`);
    assert.equal(
      (await restored.command(restored.envelope("sendText", id, { text: "write" }))).status,
      "rejected",
    );
    await assert.rejects(access(join(f.cwd, "result.txt")));
    assert.equal(
      (await restored.command(restored.envelope("switchCollaborationMode", id, { mode: "yolo" })))
        .status,
      "accepted",
    );
    const after = restored.messages.length;
    const command = restored.envelope("sendText", id, { text: "write" });
    assert.equal((await restored.command(command)).status, "accepted");
    await restored.wait(
      (m) =>
        m.params?.frame?.payload?.deltas?.some(
          (d: any) =>
            d.row?.kind === "turnHeader" &&
            d.row.sourceCommandId === command.commandId &&
            d.row.state === "completedSuccess",
        ),
      after,
    );
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "written by Rust");
    assert.deepEqual(restored.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test(
  "Rust EOF reaps background tasks and persists terminal state for cold history",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture({
      respond(req, res) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        if (req.messages.at(-1).role !== "tool")
          call(res, "Bash", { command: "echo $$ > child.pid; sleep 30", run_in_background: true });
        else done(res);
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "background" }));
      await h.completed(id);
      // 前台回复结束只证明后台登记成功；等实际 Shell 写出 pid 后，才能验证 EOF 回收进程。
      const pid = Number(await waitForFile(join(f.cwd, "child.pid")));
      process.kill(pid, 0);
      await h.close();
      assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
      const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
      try {
        const body = JSON.parse(
          db.prepare("SELECT body FROM rust_session WHERE id = ?").get(id)!.body as string,
        );
        assert(Object.values(body.background).every((t: any) => t.status !== "running"));
      } finally {
        db.close();
      }
      const recovered = f.start();
      await recovered.subscribe(`conversation/${id}`);
      assert.deepEqual(recovered.schemaErrors, []);
    } finally {
      await f.close();
    }
  },
);
