// Compatibility entry for the unmodified v3.14.3 Host. No session state or frame parsing.
const { readFile, writeFile, rename } = require("node:fs/promises");
const { resolve, join } = require("node:path");
const { spawn } = require("node:child_process");

async function main() {
  const launchDir = resolve(__dirname, "../../../../..");
  const manifest = JSON.parse(await readFile(join(launchDir, "launch.json"), "utf8"));
  if (manifest.schemaVersion !== 1 || manifest.appVersion !== "3.14.3")
    throw Error("Unsupported launcher manifest");
  const args = process.argv.slice(2);
  let writes = Promise.resolve();
  const receipt = (value) =>
    (writes = writes
      .then(async () => {
        const path = join(launchDir, `bridge-${process.pid}.json`),
          temp = `${path}.tmp`;
        await writeFile(
          temp,
          JSON.stringify({
            ...value,
            pid: process.pid,
            appVersion: manifest.appVersion,
            at: new Date().toISOString(),
          }),
          { mode: 0o600 },
        );
        await rename(temp, path);
      })
      .catch((error) => process.stderr.write(`ZCode Rust receipt: ${error.message}\n`)));
  if (args.includes("--prepare-storage")) {
    // 旧 Host 只能创建 Node Worker；复用安装包自己的迁移入口，不能把 Rust binary 交给 Worker。
    await receipt({ phase: "storage-bridge-loaded" });
    require(manifest.nodeBundle);
    return;
  }
  if (args[0] !== "app-server" || !args.includes("--stdio"))
    throw Error("Bridge only supports app-server --stdio");
  const env = { ...process.env };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.NODE_OPTIONS;
  // 子进程留在 Host 的进程组。独立 stdin 管道保证 bridge 被强杀时 Rust 收到 EOF，避免遗留运行时。
  const child = spawn(manifest.binary, args, {
    cwd: process.cwd(),
    env,
    stdio: ["pipe", "pipe", "pipe"],
    detached: false,
  });
  const onTerm = () => child.kill("SIGTERM");
  const onInt = () => child.kill("SIGINT");
  process.stdin.pipe(child.stdin);
  child.stdout.pipe(process.stdout, { end: false });
  child.stderr.pipe(process.stderr, { end: false });
  for (const stream of [
    process.stdin,
    process.stdout,
    process.stderr,
    child.stdin,
    child.stdout,
    child.stderr,
  ])
    stream.on("error", onTerm);
  process.on("SIGTERM", onTerm);
  process.on("SIGINT", onInt);
  const closed = new Promise((done, fail) => {
    child.once("error", fail);
    child.once("close", (code, signal) => done({ code, signal }));
  });
  child.once("spawn", () => {
    void receipt({ phase: "agent-started", rustPid: child.pid });
  });
  let result;
  try {
    result = await closed;
  } finally {
    process.stdin.unpipe(child.stdin);
    process.stdin.destroy();
  }
  process.removeListener("SIGTERM", onTerm);
  process.removeListener("SIGINT", onInt);
  await receipt({ phase: "agent-exited", rustPid: child.pid, ...result });
  process.exitCode = result.code ?? 1;
}
main().catch((error) => {
  process.stderr.write(`ZCode Rust bridge: ${error.message}\n`);
  process.exitCode = 1;
});
