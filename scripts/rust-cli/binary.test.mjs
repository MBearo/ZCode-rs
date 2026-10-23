// 可执行文件名只定义一处（工作线 F59，docs/specs/rust-cli/cli-args.md §13、README §4.1）：用户敲的命令是 `zcode`，
// cargo 产出的可执行文件是 `zcode-rs`。源头是 cli crate 的 `[[bin]] name`，脚本只从 binary.mjs 取。
// 只读仓库里的文件。
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import { join, relative, sep } from "node:path";
import test from "node:test";

import { BINARY_NAME, binaryFile, binaryPath } from "./binary.mjs";

const root = join(import.meta.dirname, "../..");
const scripts = import.meta.dirname;

/** cli crate 的 `[[bin]]` 段里的 `name`。 */
function binNames(manifest) {
  const names = [];
  for (const section of manifest.split(/^\[/m).slice(1)) {
    if (!section.startsWith("[bin]]")) continue;
    const name = /^name\s*=\s*"([^"]+)"/m.exec(section);
    if (name) names.push(name[1]);
  }
  return names;
}

test("可执行文件名与 cli crate 的 [[bin]] name 相同", async () => {
  assert.equal(BINARY_NAME, "zcode-rs");
  const manifest = await readFile(join(root, "packages/rust-cli/crates/cli/Cargo.toml"), "utf8");
  assert.deepEqual(binNames(manifest), [BINARY_NAME]);
});

test("文件名按平台加 .exe；路径在 target/<profile> 下", () => {
  assert.equal(binaryFile("darwin"), "zcode-rs");
  assert.equal(binaryFile("linux"), "zcode-rs");
  assert.equal(binaryFile("win32"), "zcode-rs.exe");
  assert.equal(
    binaryPath("/work", "debug", "linux"),
    join("/work", "packages/rust-cli/target", "debug", "zcode-rs"),
  );
  assert.equal(
    binaryPath("/work", join("x86_64-unknown-linux-musl", "release"), "linux"),
    join("/work", "packages/rust-cli/target", "x86_64-unknown-linux-musl", "release", "zcode-rs"),
  );
  assert.equal(
    binaryPath("/work", "release", "win32"),
    join("/work", "packages/rust-cli/target", "release", "zcode-rs.exe"),
  );
});

/** scripts/rust-cli 下的脚本（不含抓到的数据与夹具）。 */
async function scriptFiles(dir) {
  const files = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (["baselines", "fixtures", "node_modules"].includes(entry.name)) continue;
      files.push(...(await scriptFiles(path)));
    } else if (/\.(mjs|mts|cjs|js)$/.test(entry.name)) {
      files.push(path);
    }
  }
  return files;
}

// 写死的旧名字：字符串里只有 `zcode` / `zcode.exe`，或路径以 `/zcode` 结尾。注释不算（注释里的 zcode 是命令名）。
const HARDCODED = /["'`]zcode(\.exe)?["'`]|\/zcode["'`]/;

test("scripts/rust-cli 下没有别处写死可执行文件名", async () => {
  const hits = [];
  for (const file of await scriptFiles(scripts)) {
    const name = relative(scripts, file).split(sep).join("/");
    if (name === "binary.mjs" || name === "binary.test.mjs") continue;
    const lines = (await readFile(file, "utf8")).split("\n");
    lines.forEach((line, index) => {
      const code = line.trimStart();
      if (code.startsWith("//") || code.startsWith("*") || code.startsWith("/*")) return;
      if (HARDCODED.test(line)) hits.push(`${name}:${index + 1}: ${code}`);
    });
  }
  assert.deepEqual(hits, [], "改成从 scripts/rust-cli/binary.mjs 取");
});
