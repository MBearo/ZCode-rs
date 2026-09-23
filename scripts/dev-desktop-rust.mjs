import { spawn } from "node:child_process";
import { mkdir, readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { parseArgs } from "node:util";

const root = resolve(import.meta.dirname, "..");
const { values } = parseArgs({
  options: {
    config: { type: "string" },
    "data-dir": { type: "string" },
    help: { type: "boolean" },
    debug: { type: "boolean" },
  },
});
if (values.help) {
  console.info(
    "Usage: pnpm dev:desktop:rust [--config <fixture model.json>] [--data-dir <isolated experiment directory>] [--debug] (default: release)",
  );
  process.exit(values.help ? 0 : 1);
}
const config = values.config ? resolve(values.config) : undefined;
const dataDir = values["data-dir"] ? resolve(values["data-dir"]) : undefined;
const model = config ? JSON.parse(await readFile(config, "utf8")) : undefined;
if (model && (!model.providerId || !model.modelId || !model.baseUrl || !model.reasoningLevel))
  throw new Error("Model config requires providerId, modelId, reasoningLevel and baseUrl");
if (dataDir) await mkdir(dataDir, { recursive: true });
async function run(command, args, env = process.env) {
  await new Promise((done, fail) => {
    const child = spawn(command, args, { cwd: root, env, stdio: "inherit" });
    const interrupt = () => child.kill("SIGINT");
    const terminate = () => child.kill("SIGTERM");
    process.once("SIGINT", interrupt);
    process.once("SIGTERM", terminate);
    child.once("error", fail);
    child.once("close", (code, signal) => {
      process.removeListener("SIGINT", interrupt);
      process.removeListener("SIGTERM", terminate);
      if (code === 0 || signal === "SIGINT" || signal === "SIGTERM") done();
      else fail(new Error(`${command} exited with ${signal ?? code}`));
    });
  });
}
const profile = values.debug ? "debug" : "release";
await run(
  "cargo",
  [
    "build",
    ...(values.debug ? [] : ["--release"]),
    "--locked",
    "--manifest-path",
    "apps/zcode-rust/Cargo.toml",
  ],
  {
    ...process.env,
    CARGO_INCREMENTAL: "0",
  },
);
console.info(
  model
    ? `Rust fixture model: ${model.providerId}/${model.modelId}`
    : "Rust runtime uses the existing App provider settings and account authentication.",
);
await run(process.execPath, [resolve(root, "scripts/dev-desktop-env.mjs"), "production"], {
  ...process.env,
  ZCODE_AGENT_SERVER_RUNTIME: "rust-core",
  ZCODE_AGENT_SERVER_COMMAND: resolve(
    root,
    `apps/zcode-rust/target/${profile}/zcode-rust${process.platform === "win32" ? ".exe" : ""}`,
  ),
  ZCODE_AGENT_SERVER_ARGS_JSON: JSON.stringify([
    "app-server",
    "--stdio",
    ...(dataDir ? ["--data-dir", join(dataDir, "agent")] : []),
    ...(config ? ["--config", config] : []),
  ]),
  ...(dataDir
    ? {
        ZCODE_DATA_BASE_DIR: join(dataDir, "app"),
        ZCODE_DESKTOP_APPLICATION_NAME: "ZCode Rust Core",
        ZCODE_DESKTOP_USER_DATA_DIR: join(dataDir, "electron"),
        ZCODE_DESKTOP_SESSION_DATA_DIR: join(dataDir, "electron-session"),
      }
    : {}),
  ZCODE_DISABLE_FIXED_REMOTE_DEBUGGING_PORT: "1",
});
