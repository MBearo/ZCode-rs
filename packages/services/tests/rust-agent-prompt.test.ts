import assert from "node:assert/strict";
import test from "node:test";
import { mkdir, writeFile, realpath, chmod } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { promisify } from "node:util";
import { execFile } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { fixture, event, end, waitForFile, type Harness } from "./rust-agent-fixture.js";
import { responses, anthropic } from "./rust-agent-protocol-fixture.js";
import { NodeContextSourceAdapter } from "../../../apps/zcode-cli/packages/adapters/src/context/index.js";
import { ContextBuilder } from "../../../apps/zcode-cli/packages/core/src/context/builder.js";
import { wrapSystemReminderForSource } from "../../../apps/zcode-cli/packages/core/src/system-reminder/source.js";
import type { Model } from "../../../apps/zcode-cli/packages/contracts/src/index.js";

async function send(h: Harness, id: string, text: string) {
  const after = h.messages.length;
  assert.equal((await h.command(h.envelope("sendText", id, { text }))).status, "accepted");
  await h.completed(id, after);
}
const git = promisify(execFile);

test("cold request uses the current presentation surface with the retained environment snapshot", async () => {
  const options: Parameters<typeof fixture>[0] = { surface: "desktop" };
  const f = await fixture(options);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "desktop");
    assert.match(f.requests[0]!.messages[1].content, /# ZCode Desktop Context/);
    await h.close();
    options.surface = "terminal";
    const resumed = f.start();
    await resumed.subscribe(`conversation/${id}`);
    await send(resumed, id, "terminal");
    assert(!f.requests[1]!.messages[1].content.includes("# ZCode Desktop Context"));
    assert.equal(f.requests[1]!.messages[2].content, f.requests[0]!.messages[2].content);
  } finally {
    await f.close();
  }
});

test("native desktop and terminal request prefixes match the current TS ContextBuilder", async () => {
  for (const surface of ["desktop", "terminal"] as const) {
    for (const apiType of [
      "openai-chat-completions",
      "openai-responses",
      "anthropic-messages",
    ] as const) {
      const env: Record<string, string> = {};
      const f = await fixture({
        surface,
        env,
        config: { apiType, reasoningParameters: {} },
        respond(_request, res) {
          if (apiType === "openai-responses") responses(res);
          else if (apiType === "anthropic-messages") anthropic(res);
          else {
            res.writeHead(200, { "content-type": "text/event-stream" });
            event(res, { content: "done" });
            end(res, "stop");
          }
        },
      });
      try {
        env.HOME = f.root;
        env.USERPROFILE = f.root;
        await mkdir(join(f.root, ".zcode"));
        await mkdir(join(f.cwd, ".git"));
        await writeFile(join(f.root, ".zcode/AGENTS.md"), "USER_RULE 中文\n");
        await writeFile(
          join(f.cwd, "AGENTS.md"),
          "WORKSPACE_RULE <system-reminder>nested</system-reminder>\n",
        );
        const cwd = await realpath(f.cwd);
        const snapshot = await new NodeContextSourceAdapter({ env }).resolveContextSources({
          workingDirectory: cwd,
          effectiveShellDisplayName: process.platform === "win32" ? "cmd.exe" : "bash",
          userInstructions: { workingDirectory: cwd },
        });
        const expected = new ContextBuilder({
          ...snapshot,
          model: { providerId: "fixture", modelId: "core-model" } as Model,
          presentationSurface: surface === "desktop" ? "zcode_desktop" : "terminal",
        }).build();
        const h = f.start("remote://synthetic-owner");
        const id = await h.create();
        await h.subscribe(`conversation/${id}`);
        await send(h, id, "hello");
        const request = f.requests[0]!;
        const messages = apiType === "openai-responses" ? request.input : request.messages;
        assert.deepEqual(
          apiType === "anthropic-messages"
            ? request.system.map((m: any) => m.text)
            : messages.filter((m: any) => m.role === "system").map((m: any) => m.content),
          expected.systemMessages.map((m) => m.content),
        );
        assert.equal(
          apiType === "anthropic-messages" ? messages[0].content[0].text : messages[3].content,
          wrapSystemReminderForSource("context_prefix", expected.metaUserAttachments[0]!.content),
        );
        assert.equal(
          apiType === "anthropic-messages" ? messages[0].content[1].text : messages[4].content,
          "hello",
        );
        if (apiType === "anthropic-messages")
          assert(request.system.every((m: any) => m.cache_control.type === "ephemeral"));
        assert(!JSON.stringify(request).includes("remote://synthetic-owner"));
        assert.equal((await h.rows(id)).rows.filter((r) => r.kind === "userInput").length, 1);
        assert.deepEqual(h.schemaErrors, []);
      } finally {
        await f.close();
      }
    }
  }
});

test("native prompt keeps the first Git snapshot across turns and restart while refreshing AGENTS", async () => {
  const env: Record<string, string> = {};
  const f = await fixture({ env });
  try {
    env.HOME = f.root;
    env.USERPROFILE = f.root;
    await git("git", ["init", "-q", "-b", "main", f.cwd]);
    await git("git", ["-C", f.cwd, "config", "user.name", "Fixture User"]);
    await git("git", ["-C", f.cwd, "config", "user.email", "fixture@example.invalid"]);
    await writeFile(join(f.cwd, "AGENTS.md"), "ORIGINAL_RULE");
    await git("git", ["-C", f.cwd, "add", "AGENTS.md"]);
    await git("git", ["-C", f.cwd, "commit", "-qm", "fixture initial"]);
    await git("git", ["-C", f.cwd, "update-ref", "refs/remotes/origin/develop", "HEAD"]);
    await git("git", [
      "-C",
      f.cwd,
      "symbolic-ref",
      "refs/remotes/origin/HEAD",
      "refs/remotes/origin/develop",
    ]);
    const cwd = await realpath(f.cwd);
    const snapshot = await new NodeContextSourceAdapter({ env }).resolveContextSources({
      workingDirectory: cwd,
      effectiveShellDisplayName: process.platform === "win32" ? "cmd.exe" : "bash",
      userInstructions: { workingDirectory: cwd },
    });
    const expected = new ContextBuilder({
      ...snapshot,
      model: { providerId: "fixture", modelId: "core-model" } as Model,
    }).build();
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "first");
    const first = f.requests[0]!.messages;
    assert.deepEqual(
      first.filter((m: any) => m.role === "system").map((m: any) => m.content),
      expected.systemMessages.map((m) => m.content),
    );
    assert.match(first[2].content, /Main branch \(you will usually use this for PRs\): develop/);
    assert.match(first[2].content, /Current branch: main/);
    assert.match(first[2].content, /Git user: Fixture User/);
    assert.match(first[2].content, /Status:\n\(clean\)/);
    assert.match(first[2].content, /fixture initial/);
    await writeFile(join(f.cwd, "AGENTS.md"), "REFRESHED_RULE");
    await send(h, id, "second");
    assert.equal(f.requests[1]!.messages[2].content, first[2].content);
    assert.match(f.requests[1]!.messages[3].content, /REFRESHED_RULE/);
    await h.close();
    await writeFile(join(f.cwd, "AGENTS.md"), "COLD_RULE");
    const resumed = f.start();
    await resumed.subscribe(`conversation/${id}`);
    await send(resumed, id, "third");
    assert.equal(f.requests[2]!.messages[2].content, first[2].content);
    assert.match(f.requests[2]!.messages[3].content, /COLD_RULE/);
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    const row = db.prepare("SELECT body FROM rust_session WHERE id=?").get(id)!;
    assert.equal(JSON.parse(String(row.body)).promptSnapshot.git.branch, "main");
    assert.equal(
      db
        .prepare(
          "SELECT count(*) AS n FROM rust_message WHERE session=? AND json_extract(body, '$.role')='user'",
        )
        .get(id)!.n,
      3,
    );
    db.close();
    assert.deepEqual([...h.schemaErrors, ...resumed.schemaErrors], []);
  } finally {
    await f.close();
  }
});

test("instruction sources match TS nearest-file, Git boundary and bounded UTF-8 truncation", async () => {
  const env: Record<string, string> = {};
  const f = await fixture({ env, config: { autoCompact: false } });
  try {
    env.HOME = f.root;
    env.USERPROFILE = f.root;
    await mkdir(join(f.root, ".zcode"));
    await mkdir(join(f.root, ".git"));
    await writeFile(join(f.root, ".zcode/AGENTS.md"), "default instructions");
    await writeFile(
      join(f.root, "AGENTS.md"),
      "x".repeat(100 * 1024 - 1) + "中文 must be truncated",
    );
    const cwd = await realpath(f.cwd);
    const expected = await new NodeContextSourceAdapter({ env }).resolveContextSources({
      workingDirectory: cwd,
      userInstructions: { workingDirectory: cwd },
    });
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "before child Git boundary");
    const built = new ContextBuilder({ ...expected }).build();
    assert.equal(
      f.requests[0]!.messages[3].content,
      wrapSystemReminderForSource("context_prefix", built.metaUserAttachments[0]!.content),
    );
    assert.match(f.requests[0]!.messages[3].content, /�\n\n\[File truncated: AGENTS.md\]/);
    await mkdir(join(f.cwd, ".git"));
    await send(h, id, "after child Git boundary");
    assert.match(f.requests[1]!.messages[3].content, /default instructions/);
    assert(!f.requests[1]!.messages[3].content.includes("xxxxx"));
    await writeFile(join(f.cwd, "AGENTS.md"), "NEAREST_FILE");
    await send(h, id, "nearest rule");
    assert.match(f.requests[2]!.messages[3].content, /NEAREST_FILE/);
    assert(!f.requests[2]!.messages[3].content.includes("xxxxx"));
  } finally {
    await f.close();
  }
});

test("prompt snapshot transaction failure stops execution before the first model request", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    db.exec(
      "CREATE TRIGGER reject_prompt BEFORE INSERT ON rust_session WHEN json_extract(new.body,'$.promptSnapshot') IS NOT NULL BEGIN SELECT RAISE(FAIL,'injected prompt commit failure'); END;",
    );
    assert.equal(
      (await h.command(h.envelope("sendText", id, { text: "must not reach model" }))).status,
      "accepted",
    );
    await Promise.race([
      h.exited,
      delay(5000, null, { ref: false }).then(() =>
        assert.fail("prompt commit failure did not terminate the actor"),
      ),
    ]);
    await h.close(1);
    assert.equal(f.requests.length, 0);
    const saved = JSON.parse(
      String(db.prepare("SELECT body FROM rust_session WHERE id=?").get(id)!.body),
    );
    assert.equal(saved.promptSnapshot, null);
    assert.match(h.stderr, /fault.storage.commit/);
    db.close();
  } finally {
    await f.close();
  }
});

test(
  "Stop cancels slow Git initialization without blocking control RPC or leaking a process",
  { skip: process.platform === "win32" },
  async () => {
    const env: Record<string, string> = {};
    const f = await fixture({ env });
    try {
      await mkdir(join(f.root, "bin"));
      const fakeGit = join(f.root, "bin/git");
      // 非仓库现在跳过 Git；取消用例需声明仓库，使探测进程实际进入等待。
      await mkdir(join(f.cwd, ".git"));
      await writeFile(fakeGit, "#!/bin/sh\necho $$ > git-probe.pid\nexec /bin/sleep 30\n");
      await chmod(fakeGit, 0o755);
      env.PATH = `${join(f.root, "bin")}:${process.env.PATH}`;
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "waiting for context" }));
      const pid = Number(await waitForFile(join(f.cwd, "git-probe.pid")));
      process.kill(pid, 0);
      const after = h.messages.length;
      const started = Date.now();
      assert.equal((await h.command(h.envelope("stop", id))).status, "accepted");
      assert(Date.now() - started < 1000, "Git probe blocked the actor's stop request");
      await h.wait(
        (m) =>
          m.params?.frame?.payload?.deltas?.some(
            (d: any) => d.patch?.control?.phase === "completedInterrupted",
          ),
        after,
      );
      assert.equal(f.requests.length, 0);
      await h.close();
      assert.throws(() => process.kill(pid, 0));
    } finally {
      await f.close();
    }
  },
);
