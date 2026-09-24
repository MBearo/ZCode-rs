// Run with TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json node --import tsx.
// Spec rust-m11-node-storage §11 scenario 1: an existing Node session database (by default
// the user's `~/.zcode/cli/db/db.sqlite`) is copied with SQLite's online backup; the Rust
// runtime opens the copy without touching its schema or unknown migrations, reads every
// session like Node's readers do, and continues one session that Node then reads back.
// Only counts and JSON paths are printed, never session content; the copy is deleted.
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { once } from "node:events";
import { mkdtemp, mkdir, rm, writeFile, access } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { backup, DatabaseSync } from "node:sqlite";
import { isDeepStrictEqual } from "node:util";
import { SqliteSessionStore } from "../apps/zcode-cli/packages/adapters/src/storage/session-store/sqlite-session-store.ts";
import { createNodeToolArtifactStore } from "../apps/zcode-cli/packages/adapters/src/storage/index.ts";
import { hydrateMessageHistoryFromSession } from "../apps/zcode-cli/packages/core/src/agent/session-history-hydrator.ts";
import { createMessageHistory } from "../apps/zcode-cli/packages/core/src/agent/message-history.ts";
import { compareDatabase } from "./zcode-cli-rust-node-compare.mjs";

const source = resolve(process.argv[2] ?? join(homedir(), ".zcode/cli/db/db.sqlite"));
const artifacts = resolve(process.argv[3] ?? join(homedir(), ".zcode/cli/artifacts"));
const binary = resolve(process.argv[4] ?? "apps/zcode-cli-rust/target/release/zcode-cli-rust");
const reader = resolve(process.argv[5] ?? "apps/zcode-cli-rust/target/release/examples/node_read");
function schemaFacts(path) {
  const db = new DatabaseSync(path, { readOnly: true });
  try {
    return {
      migrations: db.prepare("select * from schema_migration order by rowid").all(),
      schema: db.prepare("select type, name, sql from sqlite_master order by type, name").all(),
    };
  } finally {
    db.close();
  }
}

/** One Rust App Server over the copy; `drive` gets an RPC helper. */
async function withRust(root, cwd, config, drive) {
  const child = spawn(
    binary,
    ["app-server", "--stdio", "--cwd", cwd, "--data-dir", join(root, "data"), "--config", config],
    {
      env: {
        ...process.env,
        HOME: join(root, "home"),
        USERPROFILE: join(root, "home"),
        ZCODE_SESSION_DB_PATH: join(root, "db.sqlite"),
      },
    },
  );
  let buffer = "";
  let serial = 0;
  let stderr = "";
  const pending = new Map();
  const frames = [];
  child.stderr.on("data", (d) => (stderr += d));
  child.stdout.setEncoding("utf8").on("data", (chunk) => {
    buffer += chunk;
    let end;
    while ((end = buffer.indexOf("\n")) >= 0) {
      const frame = JSON.parse(buffer.slice(0, end));
      buffer = buffer.slice(end + 1);
      if (frame.id !== undefined && pending.has(frame.id)) {
        const waiter = pending.get(frame.id);
        pending.delete(frame.id);
        if (frame.error) waiter.reject(new Error(frame.error.message));
        else waiter.resolve(frame.result);
      } else frames.push(frame);
    }
  });
  const rpc = (method, params = {}) =>
    new Promise((resolveRpc, reject) => {
      const id = ++serial;
      pending.set(id, { resolve: resolveRpc, reject });
      child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
    });
  const exit = once(child, "close");
  let result;
  try {
    result = await drive(rpc, frames);
  } finally {
    child.stdin.end();
  }
  const [code] = await exit;
  if (code !== 0) throw new Error(`Rust exited ${code}: ${stderr.slice(-500)}`);
  return result;
}

const root = await mkdtemp(join(tmpdir(), "zcode-real-db-"));
const summary = {};
try {
  await Promise.all([mkdir(join(root, "home")), mkdir(join(root, "data"))]);
  const copy = join(root, "db.sqlite");
  const live = new DatabaseSync(source, { readOnly: true });
  await backup(live, copy);
  live.close();
  const counts = new DatabaseSync(copy, { readOnly: true });
  summary.sessions = counts.prepare("select count(*) n from session").get().n;
  summary.messages = counts.prepare("select count(*) n from message").get().n;
  const workspace = counts
    .prepare(
      "select directory, count(*) n from session where workspace_id is null and task_type = 'interactive' group by directory order by n desc limit 20",
    )
    .all();
  counts.close();
  let cwd;
  for (const row of workspace) {
    if (
      await access(row.directory).then(
        () => true,
        () => false,
      )
    ) {
      cwd = row.directory;
      break;
    }
  }
  if (!cwd) throw new Error("No local workspace of the database exists on disk");

  // 1. Rust 打开已有库：不重跑迁移、不改未知迁移与表结构。
  const before = schemaFacts(copy);
  const config = join(root, "model.json");
  const server = createServer(async (request, response) => {
    for await (const _ of request) {
      // Consume the request before answering.
    }
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.write(`data: ${JSON.stringify({ choices: [{ delta: { content: "ok" } }] })}\n\n`);
    response.end('data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  await writeFile(
    config,
    JSON.stringify({
      providerId: "check",
      modelId: "check",
      reasoningLevel: "none",
      baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    }),
  );
  const listed = await withRust(root, cwd, config, async (rpc) => {
    await rpc("runtime/capabilities");
    const workspace = { workspacePath: cwd, workspaceKey: cwd };
    return (await rpc("session/list", { workspace, limit: 1000 })).sessions;
  });
  summary.listedInWorkspace = listed.length;
  summary.schemaUnchanged = isDeepStrictEqual(before, schemaFacts(copy));

  // 2. 逐会话比对 Rust 与 Node 的冷读取（模型上下文、界面行、快照状态）。
  console.log(JSON.stringify({ step: "startup", ...summary }));
  summary.compared = await compareDatabase(copy, artifacts, reader);
  console.log(JSON.stringify({ step: "compared", compared: summary.compared }));
  // 3. Rust 续写一个本工作区的会话，Node 读回。
  const store = await SqliteSessionStore.openStartup({ dbPath: copy });
  const artifactStore = createNodeToolArtifactStore({
    imageCacheRootDir: join(root, "cache/image"),
    pdfCacheRootDir: join(root, "cache/pdf"),
    rootDir: artifacts,
    videoCacheRootDir: join(root, "cache/video"),
  });
  const target = listed.find((s) => s.sessionKind === "interactive");
  if (target) {
    const before = (await store.messages({ sessionID: target.sessionId })).length;
    await withRust(root, cwd, config, async (rpc, frames) => {
      const topic = `conversation/${target.sessionId}`;
      await rpc("v4/conversation/subscribe", {
        topic,
        connectionId: "check",
        clientMode: "desktop-continuous",
      });
      await rpc("v4/command", {
        commandId: randomUUID(),
        clientId: "check",
        sessionId: target.sessionId,
        type: "sendText",
        // 会话存的模型不在检查用的静态配置里：本次输入显式选择本地模型。
        payload: {
          text: "continue",
          modelSelection: {
            providerId: "check",
            modelId: "check",
            options: { reasoningLevel: "none" },
          },
        },
        issuedAt: Date.now(),
      });
      for (let i = 0; i < 400; i++) {
        const done = frames.some((f) =>
          f.params?.frame?.payload?.deltas?.some((d) =>
            ["completedSuccess", "error"].includes(d.patch?.control?.phase),
          ),
        );
        if (done) break;
        await new Promise((r) => setTimeout(r, 25));
      }
      const failed = frames.some((f) =>
        f.params?.frame?.payload?.deltas?.some((d) => d.patch?.control?.phase === "error"),
      );
      if (failed) throw new Error("Rust could not continue the session");
    });
    const after = await store.messages({ sessionID: target.sessionId });
    const history = createMessageHistory();
    await hydrateMessageHistoryFromSession({ artifactStore, history, messages: after });
    summary.continued = {
      addedMessages: after.length - before,
      nodeHydrated: history.toRuntimeEntries().length > 0,
    };
  }
  store.close();
  server.closeAllConnections();
  server.close();
} finally {
  await rm(root, { recursive: true, force: true });
}
console.log(JSON.stringify(summary, null, 2));
