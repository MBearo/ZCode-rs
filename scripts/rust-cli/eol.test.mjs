// packages/rust-cli 与 scripts/rust-cli 的换行规则（修复线 F56，docs/specs/rust-cli/prompts.md「换行」、
// node-store.md §2.1）：任何 core.autocrlf 设置下文本都检出成 LF；vendor 按字节保存；计数文件保留合并驱动。
// 真实的检出在临时仓库里做，只读仓库里的两份 .gitattributes。
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { promisify } from "node:util";

const exec = promisify(execFile);
const root = join(import.meta.dirname, "../..");
const ATTRIBUTES = ["packages/rust-cli/.gitattributes", "scripts/rust-cli/.gitattributes"];

/** `git check-attr` 的结果：{ 路径: { 属性: 值 } }。 */
async function attributes(cwd, env, names, paths) {
  const { stdout } = await exec("git", ["check-attr", ...names, "--", ...paths], { cwd, env });
  const result = {};
  for (const line of stdout.trim().split("\n")) {
    const [path, name, value] = line.split(": ");
    result[path] = { ...result[path], [name]: value };
  }
  return result;
}

test("编入与逐字比较的文本都是 text=auto、eol=lf；vendor 不转换；计数文件保留驱动", async () => {
  const texts = [
    "packages/rust-cli/crates/node-store/src/repo/migrations/0001_base_session_store.sql",
    "packages/rust-cli/prompts/system/identity.txt",
    "packages/rust-cli/prompts/loop/preamble.md",
    "packages/rust-cli/prompts/tools/Bash.json",
    "packages/rust-cli/crates/env-local/src/shell/macos-base.sb",
    "packages/rust-cli/crates/policy/src/readonly/readonly-policy.json",
    "packages/rust-cli/crates/platform/assets/zcode-builtin.json",
    // scripts/rust-cli 下的脚本与逐字比较的文本（`*` 一条规则，F60 起不点名只在本仓库的文件）。
    "scripts/rust-cli/fixture/model.mjs",
  ];
  const vendor = ["packages/rust-cli/vendor/THIRD-PARTY-NOTICES.txt"];
  const counter = "packages/rust-cli/build-number";
  const found = await attributes(
    root,
    process.env,
    ["text", "eol", "merge"],
    [...texts, ...vendor, counter],
  );
  for (const path of texts)
    assert.deepEqual(found[path], { text: "auto", eol: "lf", merge: "unspecified" }, path);
  for (const path of vendor) assert.equal(found[path].text, "unset", path);
  assert.deepEqual(found[counter], { text: "set", eol: "lf", merge: "zcode-build-number" });
});

test("core.autocrlf=true 的克隆：文本检出成 LF，vendor 按字节保存", async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "rust-cli-eol-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  // 不读本机的全局与系统配置（autocrlf、签名、hooks 等），只用本测试设的。
  const env = {
    ...process.env,
    GIT_CONFIG_GLOBAL: join(dir, "gitconfig"),
    GIT_CONFIG_NOSYSTEM: "1",
  };
  const source = join(dir, "source");
  const git = (cwd, ...args) => exec("git", args, { cwd, env });
  const files = {
    "packages/rust-cli/crates/node-store/src/repo/migrations/0001.sql":
      "create table a (\n  id text\n);\n",
    "packages/rust-cli/prompts/system/a.txt": "line one\nline two\n",
    "packages/rust-cli/prompts/tools/A.json": '{\n  "name": "A"\n}\n',
    "scripts/rust-cli/fixture/golden.txt": "Usage: zcode [options]\n\nOptions:\n",
    // 许可证原文里本来就有 CRLF：原样保存，也不在提交时规范成 LF。
    "packages/rust-cli/vendor/THIRD-PARTY-NOTICES.txt": "lf line\ncrlf line\r\n",
  };
  for (const path of ATTRIBUTES) files[path] = await readFile(join(root, path), "utf8");
  for (const [path, text] of Object.entries(files)) {
    await mkdir(dirname(join(source, path)), { recursive: true });
    await writeFile(join(source, path), text);
  }
  await git(source, "init", "-q", "-b", "main");
  await git(source, "config", "user.email", "dev@example.com");
  await git(source, "config", "user.name", "dev");
  await git(source, "add", ".");
  await git(source, "commit", "-qm", "base");
  await git(dir, "clone", "-q", "-c", "core.autocrlf=true", source, "clone");
  for (const [path, text] of Object.entries(files)) {
    assert.equal(await readFile(join(dir, "clone", path), "utf8"), text, path);
  }
});
