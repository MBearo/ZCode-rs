// Run with TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json node --import tsx.
// Cross-runtime acceptance (spec rust-m11-node-storage §11 scenarios 4–6) with the real
// Node CLI (`zcode.cjs`) and the Rust binary over one session database:
//   4. Node and Rust continue one session in turn (text and tool turns);
//   5. either runtime is killed while streaming or running a tool; the other resumes it
//      interrupted and answers the queued command as discarded;
//   6. both processes write their own sessions at the same time.
// After each scenario Node's readers and Rust's cold reading are compared session by session.
import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { compareDatabase } from "./zcode-cli-rust-node-compare.mjs";
import {
  modelServer,
  prepareHome,
  startRuntime,
  transcript,
} from "./zcode-cli-rust-interop-runtime.mjs";

const bundle = resolve(process.argv[2] ?? "apps/zcode-cli/packages/cli/dist/zcode.cjs");
const binary = resolve(process.argv[3] ?? "apps/zcode-cli-rust/target/release/zcode-cli-rust");
const reader = resolve(process.argv[4] ?? "apps/zcode-cli-rust/target/release/examples/node_read");

async function scenario(name, run) {
  const root = await mkdtemp(join(tmpdir(), `zcode-interop-${name}-`));
  const model = await modelServer();
  try {
    const env = await prepareHome(root, model.baseUrl);
    const workspace = async (label) => {
      const cwd = join(root, label);
      await mkdir(cwd, { recursive: true });
      await writeFile(join(cwd, "note.txt"), "interop note\n");
      return cwd;
    };
    const start = (kind, cwd) =>
      startRuntime(kind, { bundle, binary, cwd, env, dataDir: join(root, "rust-data") });
    const result = await run({ start, workspace, model, root });
    const compared = await compareDatabase(
      join(root, "db.sqlite"),
      join(env.HOME, ".zcode/cli/artifacts"),
      reader,
    );
    const equal = compared.equal === compared.read && !compared.rustFailed && !compared.nodeFailed;
    console.log(
      JSON.stringify({ scenario: name, ...result, compared: equal ? "equal" : compared }),
    );
    if (!equal) process.exitCode = 1;
  } finally {
    model.close();
    await rm(root, { recursive: true, force: true });
  }
}

/** The model request of the latest turn as `role:text` entries. */
const lastTranscript = (model) => transcript(model.requests.at(-1));

// 场景 4：Node 与 Rust 交替续写同一会话（含工具轮）。
await scenario("alternate", async ({ start, workspace, model }) => {
  const cwd = await workspace("ws");
  const node = start("node", cwd);
  const created = await node.command("createSession", null, { workspaceId: cwd });
  assert(created.result?.sessionId, JSON.stringify(created));
  const id = created.result.sessionId;
  await node.subscribe(id);
  await node.turn(id, "turn 1 from node");
  await node.turn(id, "use tool on note");
  await node.close();
  const steps = [];
  const sent = ["turn 1 from node", "use tool on note"];
  for (const [kind, text] of [
    ["rust", "turn 2 from rust"],
    ["node", "turn 3 from node"],
    ["rust", "turn 4 from rust"],
  ]) {
    const runtime = start(kind, cwd);
    await runtime.subscribe(id);
    await runtime.turn(id, text);
    sent.push(text);
    steps.push(kind);
    await runtime.close();
    // 请求里的对话是此前所有运行时写入的完整历史（含对方的工具调用与结果）。
    const seen = lastTranscript(model);
    assert.deepEqual(
      seen.filter((e) => e.startsWith("user:")),
      sent.map((t) => `user:${t}`),
      `${kind} request: ${seen.join(" | ")}`,
    );
    assert(seen.includes("call:Read") && seen.includes("tool:result"), seen.join(" | "));
  }
  return { turns: 5, runtimes: ["node", ...steps] };
});

// 场景 5：流式中与工具执行中强杀，另一个运行时恢复；排队命令按重启丢弃回答。
await scenario("crash", async ({ start, workspace }) => {
  const cwd = await workspace("ws");
  const results = {};
  for (const [killed, resumer, prompt] of [
    ["rust", "node", "slow stream"],
    ["node", "rust", "slow stream"],
    ["rust", "node", "shell please"],
    ["node", "rust", "shell please"],
  ]) {
    const victim = start(killed, cwd);
    const id = (await victim.command("createSession", null, { workspaceId: cwd })).result.sessionId;
    await victim.subscribe(id);
    await victim.turn(id, "before crash");
    const from = victim.frames.length;
    await victim.command("sendText", id, { text: prompt });
    await victim.until(
      (d) =>
        prompt.startsWith("slow")
          ? d.row?.kind === "assistantText" || d.op === "row.delta"
          : d.row?.kind === "toolCall" && d.row.status === "running",
      from,
    );
    if (!prompt.startsWith("slow")) {
      for (let i = 0; i < 200; i++) {
        if (
          await readFile(join(cwd, "shell.pid"), "utf8").then(
            (t) => t.trim(),
            () => "",
          )
        )
          break;
        await new Promise((r) => setTimeout(r, 25));
      }
    }
    const queued = randomUUID();
    const ack = await victim.command("sendText", id, { text: "queued input" }, queued);
    assert.equal(ack.result?.delivery, "queue", JSON.stringify(ack));
    await victim.kill();
    if (!prompt.startsWith("slow")) {
      // 被杀进程留下的 Shell 由测试回收。
      const pid = Number((await readFile(join(cwd, "shell.pid"), "utf8")).trim());
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // 已退出。
      }
      await rm(join(cwd, "shell.pid"), { force: true });
    }
    const resumed = start(resumer, cwd);
    await resumed.subscribe(id);
    // 被杀的轮次冷恢复为已结束：没有仍在运行或流式中的行，会话不处于运行态。
    const snapshot = await resumed.snapshot();
    const open = snapshot.rows.window.filter(
      (row) =>
        ["running", "streaming"].includes(row.state) ||
        ["running", "inputStreaming", "pendingApproval"].includes(row.status),
    );
    assert.deepEqual(open, [], `${resumer} resumed with open rows`);
    assert(!["running", "prewarming"].includes(snapshot.control.phase), snapshot.control.phase);
    const replay = await resumed.command("sendText", id, { text: "queued input" }, queued);
    assert.equal(
      replay.reasonCode,
      "fault.command.inputDiscardedOnRestart",
      JSON.stringify(replay),
    );
    await resumed.turn(id, "after crash");
    await resumed.close();
    results[`${killed}:${prompt.split(" ")[0]}`] = `${resumer} resumed`;
  }
  return results;
});

// 场景 6：Node 与 Rust 同时写各自的会话。
await scenario("concurrent", async ({ start, workspace }) => {
  const turns = 12;
  const runs = await Promise.all(
    ["node", "rust"].map(async (kind) => {
      const cwd = await workspace(`ws-${kind}`);
      const runtime = start(kind, cwd);
      const id = (await runtime.command("createSession", null, { workspaceId: cwd })).result
        .sessionId;
      await runtime.subscribe(id);
      for (let i = 0; i < turns; i++) await runtime.turn(id, `${kind} turn ${i}`);
      await runtime.close();
      assert.doesNotMatch(runtime.stderr, /database is locked|SQLITE_BUSY/i);
      return kind;
    }),
  );
  return { runtimes: runs, turnsEach: turns };
});
