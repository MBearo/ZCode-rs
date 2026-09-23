import { spawn } from "node:child_process";
import { readdir } from "node:fs/promises";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
async function run(command, args) {
  await new Promise((done, fail) => {
    const child = spawn(command, args, {
      cwd: root,
      stdio: "inherit",
      env: {
        ...process.env,
        TSX_TSCONFIG_PATH: resolve(root, "packages/services/tests/tsconfig.zcode-cli-rust.json"),
      },
    });
    child.once("error", fail);
    child.once("close", (code, signal) =>
      code === 0 ? done() : fail(new Error(`${command} failed: ${signal ?? code}`)),
    );
  });
}
await run(process.execPath, ["apps/zcode-cli/packages/dynamic-workflow/scripts/generate-libs.mjs"]);
await run(process.execPath, [
  "--import",
  "tsx",
  "scripts/generate-zcode-cli-rust-prompt.mjs",
  "--check",
]);
await run(process.execPath, [
  "--import",
  "tsx",
  "scripts/generate-zcode-cli-rust-tool-schemas.mjs",
  "--check",
]);
await run(process.execPath, [
  "node_modules/typescript/bin/tsc",
  "-p",
  "packages/services/tests/tsconfig.zcode-cli-rust.json",
]);
await run("cargo", ["test", "--locked", "--manifest-path", "apps/zcode-cli-rust/Cargo.toml"]);
await run("cargo", [
  "build",
  "--examples",
  "--locked",
  "--manifest-path",
  "apps/zcode-cli-rust/Cargo.toml",
]);
await run("cargo", ["build", "--locked", "--manifest-path", "apps/zcode-cli-rust/Cargo.toml"]);
const tests = (await readdir(resolve(root, "packages/services/tests")))
  .filter((name) => /^zcode-cli-rust-.*\.test\.ts$/.test(name))
  .map((name) => `packages/services/tests/${name}`);
if (!tests.length) throw new Error("Rust App integration tests are missing");
await run(process.execPath, ["--import", "tsx", "--test", ...tests]);
