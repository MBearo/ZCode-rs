// scripts/rust-cli/build-number.mjs 的测试（docs/specs/rust-cli/version.md §2、§4、§5）：发版的加 1 与坏内容、
// 合并驱动取较大值（含真实的 git 合并）、版本拼接。全部在临时目录里做，只读仓库里的计数文件。
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { promisify } from "node:util";

import {
  counterFile,
  currentVersion,
  manifestFile,
  merge,
  parseBuildNumber,
  readBuildNumber,
  release,
  releaseVersion,
  workspace,
} from "./build-number.mjs";

const exec = promisify(execFile);
const script = join(import.meta.dirname, "build-number.mjs");
const manifestText = '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.0"\n';
const conflict = "<<<<<<< HEAD\n53\n=======\n55\n>>>>>>> lane\n";

async function fixture(t, counter) {
  const dir = await mkdtemp(join(tmpdir(), "rust-cli-build-number-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const paths = { manifest: join(dir, "Cargo.toml"), counter: join(dir, "build-number") };
  await writeFile(paths.manifest, manifestText);
  if (counter !== undefined) await writeFile(paths.counter, counter);
  return { dir, ...paths };
}

const exists = (path) =>
  stat(path).then(
    () => true,
    () => false,
  );

test("计数是一个十进制整数（version.md §2）", () => {
  assert.equal(parseBuildNumber("51\n"), 51);
  assert.equal(parseBuildNumber(" 41 "), 41);
  assert.equal(parseBuildNumber("51\r\n"), 51);
  for (const text of ["", "abc", "-1", "1.5", "0x10", conflict, "99999999999999999999"]) {
    assert.throws(() => parseBuildNumber(text), /is not a build number/u, JSON.stringify(text));
  }
});

test("版本：major.minor 来自 workspace 的版本，第三位是计数（version.md §1）", async () => {
  assert.equal(releaseVersion(manifestText, 12), "0.1.12");
  assert.throws(() => releaseVersion('[package]\nversion = "1.2.3"\n', 1), /workspace\.package/u);
  // 仓库里提交的文件是整数，版本按它拼。
  const count = await readBuildNumber();
  const [major, minor] = (await readFile(manifestFile, "utf8"))
    .split(/^\[workspace\.package\]\s*$/m)[1]
    .match(/^version\s*=\s*"(\d+)\.(\d+)\./m)
    .slice(1);
  assert.equal(await currentVersion(), `${major}.${minor}.${count}`);
  assert.equal(counterFile, join(workspace, "build-number"));
});

test("发版：读文件、加 1、原子写回，返回新版本（version.md §4）", async (t) => {
  const { counter, manifest } = await fixture(t, "51\n");
  assert.equal(await release({ manifest, counter }), "0.1.52");
  assert.equal(await readFile(counter, "utf8"), "52\n");
  assert.equal(await exists(`${counter}.tmp`), false);
  assert.equal(await currentVersion({ manifest, counter }), "0.1.52");
});

test("发版：文件缺失或不是整数时失败，不写", async (t) => {
  const missing = await fixture(t);
  await assert.rejects(release(missing), /is missing/u);
  assert.equal(await exists(missing.counter), false);
  for (const text of ["", "abc\n", conflict]) {
    const broken = await fixture(t, text);
    await assert.rejects(release(broken), /is not a build number/u);
    assert.equal(await readFile(broken.counter, "utf8"), text);
    assert.equal(await exists(`${broken.counter}.tmp`), false);
  }
});

test("合并驱动：两边取较大值写进 ours；坏内容时失败、ours 不动（version.md §5）", async (t) => {
  const { dir } = await fixture(t);
  const ours = join(dir, "ours");
  const theirs = join(dir, "theirs");
  for (const [a, b, larger] of [
    ["53\n", "55\n", "55\n"],
    ["57\n", "55\n", "57\n"],
    ["52\n", "52\n", "52\n"],
  ]) {
    await writeFile(ours, a);
    await writeFile(theirs, b);
    await merge({ ours, theirs });
    assert.equal(await readFile(ours, "utf8"), larger);
  }
  await writeFile(ours, "53\n");
  await writeFile(theirs, conflict);
  await assert.rejects(merge({ ours, theirs }), /is not a build number/u);
  assert.equal(await readFile(ours, "utf8"), "53\n");
});

test("真实的 git 合并：配置了驱动时取较大值，没配置时按文本冲突", async (t) => {
  const { dir } = await fixture(t);
  const repo = join(dir, "repo");
  // 不读本机的全局与系统配置（签名、hooks 等），只用本测试设的。
  const env = {
    ...process.env,
    GIT_CONFIG_GLOBAL: join(dir, "gitconfig"),
    GIT_CONFIG_NOSYSTEM: "1",
  };
  const git = (...args) => exec("git", args, { cwd: repo, env });
  await mkdir(join(repo, "packages/rust-cli"), { recursive: true });
  await git("init", "-q", "-b", "main");
  await git("config", "user.email", "dev@example.com");
  await git("config", "user.name", "dev");
  // 与仓库里相同的属性文件。
  await writeFile(
    join(repo, "packages/rust-cli/.gitattributes"),
    await readFile(join(workspace, ".gitattributes"), "utf8"),
  );
  const file = join(repo, "packages/rust-cli/build-number");
  await writeFile(file, "51\n");
  await git("add", ".");
  await git("commit", "-qm", "base");
  await git("checkout", "-qb", "lane");
  await writeFile(file, "55\n");
  await git("commit", "-qam", "lane");
  await git("checkout", "-q", "main");
  await writeFile(file, "53\n");
  await git("commit", "-qam", "main");
  assert.equal(
    (await git("check-attr", "merge", "packages/rust-cli/build-number")).stdout.trim(),
    "packages/rust-cli/build-number: merge: zcode-build-number",
  );
  // 没配置驱动：git 的缺省文本合并，同一行冲突。
  await assert.rejects(git("merge", "-q", "lane", "-m", "merge"));
  assert.match(await readFile(file, "utf8"), /^<<<<<<< /u);
  await git("merge", "--abort");
  // 配置驱动（version.md §5 的命令；这里用脚本的绝对路径）：自动取较大值。
  await git("config", "merge.zcode-build-number.driver", `node "${script}" merge %O %A %B`);
  await git("merge", "-q", "lane", "-m", "merge");
  assert.equal(await readFile(file, "utf8"), "55\n");
});

test("命令行：用法错误退出码 2", async () => {
  await assert.rejects(exec(process.execPath, [script]), (error) => error.code === 2);
  await assert.rejects(exec(process.execPath, [script, "merge", "a"]), (error) => error.code === 2);
});
