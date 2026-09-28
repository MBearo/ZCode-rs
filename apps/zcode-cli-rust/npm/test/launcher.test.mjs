import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  rm,
  copyFile,
  chmod,
  realpath,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { prepareLaunch, cleanLaunchEnv } from "../lib/launch.mjs";
import { inspectApp, runningAppPids } from "../lib/app.mjs";
import { installBinary } from "../lib/install.mjs";
import { spawn } from "node:child_process";
import { once } from "node:events";

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "rust-launch-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const resources = join(root, "ZCode App", "resources");
  await mkdir(join(resources, "app"), { recursive: true });
  await mkdir(join(resources, "glm"), { recursive: true });
  await writeFile(
    join(resources, "app", "package.json"),
    JSON.stringify({ name: "@zcode/desktop", version: "3.14.3" }),
  );
  await writeFile(join(resources, "glm", "zcode.cjs"), "// original");
  const executable = join(root, "ZCode App", "zcode");
  await writeFile(executable, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  const binary = join(root, "rust binary");
  await writeFile(binary, "native-fixture", { mode: 0o755 });
  return { root, executable, binary, resources };
}

test("reject unsupported App before creating launcher state", async (t) => {
  const f = await fixture(t);
  await writeFile(join(f.resources, "app/package.json"), JSON.stringify({ version: "3.14.4" }));
  await assert.rejects(inspectApp(f.executable), /3.14.3/);
});
test("install is immutable, verifies reused contents and rejects checksum mismatch", async (t) => {
  const f = await fixture(t),
    runtimeDir = join(f.root, "runtime");
  const sha256 = createHash("sha256").update("native-fixture").digest("hex");
  const source = { binary: f.binary, sha256, version: "0.1.0", platform: "test" };
  const a = await installBinary(source, runtimeDir);
  const b = await installBinary(source, runtimeDir);
  assert.equal(a, b);
  assert.equal(await readFile(a, "utf8"), "native-fixture");
  await writeFile(a, "corrupted");
  await assert.rejects(installBinary(source, runtimeDir), /checksum/i);
  await assert.rejects(
    installBinary({ ...source, sha256: "0".repeat(64) }, runtimeDir),
    /checksum/i,
  );
});
test("simultaneous installs converge on one verified executable", async (t) => {
  const f = await fixture(t);
  const source = {
    binary: f.binary,
    sha256: createHash("sha256").update("native-fixture").digest("hex"),
    version: "0.1.0",
    platform: "test",
  };
  const paths = await Promise.all(
    Array.from({ length: 5 }, () => installBinary(source, join(f.root, "runtime"))),
  );
  assert.equal(new Set(paths).size, 1);
  assert.equal(await readFile(paths[0], "utf8"), "native-fixture");
});
test(
  "running App detection survives process.title changes",
  { skip: process.platform !== "darwin", timeout: 15000 },
  async (t) => {
    const f = await fixture(t),
      executable = join(f.root, "renamed executable");
    await copyFile(process.execPath, executable);
    await chmod(executable, 0o755);
    const child = spawn(
      executable,
      [
        "-e",
        'process.title="ZCode fixture";process.stdout.write("ready");setInterval(()=>{},1000)',
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    t.after(async () => {
      if (child.exitCode === null && child.signalCode === null) {
        const closed = once(child, "close");
        child.kill("SIGKILL");
        await closed;
      }
    });
    await once(child.stdout, "data");
    assert.ok((await runningAppPids(await realpath(executable))).includes(child.pid));
  },
);
test("parallel launches have separate manifests; paths with spaces survive", async (t) => {
  const f = await fixture(t),
    app = await inspectApp(f.executable);
  const options = { app, binary: f.binary, runtimeDir: join(f.root, "runtime") };
  const [a, b] = await Promise.all([prepareLaunch(options), prepareLaunch(options)]);
  assert.notEqual(a.cwd, b.cwd);
  const manifest = JSON.parse(await readFile(a.manifest, "utf8"));
  assert.equal(manifest.binary, f.binary);
  assert.equal(manifest.nodeBundle, app.nodeBundle);
  assert.equal(
    await readFile(a.bridge, "utf8"),
    await readFile(new URL("../bridge.cjs", import.meta.url), "utf8"),
  );
});
test("override env cannot bypass the compatible entry and stdio tap cannot intercept it", () => {
  const env = cleanLaunchEnv({
    PATH: "/bin",
    ZCODE_AGENT_SERVER_COMMAND: "/old",
    ZCODE_AGENT_SERVER_RUNTIME: "rust",
    ZCODE_AGENT_SERVER_ARGS_JSON: "[]",
    ZCODE_AGENT_SERVER_CWD: "/old",
    ZCODE_DESKTOP_AGENT_BYTECODE: "1",
    ELECTRON_RUN_AS_NODE: "1",
    NODE_OPTIONS: "--require bad",
  });
  assert.deepEqual(env, { PATH: "/bin" });
});

test("ASAR metadata reading is bounded and supports the official archive layout", async (t) => {
  const { createPackage } = await import("@electron/asar");
  const { readAppPackage } = await import("../lib/app.mjs");
  const f = await fixture(t);
  const input = join(f.root, "asar-input");
  await mkdir(input);
  await writeFile(
    join(input, "package.json"),
    JSON.stringify({ name: "@zcode/desktop", version: "3.14.3" }),
  );
  await createPackage(input, join(f.resources, "app.asar"));
  await rm(join(f.resources, "app"), { recursive: true });
  assert.equal((await readAppPackage(f.resources)).version, "3.14.3");
  await writeFile(join(f.resources, "app.asar"), Buffer.alloc(16, 255));
  await assert.rejects(readAppPackage(f.resources), /Invalid app.asar/);
});
