// Run with TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json node --import tsx.
// Uses the actual App client/schema, the same seeded canonical history, and interleaved releases.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { DatabaseSync } from "node:sqlite";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { cpus } from "node:os";
import { fixture } from "../packages/services/tests/zcode-cli-rust-fixture.ts";

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
  const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
  try {
    const insert = db.prepare(
      "INSERT INTO rust_message SELECT workspace,session,?,? FROM rust_message WHERE session=? LIMIT 1",
    );
    db.exec("BEGIN");
    db.prepare(
      "INSERT INTO rust_session SELECT workspace,?,json_set(body,'$.id',?) FROM rust_session WHERE id=?",
    ).run(pageId, pageId, ids[0]);
    for (const table of ["rust_message", "rust_row"]) {
      db.prepare(
        `INSERT INTO ${table} SELECT workspace,?,ordinal,body FROM ${table} WHERE session=?`,
      ).run(pageId, ids[0]);
    }
    db.prepare(
      "INSERT INTO rust_history SELECT workspace,?,kind,ordinal,body FROM rust_history WHERE session=?",
    ).run(pageId, ids[0]);
    const row = JSON.parse(
      db
        .prepare(
          "SELECT body FROM rust_row WHERE session=? AND json_extract(body,'$.kind')='assistantText' LIMIT 1",
        )
        .get(ids[0]).body,
    );
    const insertRow = db.prepare("INSERT INTO rust_row VALUES(?,?,?,?)");
    for (let i = 0; i < 200; i++)
      insertRow.run(
        f.cwd,
        pageId,
        1000 + i,
        JSON.stringify({
          ...row,
          rowId: 1000 + i,
          entityId: `page-${i}`,
          text: "x".repeat(32 * 1024),
        }),
      );
    const body = JSON.stringify({ role: "user", content: "x".repeat(64 * 1024) });
    // Canonical-only fixtures isolate session retention from App output size and model context limits.
    for (const id of ids) for (let i = 0; i < 64; i++) insert.run(1000 + i, body, id);
    db.exec("COMMIT");
  } finally {
    db.close();
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
        const cached = await h.rows(ids.at(-1));
        assert.equal((await h.rows(ids.at(-1))).atLogEpoch, cached.atLogEpoch);
        await h.subscribe(`conversation/${ids[0]}`);
        const pinnedEpoch = (await h.rows(ids[0])).atLogEpoch;
        for (const id of ids.slice(1)) await h.rows(id);
        assert.equal((await h.rows(ids[0])).atLogEpoch, pinnedEpoch);
        await idle(h);
        const pinnedRssKiB = await rss(h);
        await h.subscribe(`conversation/${pageId}`);
        const pageMs = [];
        for (let i = 0; i < 3; i++) {
          const at = performance.now(),
            page = await h.rows(pageId);
          pageMs.push(performance.now() - at);
          assert.equal(page.hasMore, true);
          assert.equal(page.rows.at(-1).rowId, 1199);
          assert.equal(page.rows[0].rowId, 1173);
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
          pinnedRssKiB,
          sampledPeakRssKiB: Math.max(...rssSamples, pinnedRssKiB),
          coldReadP95Ms: coldReadMs[Math.floor(coldReadMs.length * 0.95)],
          largePageMs: pageMs[1],
        };
        results.push(result);
        await writeFile(
          join(directory, `${version}-${repetition}.json`),
          JSON.stringify(result, null, 2) + "\n",
        );
        console.log(`${version} ${repetition}/5: idle ${(idleRssKiB / 1024).toFixed(2)} MiB`);
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
            "pinnedRssKiB",
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
