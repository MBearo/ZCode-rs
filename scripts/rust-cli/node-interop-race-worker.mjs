// rust-cli 与 Node CLI 的双进程并发写与迁移竞争的 Node 一侧（docs/specs/rust-cli/node-store.md §6 第 12 段）。
//
// 由 node-interop-race.mjs 以 tsx 子进程启动，用 Node 的真实代码读写会话库：`SqliteSessionStore.openStartup`、
// `message-persistence.ts` 的写入函数、history hydrator。stdin 每行一个命令（`{id, cmd, …}`）或起跑信号
// （`{go: id}`），stdout 每行一个带 id 的回复；要对齐起跑的命令先回 `{ready: true}`，等到起跑信号再做。
//
// 用法（只由协调脚本调用）：node --import <tsx loader> node-interop-race-worker.mjs --node-root <检出>
import { createHash } from "node:crypto";
import { createInterface } from "node:readline";
import { DatabaseSync } from "node:sqlite";
import { importNode, nodeRootFromArgs } from "./node-root.mjs";

const WORKSPACE = "/w/race";

async function worker(nodeRoot) {
  const { SqliteSessionStore } = await importNode(
    nodeRoot,
    "adapters/src/storage/session-store/sqlite-session-store.ts",
  );
  const { SQLITE_MIGRATIONS } = await importNode(
    nodeRoot,
    "adapters/src/storage/session-store/migrations.ts",
  );
  const persistence = await importNode(nodeRoot, "core/src/runtime/methods/message-persistence.ts");
  const { hydrateMessageHistoryFromSession } = await importNode(
    nodeRoot,
    "core/src/agent/session-history-hydrator.ts",
  );
  const { createMessageHistory } = await importNode(nodeRoot, "core/src/agent/message-history.ts");
  const { toTokenUsageInfo } = await importNode(
    nodeRoot,
    "core/src/runtime/helpers/tool-result.ts",
  );
  const { projectIdFromDirectory, slugify } = await importNode(
    nodeRoot,
    "core/src/runtime/helpers/project.ts",
  );
  const { createMessageId, createPartId, createTurnId } = await importNode(
    nodeRoot,
    "contracts/src/interfaces/shared.ts",
  );

  const send = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
  const gates = new Map();
  const ready = (id, value = {}) =>
    new Promise((resolve) => {
      gates.set(id, resolve);
      send({ id, ready: true, ...value });
    });
  const describe = (error) => ({
    message: error?.message,
    code: error?.code,
    errcode: error?.errcode,
    errstr: error?.errstr,
    kind: error?.kind,
  });
  const stores = new Map();
  const storeFor = async (dbPath) => {
    if (!stores.has(dbPath)) stores.set(dbPath, await SqliteSessionStore.openStartup({ dbPath }));
    return stores.get(dbPath);
  };
  const selection = { providerId: "p", modelId: "m" };

  const commands = {
    migrations: () => ({
      migrations: SQLITE_MIGRATIONS.map((m) => ({
        id: m.id,
        checksum: createHash("sha256").update(m.sql.trim()).digest("hex"),
      })).sort((a, b) => a.id.localeCompare(b.id)),
    }),
    async open({ id, dbPath }) {
      await ready(id);
      const start = Date.now();
      let waited = false;
      // Node 的启动进度：等过写锁时报 `waiting_for_lock`（migration-runner.ts）。
      const onProgress = async ({ phase }) => {
        if (phase === "waiting_for_lock") waited = true;
      };
      try {
        const store = await SqliteSessionStore.openStartup({ dbPath }, { onProgress });
        const applied = store.migrationResult.applied.length;
        store.close();
        return { start, end: Date.now(), applied, waited };
      } catch (error) {
        return { start, end: Date.now(), error: describe(error) };
      }
    },
    // `split`：第一轮写完 user 后再报一次 ready、等第二个起跑信号，好在 Node 一轮的中间持锁。
    async write({ id, dbPath, session, n, tag, pause = 0, split = false }) {
      const store = await storeFor(dbPath);
      let sessionID = session;
      if (!sessionID) {
        sessionID = `sess_${crypto.randomUUID()}`;
        await store.createSession({
          id: sessionID,
          projectID: projectIdFromDirectory(WORKSPACE),
          slug: slugify(sessionID),
          directory: WORKSPACE,
          path: WORKSPACE,
          title: "",
          titleSource: "default",
          version: "node-race",
          traceID: sessionID,
          permission: { mode: "edit" },
        });
      }
      await ready(id, { session: sessionID });
      // Node 写入代码要的 runtime 成员（`AgentRuntimeInternal` 的子集，同 node-interop-check.mjs）。
      const runtime = {
        sessionId: sessionID,
        sessionStore: store,
        config: { mode: "edit" },
        getTools: () => [],
        getSessionModelSelection: () => selection,
        getPlanEnabled: () => false,
        workingDirectory: WORKSPACE,
        workspaceRoot: WORKSPACE,
      };
      runtime.persistMessage = persistence.persistMessage.bind(runtime);
      runtime.persistPart = persistence.persistPart.bind(runtime);
      const rounds = [];
      for (let i = 0; i < n; i++) {
        const start = Date.now();
        let resumed = start;
        try {
          const trace = { traceId: sessionID, turnId: createTurnId() };
          const userID = createMessageId();
          await persistence.persistUserPrompt.call(
            runtime,
            userID,
            `${tag} q${i}`,
            undefined,
            trace,
            {
              sourceCommandId: `${tag}_${i}`,
            },
          );
          if (split && i === 0) await ready(id, { half: true });
          resumed = Date.now();
          const assistantID = createMessageId();
          const created = Date.now();
          const tokens = toTokenUsageInfo({ inputTokens: 12, outputTokens: 7, totalTokens: 19 });
          await persistence.persistAssistantMessage.call(
            runtime,
            assistantID,
            userID,
            created,
            { completed: Date.now(), finish: "stop", tokens },
            trace,
            selection,
          );
          for (const part of [
            { type: "step-start" },
            { type: "text", text: `${tag} a${i}`, time: { start: created, end: Date.now() } },
            { type: "step-finish", reason: "stop", cost: 0, tokens },
          ]) {
            await runtime.persistPart({
              id: createPartId(),
              sessionID,
              messageID: assistantID,
              ...part,
            });
          }
          rounds.push({ start, resumed, end: Date.now() });
        } catch (error) {
          rounds.push({ start, resumed, end: Date.now(), error: describe(error) });
        }
        if (pause) await new Promise((resolve) => setTimeout(resolve, pause));
      }
      return { session: sessionID, rounds };
    },
    // `older`：模拟旧版 Node（0.16.5）的启动迁移，在持锁的事务里补齐前 22 个迁移（没有改 session 表的 0023 与 0024）再提交。
    async hold({ id, dbPath, ms, wal, older = false }) {
      const db = new DatabaseSync(dbPath);
      if (wal || older) db.exec("pragma journal_mode = wal");
      db.exec("begin immediate");
      if (older) {
        db.exec(
          "create table if not exists schema_migration (id text primary key, checksum text not null, app_version text, time_applied integer not null)",
        );
        for (const migration of SQLITE_MIGRATIONS.slice(0, 22)) {
          if (migration.apply) migration.apply(db);
          else db.exec(migration.sql);
          db.prepare("insert into schema_migration values (?, ?, ?, ?)").run(
            migration.id,
            createHash("sha256").update(migration.sql.trim()).digest("hex"),
            migration.appVersion,
            Date.now(),
          );
        }
      }
      send({ id, held: Date.now() });
      await new Promise((resolve) => setTimeout(resolve, ms));
      // 释放时间取在提交之前：等锁的一方只能在这之后拿到锁。
      const released = Date.now();
      db.exec("commit");
      db.close();
      return { released };
    },
    async read({ dbPath, session }) {
      const store = await storeFor(dbPath);
      const info = await store.getSession(session);
      const history = createMessageHistory();
      await hydrateMessageHistoryFromSession({
        history,
        messages: await store.messages({ sessionID: session }),
        branchCutAfterMessageId: info?.revert?.branchCutAfterMessageID,
        rewindCreatedMessageId: info?.revert?.createdMessageID,
        rewindKeptMessageIds: info?.revert?.keptMessageIDs,
        rewindTargetMessageId: info?.revert?.targetMessageID,
      });
      const entries = history
        .toRuntimeEntries()
        .filter((entry) => ["user", "assistant"].includes(entry.message?.role))
        .map(({ message }) => {
          const blocks =
            typeof message.content === "string"
              ? [{ type: "text", text: message.content }]
              : message.content;
          const text = blocks
            .filter((b) => b.type === "text")
            .map((b) => b.text)
            .join("\n\n");
          return { role: message.role, text };
        });
      return { history: entries };
    },
  };

  createInterface({ input: process.stdin })
    .on("line", (line) => {
      const message = JSON.parse(line);
      if (message.go !== undefined) {
        gates.get(message.go)?.();
        gates.delete(message.go);
        return;
      }
      Promise.resolve()
        .then(() => commands[message.cmd](message))
        .then(
          (value) => send({ id: message.id, ...value }),
          (error) => send({ id: message.id, error: describe(error) }),
        );
    })
    .on("close", () => {
      for (const store of stores.values()) store.close();
    });
}

await worker(nodeRootFromArgs(process.argv.slice(2)).nodeRoot);
