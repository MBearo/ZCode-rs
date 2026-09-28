import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { Worker } from "node:worker_threads";
import { once } from "node:events";
import { prepareLaunch } from "../lib/launch.mjs";

async function setup(t, source) {
  const root = await mkdtemp(join(tmpdir(), "rust-bridge-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const binary = join(root, "native fixture");
  await writeFile(binary, `#!${process.execPath}\n${source}`, { mode: 0o755 });
  const nodeBundle = join(root, "original.cjs");
  await writeFile(
    nodeBundle,
    'process.stdout.write(JSON.stringify({method:"startup/storagePrepared"})+"\\n");',
  );
  const launch = await prepareLaunch({
    app: { version: "3.14.3", nodeBundle, executable: "/fixture/app" },
    binary,
    runtimeDir: root,
  });
  return { ...launch, root, binary };
}

test(
  "bridge transparently forwards fragmented bytes and EOF; propagates exit code",
  { skip: process.platform === "win32" },
  async (t) => {
    const f = await setup(
      t,
      'process.stdin.pipe(process.stdout); process.stdin.on("end",()=>{process.exitCode=7});',
    );
    const child = spawn(process.execPath, [f.bridge, "app-server", "--stdio"], { stdio: "pipe" });
    t.after(() => child.kill("SIGKILL"));
    const chunks = [];
    child.stdout.on("data", (v) => chunks.push(v));
    let stderr = "";
    child.stderr.on("data", (v) => (stderr += v));
    const closed = once(child, "close");
    child.stdin.write('{"id":1');
    child.stdin.end(',"text":"中文"}\n');
    assert.equal((await closed)[0], 7);
    assert.equal(Buffer.concat(chunks).toString(), '{"id":1,"text":"中文"}\n');
    assert.equal(stderr, "");
    const receipt = JSON.parse(await readFile(join(f.cwd, `bridge-${child.pid}.json`), "utf8"));
    assert.equal(receipt.phase, "agent-exited");
    assert.equal(receipt.code, 7);
  },
);
test("storage Worker loads the original bundle, never the Rust binary", async (t) => {
  const f = await setup(
    t,
    'throw Error("Must not start Rust during Worker storage initialization")',
  );
  const worker = new Worker(f.bridge, {
    argv: ["app-server", "--stdio", "--prepare-storage", "--cwd", f.root],
    stdout: true,
    stderr: true,
  });
  const chunks = [];
  worker.stdout.on("data", (v) => chunks.push(v));
  assert.equal((await once(worker, "exit"))[0], 0);
  assert.deepEqual(JSON.parse(Buffer.concat(chunks).toString()), {
    method: "startup/storagePrepared",
  });
});
test(
  "SIGTERM reaches Rust child and bridge waits for its exit",
  { skip: process.platform === "win32" },
  async (t) => {
    const f = await setup(
      t,
      'process.on("SIGTERM",()=>{process.stdout.write("stopped\\n");process.exit(0)});process.stdout.write("ready\\n");setInterval(()=>{},1000)',
    );
    const child = spawn(process.execPath, [f.bridge, "app-server", "--stdio"], { stdio: "pipe" });
    t.after(() => child.kill("SIGKILL"));
    let text = "";
    child.stdout.on("data", (v) => (text += v));
    await once(child.stdout, "data");
    const closed = once(child, "close");
    child.kill("SIGTERM");
    assert.equal((await closed)[0], 0);
    assert.equal(text, "ready\nstopped\n");
  },
);
test("missing native executable fails without corrupting protocol stdout", async (t) => {
  const f = await setup(t, "");
  await rm(f.binary);
  const child = spawn(process.execPath, [f.bridge, "app-server", "--stdio"], { stdio: "pipe" });
  let stdout = "",
    stderr = "";
  child.stdout.on("data", (v) => (stdout += v));
  child.stderr.on("data", (v) => (stderr += v));
  assert.notEqual((await once(child, "close"))[0], 0);
  assert.equal(stdout, "");
  assert.match(stderr, /ENOENT/);
});

test(
  "abrupt bridge death closes Rust stdin instead of leaving an orphan",
  { skip: process.platform === "win32" },
  async (t) => {
    const f = await setup(
      t,
      'process.stdin.resume();process.stdin.on("end",()=>{require("node:fs").writeFileSync(process.env.EOF_MARKER,"closed");process.exit(0)});process.stdout.write("ready\\n")',
    );
    const marker = join(f.root, "eof");
    const child = spawn(process.execPath, [f.bridge, "app-server", "--stdio"], {
      stdio: "pipe",
      env: { ...process.env, EOF_MARKER: marker },
    });
    await once(child.stdout, "data");
    const closed = once(child, "close");
    child.kill("SIGKILL");
    await closed;
    const deadline = Date.now() + 5000;
    let text;
    while (Date.now() < deadline) {
      try {
        text = await readFile(marker, "utf8");
        break;
      } catch {
        await new Promise((r) => setTimeout(r, 20));
      }
    }
    assert.equal(text, "closed");
  },
);
