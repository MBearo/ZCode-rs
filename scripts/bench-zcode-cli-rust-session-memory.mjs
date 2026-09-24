// Run with TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json node --import tsx.
// Uses the actual App client/schema, the same seeded canonical history, and interleaved releases.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { cpus } from "node:os";
import { fixture } from "../packages/services/tests/zcode-cli-rust-fixture.ts";
import { createSqliteSessionStore } from "../apps/zcode-cli/packages/adapters/src/storage/session-store.ts";

const [baseline, candidate, output = ".zcode-runtime/rust-perf-20260922/memory"] =
  process.argv.slice(2);
if (!baseline || !candidate) throw new Error("Expected baseline and candidate binaries");
const run = promisify(execFile);
const options = { binary: resolve(baseline) };
const f = await fixture(options);
const results = [];
const pageId = "page-benchmark";
const directory = resolve(output);
await mkdir(directory, { recursive: true });
async function rss(h) {
  const { stdout } =
    process.platform === "win32"
      ? await run("powershell.exe", [
          "-NoProfile",
          "-Command",
          `(Get-Process -Id ${h.child.pid}).WorkingSet64 / 1024`,
        ])
      : await run("ps", ["-o", "rss=", "-p", String(h.child.pid)]);
  return Number(stdout.trim());
}
/** macOS phys_footprint (Activity Monitor memory): unlike `ps` RSS it leaves out freed
 * pages the allocator keeps as reusable. `null` on other platforms. */
async function footprint(h) {
  if (process.platform !== "darwin") return null;
  const { stdout } = await run("footprint", ["-p", String(h.child.pid)]);
  const match = /Footprint:\s+([\d.]+)\s+([KMG])B/.exec(stdout);
  if (!match) return null;
  return Number(match[1]) * { K: 1, M: 1024, G: 1024 * 1024 }[match[2]];
}
async function idle(h) {
  // A subsequent owner RPC observes completion of the preceding request's eviction, without sleeps.
  await h.client.request("runtime/capabilities", {});
}
try {
  const seed = f.start(),
    ids = [];
  for (let i = 0; i < 12; i++) {
    const id = await seed.create();
    ids.push(id);
    await seed.subscribe(`conversation/${id}`);
    await seed.command(seed.envelope("sendText", id, { text: "seed" }));
    await seed.completed(id);
  }
  await seed.close();
  // 会话库与 Node 共用（spec rust-m11-node-storage §2.1）：用 Node 仓储写入大段历史。
  const store = createSqliteSessionStore({ dbPath: f.db });
  try {
    const add = async (sessionID, role, i, text) => {
      const id = `msg_bench_${sessionID}_${i}`;
      await store.saveMessage({
        id,
        sessionID,
        role,
        time: { created: 1000 + i, ...(role === "assistant" ? { completed: 1000 + i } : {}) },
        agent: "main",
        ...(role === "assistant"
          ? {
              parentID: `msg_bench_${sessionID}_${sessionID === pageId ? 0 : 100}`,
              mode: "yolo",
              path: { cwd: f.cwd, root: f.cwd },
              cost: 0,
              tokens: { input: 1, output: 1, reasoning: 0, cache: { read: 0, write: 0 } },
            }
          : {}),
      });
      await store.savePart({
        id: `prt_bench_${sessionID}_${i}`,
        sessionID,
        messageID: id,
        type: "text",
        text,
      });
    };
    await store.createSession({
      id: pageId,
      projectID: "bench",
      directory: f.cwd,
      slug: pageId,
      title: pageId,
      version: "bench",
    });
    await add(pageId, "user", 0, "page");
    for (let i = 1; i <= 200; i++) await add(pageId, "assistant", i, "x".repeat(32 * 1024));
    // Canonical-only fixtures isolate session retention from App output size and model context limits.
    // 大段正文放在 assistant 消息：冷加载会为每条 Node user 消息保留一份输入边界正文（编辑/重试用），
    // 64 条 64 KiB 的 user 消息单个会话就超过 16 MiB 的空闲驻留预算。
    const body = "x".repeat(64 * 1024);
    for (const id of ids) {
      await add(id, "user", 100, "history");
      for (let i = 1; i <= 64; i++) await add(id, "assistant", 100 + i, body);
    }
  } finally {
    store.close();
  }
  for (let repetition = 1; repetition <= 5; repetition++) {
    const versions = [
      ["baseline", baseline],
      ["candidate", candidate],
    ];
    if (!(repetition % 2)) versions.reverse();
    for (const [version, binary] of versions) {
      options.binary = resolve(binary);
      const start = performance.now(),
        h = f.start();
      try {
        await idle(h);
        const startupMs = performance.now() - start;
        const startupRssKiB = await rss(h),
          coldReadMs = [],
          rssSamples = [];
        for (const id of ids) {
          const at = performance.now();
          await h.rows(id);
          await idle(h);
          coldReadMs.push(performance.now() - at);
          rssSamples.push(await rss(h));
        }
        const idleRssKiB = await rss(h);
        const idleFootprintKiB = await footprint(h);
        const cached = await h.rows(ids.at(-1));
        assert.equal((await h.rows(ids.at(-1))).atLogEpoch, cached.atLogEpoch);
        await h.subscribe(`conversation/${ids[0]}`);
        const pinnedEpoch = (await h.rows(ids[0])).atLogEpoch;
        for (const id of ids.slice(1)) await h.rows(id);
        assert.equal((await h.rows(ids[0])).atLogEpoch, pinnedEpoch);
        await idle(h);
        const pinnedRssKiB = await rss(h);
        const pinnedFootprintKiB = await footprint(h);
        await h.subscribe(`conversation/${pageId}`);
        const pageMs = [];
        for (let i = 0; i < 3; i++) {
          const at = performance.now(),
            page = await h.rows(pageId);
          pageMs.push(performance.now() - at);
          assert.equal(page.hasMore, true);
        }
        pageMs.sort((a, b) => a - b);
        assert.deepEqual(h.schemaErrors, []);
        coldReadMs.sort((a, b) => a - b);
        const result = {
          version,
          repetition,
          binary: resolve(binary),
          platform: process.platform,
          arch: process.arch,
          cpu: cpus()[0]?.model,
          sessions: ids.length,
          canonicalBytesPerSession: 4 * 1024 * 1024,
          startupMs,
          startupRssKiB,
          idleRssKiB,
          idleFootprintKiB,
          pinnedRssKiB,
          pinnedFootprintKiB,
          sampledPeakRssKiB: Math.max(...rssSamples, pinnedRssKiB),
          coldReadP95Ms: coldReadMs[Math.floor(coldReadMs.length * 0.95)],
          largePageMs: pageMs[1],
        };
        results.push(result);
        await writeFile(
          join(directory, `${version}-${repetition}.json`),
          JSON.stringify(result, null, 2) + "\n",
        );
        console.log(
          `${version} ${repetition}/5: idle RSS ${(idleRssKiB / 1024).toFixed(2)} MiB, footprint ${((idleFootprintKiB ?? 0) / 1024).toFixed(2)} MiB`,
        );
      } finally {
        await h.close();
      }
    }
  }
  assert.equal(f.requests.length, ids.length, "Cold reads must not call the model");
  const median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
  const summary = Object.fromEntries(
    ["baseline", "candidate"].map((version) => {
      const samples = results.filter((r) => r.version === version);
      return [
        version,
        Object.fromEntries(
          [
            "startupMs",
            "startupRssKiB",
            "idleRssKiB",
            "idleFootprintKiB",
            "pinnedRssKiB",
            "pinnedFootprintKiB",
            "sampledPeakRssKiB",
            "coldReadP95Ms",
            "largePageMs",
          ].map((key) => [key, median(samples.map((s) => s[key]))]),
        ),
      ];
    }),
  );
  await writeFile(join(directory, "summary.json"), JSON.stringify(summary, null, 2) + "\n");
} finally {
  await f.close();
}
