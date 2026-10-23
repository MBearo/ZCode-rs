#!/usr/bin/env node
// rust-cli 的计数文件 packages/rust-cli/build-number（docs/specs/rust-cli/version.md §2、§4、§5）。
//
//   node scripts/rust-cli/build-number.mjs release          发版：读文件、加 1、原子写回，打印新版本
//   node scripts/rust-cli/build-number.mjs merge %O %A %B   git 合并驱动：两边取较大值写进 %A
//
// 规则与 crates/cli/src/build_number.rs 相同（十进制整数加换行，坏内容报错不回退）。脚本不 commit、不 push。
import { readFile, rename, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(import.meta.dirname, "../..");
export const workspace = join(root, "packages/rust-cli");
export const counterFile = join(workspace, "build-number");
export const manifestFile = join(workspace, "Cargo.toml");

/** 计数：去掉首尾空白后的一个十进制整数。 */
export function parseBuildNumber(text) {
  const digits = text.trim();
  const count = /^\d+$/.test(digits) ? Number(digits) : Number.NaN;
  if (!Number.isSafeInteger(count)) {
    throw new Error(
      `${JSON.stringify(text)} is not a build number; write back one decimal integer (after a merge conflict, keep the larger number)`,
    );
  }
  return count;
}

/** release 的版本：`[workspace.package]` 版本的 major、minor 加计数（version.md §1）。 */
export function releaseVersion(manifest, count) {
  const section = manifest.split(/^\[workspace\.package\]\s*$/m)[1] ?? "";
  const match = section.match(/^version\s*=\s*"(\d+)\.(\d+)\./m);
  if (!match) throw new Error("Cargo.toml 的 [workspace.package] 里没有 version");
  return `${match[1]}.${match[2]}.${count}`;
}

export async function readBuildNumber(path = counterFile) {
  let text;
  try {
    text = await readFile(path, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") {
      throw new Error(`${path} is missing; it holds the release build number`);
    }
    throw error;
  }
  try {
    return parseBuildNumber(text);
  } catch (error) {
    throw new Error(`${path}: ${error.message}`);
  }
}

/** 计数文件现在对应的版本（CI 与 Linux 构建的报告用，不改文件）。 */
export async function currentVersion({ manifest = manifestFile, counter = counterFile } = {}) {
  return releaseVersion(await readFile(manifest, "utf8"), await readBuildNumber(counter));
}

/** 原子写：先写同目录的 `.tmp` 再 rename，中途被打断也不会留下半个数字。 */
async function writeCount(path, count) {
  const temp = `${path}.tmp`;
  await writeFile(temp, `${count}\n`);
  await rename(temp, path);
}

/** 发版：加 1 写回，返回新版本（version.md §4）。坏内容时不写。 */
export async function release({ manifest = manifestFile, counter = counterFile } = {}) {
  const count = (await readBuildNumber(counter)) + 1;
  if (!Number.isSafeInteger(count)) throw new Error(`${counter}: the build number cannot grow`);
  const version = releaseVersion(await readFile(manifest, "utf8"), count);
  await writeCount(counter, count);
  return version;
}

/** 合并驱动（version.md §5）：两边取较大值写进 ours；有一边不是整数时抛错，ours 不动。 */
export async function merge({ ours, theirs }) {
  const larger = Math.max(await readBuildNumber(ours), await readBuildNumber(theirs));
  await writeCount(ours, larger);
  return larger;
}

async function main(argv) {
  const [command, ...rest] = argv;
  if (command === "release" && rest.length === 0) {
    process.stdout.write(`${await release()}\n`);
    return 0;
  }
  if (command === "merge" && rest.length === 3) {
    // git 传 %O %A %B：基线用不到（号只增不减），结果写回 %A。
    await merge({ ours: rest[1], theirs: rest[2] });
    return 0;
  }
  process.stderr.write(
    "usage: node scripts/rust-cli/build-number.mjs release\n" +
      "       node scripts/rust-cli/build-number.mjs merge <base> <ours> <theirs>\n",
  );
  return 2;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = await main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`build-number: ${error.message}\n`);
    process.exitCode = 1;
  }
}
