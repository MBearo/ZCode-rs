#!/usr/bin/env node
// rust-cli 与 Node CLI 的双进程并发写与迁移竞争（docs/specs/rust-cli/node-store.md §6 第 12 段，§3）。
//
// 进程：本脚本是协调者，只用 node:sqlite 读库核对，不写会话；Node 一侧是 tsx 子进程 node-interop-race-worker.mjs，
// 用 Node 的真实代码（`SqliteSessionStore.openStartup`、`message-persistence.ts` 的写入函数、history hydrator）；
// rust-cli 一侧是 `crates/node-store/examples/node_race.rs` 的子进程。要同时开始的两边先报 ready，协调者同时给
// 两边写一行起跑信号。Node 写者不放在协调者里：node:sqlite 是同步的，等锁时整个事件循环停住，协调者没法计时与
// 发信号；而且 SQLite 的 POSIX 文件锁按进程算，只有分开的进程才走真正的跨进程加锁。
//
// a. 不同会话：Node 写会话 A、rust-cli 写会话 B，同时各写若干轮；
// b. 同一会话：两边同时往 Node 建的会话里写；
//    a、b 都查：没丢写（每轮的文本各一条）、message / part 的 sequence 从 0 连续不重复、integrity_check、
//    rust-cli 的回执序号连续，两边冷读取的历史相同；
// c. 锁与等待：一边持写锁 1 s 时另一边提交，等到释放后成功（两个方向）；持锁 6.5 s 时两边都在约 5 s
//    （busy_timeout 5000 ms）失败，Node 抛 SQLITE_BUSY（errcode 5），rust-cli 整批回滚、回执序号不前进；
// d. 迁移竞争：全新库上 Node `openStartup` 与 rust-cli 首次打开同时进行（重复 5 次），都成功、账本 24 行与 Node
//    的校验和相同；一边持 `begin immediate`（模拟长迁移）1 s 时另一边打开全新 / 已是 WAL 但未迁移的库，等到
//    释放后成功（两个方向）。
//
// 用法：node scripts/rust-cli/node-interop-race.mjs [--node-root <装好依赖的检出>]
// 也由 node-interop-check.mjs 调用（`checkRace(ctx)`）。
import { execFile, execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual, promisify } from "node:util";
import { ensureTypeScript, nodeRootFromArgs, repoRoot as root } from "./node-root.mjs";

const run = promisify(execFile);
/** a、b 的轮数：Node 每轮 12 条自动提交的语句，rust-cli 每轮一个事务，按各自的速度让两边写的时间大致重叠。 */
const NODE_ROUNDS = 150;
const RUST_ROUNDS = 150;
/**
 * 轮间停顿（真实回合的提交之间隔着模型请求）。不停顿时一边背靠背提交，另一边的 busy handler 按 1→100 ms 递增
 * 地轮询，几乎抢不到两次提交之间的空隙，两边的写变成先后而不是交错（实测 a 只交替 1 次）。
 */
const PAUSE_MS = 2;
const SHORT_HOLD_MS = 1000;
const LONG_HOLD_MS = 6500;
const MIGRATION_HOLD_MS = 1000;
/**
 * 迁移竞争里 Node 晚起跑的毫秒数。同时起跑时 Node（约 17 ms 补齐迁移）总比 rust-cli 的 debug 构建（约 35 ms）先
 * 拿到写锁；晚几毫秒起跑让 rust-cli 先迁移、Node 等它，两种先后都走到。
 */
const MIGRATION_OFFSETS_MS = [0, 0, 6, 12, 18];
/** 两边的 busy_timeout（Node `DEFAULT_SQLITE_STARTUP_LOCK_TIMEOUT_MS`，rust-cli `BUSY_TIMEOUT`）。 */
const BUSY_TIMEOUT_MS = 5000;

const dbOf = (dir) => join(dir, "cli/db/db.sqlite");
/** node:sqlite 在用到时才导入：单独运行时外层进程（tsx 重新执行之前）不加载它，不打印实验特性警告。 */
let DatabaseSync;
const loadSqlite = async () => {
  ({ DatabaseSync } = await import("node:sqlite"));
};

// ---------------------------------------------------------------- 子进程的行协议

/** 先到的值排队、先来的取值者等待；`fail` 之后取值失败（子进程已退出）。 */
function mailbox() {
  const queue = [];
  const waiters = [];
  let failure;
  return {
    push(value) {
      const waiter = waiters.shift();
      if (waiter) waiter.resolve(value);
      else queue.push(value);
    },
    fail(error) {
      failure = error;
      for (const waiter of waiters.splice(0)) waiter.reject(error);
    },
    next() {
      if (queue.length) return Promise.resolve(queue.shift());
      if (failure) return Promise.reject(failure);
      return new Promise((resolve, reject) => waiters.push({ resolve, reject }));
    },
  };
}

/** 子进程 stdout 的每行是一个 JSON，交给 `onValue`；退出时 `onExit` 收到带 stderr 的错误。 */
function readJsonLines(child, name, onValue, onExit) {
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  createInterface({ input: child.stdout }).on("line", (line) => {
    let value;
    try {
      value = JSON.parse(line);
    } catch {
      return; // 不是协议行（例如依赖打印的提示）。
    }
    onValue(value);
  });
  child.on("close", (code) => onExit(new Error(`${name} exited with ${code}\n${stderr}`)));
}

/** rust-cli 一侧：每个子命令一个进程，`go()` 写起跑信号，`done()` 取最后一行并关掉 stdin。 */
function rustProcess(binary, ...args) {
  const child = spawn(binary, args, { stdio: ["pipe", "pipe", "pipe"] });
  const lines = mailbox();
  readJsonLines(child, `node_race ${args[0]}`, lines.push, lines.fail);
  return {
    next: lines.next,
    go: () => child.stdin.write("go\n"),
    async done() {
      const value = await lines.next();
      child.stdin.end();
      return value;
    },
  };
}

/** Node 一侧：一个常驻的 tsx 子进程，回复按命令 id 分发，`go()` 写这个命令的起跑信号。 */
function nodeWorker(nodeRoot) {
  const loader = pathToFileURL(join(nodeRoot, "node_modules/tsx/dist/loader.mjs")).href;
  const script = join(import.meta.dirname, "node-interop-race-worker.mjs");
  const args = ["--no-warnings", "--import", loader, script];
  const child = spawn(process.execPath, [...args, "--node-root", nodeRoot], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  const boxes = new Map();
  let exited;
  const box = (id) => {
    if (!boxes.has(id)) {
      const created = mailbox();
      if (exited) created.fail(exited);
      boxes.set(id, created);
    }
    return boxes.get(id);
  };
  readJsonLines(
    child,
    "Node worker",
    ({ id, ...value }) => box(id).push(value),
    (error) => {
      exited = error;
      for (const target of boxes.values()) target.fail(error);
    },
  );
  let sequence = 0;
  return {
    call(cmd, args = {}) {
      const id = ++sequence;
      child.stdin.write(`${JSON.stringify({ id, cmd, ...args })}\n`);
      const next = () => box(id).next();
      return { next, go: () => child.stdin.write(`${JSON.stringify({ go: id })}\n`), done: next };
    },
    async close() {
      child.stdin.end();
      await once(child, "close");
    },
  };
}

/** 两边都 ready 之后起跑（`leftDelayMs` 让右边先跑这么久），返回两边的结果。 */
async function race(left, right, leftDelayMs = 0) {
  const ready = await Promise.all([left.next(), right.next()]);
  for (const value of ready)
    if (!value.ready) throw new Error(`not ready: ${JSON.stringify(value)}`);
  right.go();
  if (leftDelayMs) await new Promise((resolve) => setTimeout(resolve, leftDelayMs));
  left.go();
  return { ready, results: await Promise.all([left.done(), right.done()]) };
}

// ---------------------------------------------------------------- 库的核对（协调者只读）

function withDb(dbPath, work) {
  const db = new DatabaseSync(dbPath, { timeout: BUSY_TIMEOUT_MS });
  try {
    return work(db);
  } finally {
    db.close();
  }
}

/** integrity_check、每个会话 message 的 sequence、每条消息 part 的 sequence：都必须从 0 连续不重复。 */
function storageShape(dbPath, sessions) {
  return withDb(dbPath, (db) => {
    const integrity = db
      .prepare("pragma integrity_check")
      .all()
      .map((r) => r.integrity_check);
    const contiguous = (r) => r.nulls === 0 && r.lo === 0 && r.hi === r.n - 1 && r.d === r.n;
    const messages = Object.fromEntries(
      sessions.map((session) => {
        const r = db
          .prepare(
            `select count(*) n, count(distinct sequence) d, min(sequence) lo, max(sequence) hi,
               sum(sequence is null) nulls from message where session_id = ?`,
          )
          .get(session);
        return [session, { count: r.n, contiguous: contiguous(r) }];
      }),
    );
    const brokenParts = db
      .prepare(
        `select message_id, count(*) n, count(distinct sequence) d, min(sequence) lo, max(sequence) hi,
           sum(sequence is null) nulls from part group by message_id`,
      )
      .all()
      .filter((r) => !contiguous(r)).length;
    return { integrity, messages, brokenParts };
  });
}

/** 一个会话里 text part 的文本，按消息的 sequence 排；用来查丢写与交错。 */
function texts(dbPath, session) {
  return withDb(dbPath, (db) =>
    db
      .prepare(
        `select json_extract(p.data, '$.text') text from message m join part p on p.message_id = m.id
         where m.session_id = ? and json_extract(p.data, '$.type') = 'text'
         order by m.sequence, p.sequence`,
      )
      .all(session)
      .map((r) => r.text),
  );
}

/** 两个会话的消息按 rowid（写入先后）交替了几次。 */
function switches(dbPath, a, b) {
  return withDb(dbPath, (db) => {
    const order = db
      .prepare("select session_id s from message where session_id in (?, ?) order by rowid")
      .all(a, b)
      .map((r) => r.s);
    return order.filter((s, i) => i > 0 && s !== order[i - 1]).length;
  });
}

function ledger(dbPath) {
  return withDb(dbPath, (db) => ({
    rows: db
      .prepare("select id, checksum from schema_migration order by id")
      .all()
      .map((r) => ({ ...r })),
    mode: db.prepare("pragma journal_mode").get().journal_mode,
    integrity: db.prepare("pragma integrity_check").get().integrity_check,
  }));
}

const expectedTexts = (tag, n) =>
  Array.from({ length: n }, (_, i) => [`${tag} q${i}`, `${tag} a${i}`]).flat();
const rustHistory = (messages) =>
  messages
    .filter((m) => m.type === "user" || m.type === "assistant")
    .map((m) => ({
      role: m.type,
      text: m.content
        .filter((b) => b.type === "text")
        .map((b) => b.text)
        .join("\n\n"),
    }));
const errorsOf = (rounds) => rounds.filter((r) => r.error).map((r) => r.error);
const span = (rounds) => ({ start: rounds[0].start, end: rounds.at(-1).end });
const consecutive = (values) => values.every((v, i) => i === 0 || v === values[i - 1] + 1);

// ---------------------------------------------------------------- 场景

export async function checkRace(ctx) {
  const { nodeRoot, repoRoot = root, base, expectEqual, example } = ctx;
  await loadSqlite();
  const manifest = join(repoRoot, "packages/rust-cli/Cargo.toml");
  execFileSync(
    "cargo",
    [
      "build",
      "-q",
      "--manifest-path",
      manifest,
      "-p",
      "rust-cli-node-store",
      "--example",
      "node_race",
    ],
    // 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
    { cwd: dirname(manifest), stdio: "inherit" },
  );
  const binary = example("node_race");
  const rust = (...args) => rustProcess(binary, ...args);
  const rustRead = async (dir, session) => {
    const { stdout } = await run(binary, ["read", dir, session], { encoding: "utf8" });
    return rustHistory(JSON.parse(stdout));
  };
  await mkdir(base, { recursive: true });
  const node = nodeWorker(nodeRoot);
  const summary = {};
  let mark = Date.now();
  /** 上一段结束以来的毫秒数。 */
  const lap = () => {
    const now = Date.now();
    const ms = now - mark;
    mark = now;
    return ms;
  };
  try {
    const { migrations } = await node.call("migrations").done();
    expectEqual("Node has 24 migrations", migrations.length, 24);

    // a. 不同会话同时写。
    {
      const dir = join(base, "a");
      const { ready, results } = await race(
        node.call("write", { dbPath: dbOf(dir), n: NODE_ROUNDS, tag: "node", pause: PAUSE_MS }),
        rust("write", dir, "new", String(RUST_ROUNDS), "rust", String(PAUSE_MS)),
      );
      const [nodeSession, rustSession] = ready.map((r) => r.session);
      const [nodeResult, rustResult] = results;
      expectEqual("a: Node writes without errors", errorsOf(nodeResult.rounds), []);
      expectEqual("a: rust-cli writes without errors", errorsOf(rustResult.rounds), []);
      const receipts = rustResult.rounds.map((r) => r.sequence);
      expectEqual(
        "a: rust-cli receipts are consecutive",
        [receipts[0], consecutive(receipts)],
        [2, true],
      );
      const shape = storageShape(dbOf(dir), [nodeSession, rustSession]);
      expectEqual("a: storage shape", shape, {
        integrity: ["ok"],
        messages: {
          [nodeSession]: { count: 2 * NODE_ROUNDS, contiguous: true },
          [rustSession]: { count: 2 * RUST_ROUNDS, contiguous: true },
        },
        brokenParts: 0,
      });
      expectEqual(
        "a: no Node write is lost",
        texts(dbOf(dir), nodeSession),
        expectedTexts("node", NODE_ROUNDS),
      );
      expectEqual(
        "a: no rust-cli write is lost",
        texts(dbOf(dir), rustSession),
        expectedTexts("rust", RUST_ROUNDS),
      );
      // 冷读取：每边读两个会话，与写入的文本相同。
      for (const [session, tag, n] of [
        [nodeSession, "node", NODE_ROUNDS],
        [rustSession, "rust", RUST_ROUNDS],
      ]) {
        const expected = expectedTexts(tag, n).map((text, i) => ({
          role: i % 2 ? "assistant" : "user",
          text,
        }));
        const { history } = await node.call("read", { dbPath: dbOf(dir), session }).done();
        expectEqual(`a: Node reads the ${tag} session`, history, expected);
        expectEqual(`a: rust-cli reads the ${tag} session`, await rustRead(dir, session), expected);
      }
      const n = span(nodeResult.rounds);
      const r = span(rustResult.rounds);
      summary.a = {
        ms: lap(),
        nodeMs: n.end - n.start,
        rustMs: r.end - r.start,
        overlapMs: Math.min(n.end, r.end) - Math.max(n.start, r.start),
        switches: switches(dbOf(dir), nodeSession, rustSession),
      };
      expectEqual("a: the writes interleave", summary.a.switches > 10, true);
    }

    // b. 同一会话同时写（Node 建的会话）。
    {
      const dir = join(base, "b");
      const nodeWriter = node.call("write", {
        dbPath: dbOf(dir),
        n: NODE_ROUNDS,
        tag: "node",
        pause: PAUSE_MS,
      });
      const nodeReady = await nodeWriter.next();
      const session = nodeReady.session;
      const rustWriter = rust("write", dir, session, String(RUST_ROUNDS), "rust", String(PAUSE_MS));
      const pending = {
        next: () => Promise.resolve(nodeReady),
        go: nodeWriter.go,
        done: nodeWriter.done,
      };
      const { results } = await race(pending, rustWriter);
      const [nodeResult, rustResult] = results;
      expectEqual("b: Node writes without errors", errorsOf(nodeResult.rounds), []);
      expectEqual("b: rust-cli writes without errors", errorsOf(rustResult.rounds), []);
      const receipts = rustResult.rounds.map((r) => r.sequence);
      expectEqual(
        "b: rust-cli receipts are consecutive",
        [receipts[0], consecutive(receipts)],
        [1, true],
      );
      const total = 2 * (NODE_ROUNDS + RUST_ROUNDS);
      expectEqual("b: storage shape", storageShape(dbOf(dir), [session]), {
        integrity: ["ok"],
        messages: { [session]: { count: total, contiguous: true } },
        brokenParts: 0,
      });
      const stored = texts(dbOf(dir), session);
      expectEqual(
        "b: no write is lost",
        [...stored].sort(),
        [...expectedTexts("node", NODE_ROUNDS), ...expectedTexts("rust", RUST_ROUNDS)].sort(),
      );
      // 每边自己的轮次按写入顺序排在 sequence 上。
      expectEqual(
        "b: each side keeps its own order",
        [stored.filter((t) => t.startsWith("node ")), stored.filter((t) => t.startsWith("rust "))],
        [expectedTexts("node", NODE_ROUNDS), expectedTexts("rust", RUST_ROUNDS)],
      );
      const { history } = await node.call("read", { dbPath: dbOf(dir), session }).done();
      const rustView = await rustRead(dir, session);
      expectEqual("b: both sides read the same history", history, rustView);
      const writers = stored.map((t) => t.split(" ")[0]);
      summary.b = {
        ms: lap(),
        messages: total,
        switches: writers.filter((w, i) => i > 0 && w !== writers[i - 1]).length,
        nodeHistory: history.length,
        rustHistory: rustView.length,
      };
      expectEqual("b: the writes interleave", summary.b.switches > 10, true);
    }

    // c. 锁与等待。
    {
      const dir = join(base, "c");
      const db = dbOf(dir);
      // c1：rust-cli 持锁 1 s，Node 提交等到释放后成功。Node 先打开库、建会话（打开时的迁移步骤也要写锁）。
      const nodeC1 = node.call("write", { dbPath: db, n: 1, tag: "node-c1" });
      const { session: nodeSession } = await nodeC1.next();
      const holder1 = rust("hold", db, String(SHORT_HOLD_MS));
      await holder1.next();
      nodeC1.go();
      const c1 = (await nodeC1.done()).rounds[0];
      const released1 = (await holder1.done()).released;
      expectEqual(
        "c1: Node commits after the rust-cli lock is released",
        [c1.error, c1.end >= released1],
        [undefined, true],
      );

      // c2：Node 持锁 1 s，rust-cli 提交等到释放后成功。
      const rustC2 = rust("write", dir, "new", "1", "rust-c2");
      const { session: rustSession } = await rustC2.next();
      const holder2 = node.call("hold", { dbPath: db, ms: SHORT_HOLD_MS });
      await holder2.next();
      rustC2.go();
      const c2 = (await rustC2.done()).rounds[0];
      const released2 = (await holder2.done()).released;
      expectEqual(
        "c2: rust-cli commits after the Node lock is released",
        [c2.error, c2.sequence, c2.end >= released2],
        [null, 2, true],
      );

      // c3：持锁 6.5 s，两边都在 busy_timeout（5000 ms）后失败。Node 一轮不是一个事务：先写完 user，在 assistant 之前
      // 持锁，Node 留下没有回答的 user（Node 自己的失败形态）；rust-cli 一轮一个事务，整批回滚。
      const nodeC3 = node.call("write", {
        dbPath: db,
        session: nodeSession,
        n: 1,
        tag: "node-c3",
        split: true,
      });
      const rustC3 = rust("write", dir, rustSession, "1", "rust-c3");
      await Promise.all([nodeC3.next(), rustC3.next()]);
      nodeC3.go();
      await nodeC3.next(); // user 已写入
      const holder3 = rust("hold", db, String(LONG_HOLD_MS));
      await holder3.next();
      nodeC3.go();
      rustC3.go();
      const [c3Node, c3Rust] = await Promise.all([
        nodeC3.done().then((r) => r.rounds[0]),
        rustC3.done().then((r) => r.rounds[0]),
      ]);
      await holder3.done();
      const nodeWait = c3Node.end - c3Node.resumed;
      const rustWait = c3Rust.end - c3Rust.start;
      const nearTimeout = (ms) => ms >= BUSY_TIMEOUT_MS - 100 && ms < LONG_HOLD_MS;
      expectEqual(
        "c3: Node fails with SQLITE_BUSY after the busy timeout",
        [c3Node.error?.errcode & 0xff, c3Node.error?.errstr, nearTimeout(nodeWait)],
        [5, "database is locked", true],
      );
      expectEqual(
        "c3: rust-cli fails after the busy timeout",
        [c3Rust.sequence, c3Rust.error, nearTimeout(rustWait)],
        [null, "storage error: database is locked", true],
      );

      // c4：释放后两边再提交：rust-cli 的回执序号接着 c2（c3 整批回滚、什么也没留下）；Node 的会话里留着 c3 的 user，
      // 两边冷读取的历史相同。
      const { results } = await race(
        node.call("write", { dbPath: db, session: nodeSession, n: 1, tag: "node-c4" }),
        rust("write", dir, rustSession, "1", "rust-c4"),
      );
      expectEqual("c4: Node commits again", errorsOf(results[0].rounds), []);
      expectEqual("c4: the rust-cli receipt does not skip", results[1].rounds[0].sequence, 3);
      expectEqual("c: storage shape", storageShape(db, [nodeSession, rustSession]), {
        integrity: ["ok"],
        messages: {
          [nodeSession]: { count: 5, contiguous: true },
          [rustSession]: { count: 4, contiguous: true },
        },
        brokenParts: 0,
      });
      expectEqual(
        "c: Node keeps the c3 prompt, rust-cli rolls the whole c3 batch back",
        [texts(db, nodeSession), texts(db, rustSession)],
        [
          [...expectedTexts("node-c1", 1), "node-c3 q0", ...expectedTexts("node-c4", 1)],
          [...expectedTexts("rust-c2", 1), ...expectedTexts("rust-c4", 1)],
        ],
      );
      const { history: nodeView } = await node
        .call("read", { dbPath: db, session: nodeSession })
        .done();
      expectEqual(
        "c: both sides read the Node session with the unanswered prompt",
        nodeView,
        await rustRead(dir, nodeSession),
      );
      summary.c = {
        ms: lap(),
        nodeWaitedMs: c1.end - c1.start,
        rustWaitedMs: c2.end - c2.start,
        nodeTimeoutMs: nodeWait,
        rustTimeoutMs: rustWait,
        nodeError: c3Node.error?.message,
        rustError: c3Rust.error,
      };
    }

    // d. 迁移竞争。
    {
      const winners = [];
      for (const [i, offset] of MIGRATION_OFFSETS_MS.entries()) {
        const dir = join(base, `d-race-${i}`);
        const { results } = await race(
          node.call("open", { dbPath: dbOf(dir) }),
          rust("open", dir),
          offset,
        );
        const [nodeOpen, rustOpen] = results;
        expectEqual(
          `d${i}: both sides open the new database`,
          [nodeOpen.error, rustOpen.error],
          [undefined, null],
        );
        expectEqual(
          `d${i}: one side applies all migrations`,
          [0, 24].includes(nodeOpen.applied),
          true,
        );
        expectEqual(`d${i}: the ledger`, ledger(dbOf(dir)), {
          rows: migrations,
          mode: "wal",
          integrity: "ok",
        });
        // 先补齐迁移的一边；Node 等过锁时标 `node waited`。各自从起跑到打开完成的毫秒数。
        winners.push(
          `+${offset}ms ${nodeOpen.applied === 24 ? "node" : "rust"} migrated${nodeOpen.waited ? ", node waited" : ""} ` +
            `(node ${nodeOpen.end - nodeOpen.start} ms, rust ${rustOpen.elapsed} ms)`,
        );
      }
      summary.d = { winners };

      // 一边持 `begin immediate` 时另一边打开：`fresh` 是刚建的空文件（rollback 模式，卡在切 WAL），`wal` 是已切 WAL 的
      // 空库（卡在迁移的 begin immediate）。
      const waits = {};
      for (const holderSide of ["rust", "node"]) {
        await Promise.all(
          ["fresh", "wal"].map(async (kind) => {
            const dir = join(base, `d-hold-${holderSide}-${kind}`);
            await mkdir(join(dir, "cli/db"), { recursive: true });
            const holder =
              holderSide === "rust"
                ? rust(
                    "hold",
                    dbOf(dir),
                    String(MIGRATION_HOLD_MS),
                    ...(kind === "wal" ? ["wal"] : []),
                  )
                : node.call("hold", {
                    dbPath: dbOf(dir),
                    ms: MIGRATION_HOLD_MS,
                    wal: kind === "wal",
                  });
            await holder.next();
            const opener =
              holderSide === "rust" ? node.call("open", { dbPath: dbOf(dir) }) : rust("open", dir);
            await opener.next();
            opener.go();
            const opened = await opener.done();
            const { released } = await holder.done();
            const name = `d: ${holderSide === "rust" ? "Node" : "rust-cli"} opens a ${kind} database held by ${holderSide}`;
            expectEqual(
              name,
              [opened.error ?? null, opened.end >= released, ledger(dbOf(dir))],
              [null, true, { rows: migrations, mode: "wal", integrity: "ok" }],
            );
            waits[`${holderSide}-${kind}`] = opened.end - opened.start;
          }),
        );
      }
      summary.d.waitedMs = waits;

      // 等锁期间旧版 Node 补齐了它知道的 22 个迁移（缺改 session 表的 0023 与 0024）：rust-cli 拿到锁时库里已有业务表，
      // 结果必须与之后再打开同一个库相同——拒绝（backup_required，§2），账本仍是 22 行。Node 在同样的竞争里于事务内看到
      // 0023 要改的 session 表，先备份再补。修复线 F12 之前 rust-cli 按等锁前的预检（空库）不备份直接补。
      const older = async (name, ms) => {
        const dir = join(base, `d-older-${name}`);
        await mkdir(join(dir, "cli/db"), { recursive: true });
        const holder = node.call("hold", { dbPath: dbOf(dir), ms, older: true });
        await holder.next();
        const opener = rust("open", dir);
        await opener.next();
        if (!ms) await holder.done();
        opener.go();
        const { error } = await opener.done();
        if (ms) await holder.done();
        const rows = withDb(
          dbOf(dir),
          (db) => db.prepare("select count(*) n from schema_migration").get().n,
        );
        return { error: error?.replaceAll(dir, "<dir>") ?? null, rows };
      };
      const [raced, later] = await Promise.all([
        older("raced", MIGRATION_HOLD_MS),
        older("later", 0),
      ]);
      expectEqual(
        "d: rust-cli refuses an older database it waited for, as when it opens it later",
        raced,
        later,
      );
      expectEqual(
        "d: rust-cli refuses the older database",
        [later.rows, later.error?.endsWith("(backup_required)")],
        [22, true],
      );
      summary.d.ms = lap();
    }
  } finally {
    await node.close();
  }
  return summary;
}

// ---------------------------------------------------------------- 单独运行

const isMain = process.argv[1] && resolve(process.argv[1]) === import.meta.filename;
if (isMain) {
  const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
  // 协调者本身不导入 TypeScript；这一步检查依赖（没有时提示并以 2 退出），与其他互操作脚本一致。
  ensureTypeScript(import.meta.filename, nodeRoot, args);
  const started = Date.now();
  const failures = [];
  const expectEqual = (name, node, rust) => {
    if (isDeepStrictEqual(node, rust)) return;
    failures.push(`${name}\n  node: ${JSON.stringify(node)}\n  rust: ${JSON.stringify(rust)}`);
  };
  const example = (name) =>
    join(
      root,
      "packages/rust-cli/target/debug/examples",
      process.platform === "win32" ? `${name}.exe` : name,
    );
  const base = await mkdtemp(join(tmpdir(), "rust-cli-node-race-"));
  try {
    const summary = await checkRace({ nodeRoot, repoRoot: root, base, expectEqual, example });
    console.log(JSON.stringify(summary, null, 2));
  } finally {
    await rm(base, { recursive: true, force: true });
  }
  const seconds = ((Date.now() - started) / 1000).toFixed(1);
  if (failures.length) {
    console.error(
      `node-interop-race: ${failures.length} mismatch(es) in ${seconds}s\n${failures.join("\n")}`,
    );
    process.exitCode = 1;
  } else {
    console.log(`node-interop-race: OK (${seconds}s)`);
  }
}
