#!/usr/bin/env node
// packages/rust-cli 的边界检查（docs/specs/rust-cli/README.md §3.1、§3.2、§4）。
//
// 规则表在 boundaries/policy.mjs，检查逻辑在 boundaries/check.mjs（纯函数，boundaries/check.test.mjs 每类
// 违规一个用例）。这里只收集输入：
// - 依赖图取自 `cargo metadata`（Cargo 自己解析，表格写法、`package =` 改名、按平台的依赖都按真实包名）；
// - 每个 crate 的 .rs 文件、根与 crate 自己的 clippy.toml；
// - 接口快照的源码与已提交的 interfaces.json（boundaries/interfaces.mjs）。
// README 表格与规则表的一致性只在本仓库检查（scripts/rust-cli/repo-only/check.mjs，工作线 F60）。
import { execFile } from "node:child_process";
import { readdir, readFile } from "node:fs/promises";
import { relative, resolve, sep } from "node:path";
import { promisify } from "node:util";

import {
  checkBlastRadius,
  checkClippy,
  checkDefinitions,
  checkGraph,
  checkSources,
  checkStableDependencies,
  internalGraph,
  parseClippy,
} from "./boundaries/check.mjs";
import { SNAPSHOT, checkInterfaces, readSources } from "./boundaries/interfaces.mjs";
import * as policy from "./boundaries/policy.mjs";

const root = resolve(import.meta.dirname, "../../packages/rust-cli");
const crates = resolve(root, "crates");

async function* files(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name !== "target") yield* files(path);
    } else yield path;
  }
}

async function optional(path) {
  try {
    return await readFile(path, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}

const { stdout } = await promisify(execFile)(
  "cargo",
  [
    "metadata",
    "--format-version",
    "1",
    "--locked",
    "--no-deps",
    "--manifest-path",
    resolve(root, "Cargo.toml"),
  ],
  // 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
  { cwd: root, maxBuffer: 64 * 1024 * 1024 },
);
const metadata = JSON.parse(stdout);
const members = new Set(metadata.workspace_members);
const crateOf = (manifest) => relative(crates, resolve(manifest, "..")).split(sep)[0];
const packages = metadata.packages
  .filter((entry) => members.has(entry.id))
  .map((entry) => ({
    crate: crateOf(entry.manifest_path),
    manifest: relative(root, entry.manifest_path).replaceAll("\\", "/"),
    dependencies: entry.dependencies,
  }));
const internal = new Map(
  metadata.packages
    .filter((entry) => members.has(entry.id))
    .map((entry) => [entry.name, crateOf(entry.manifest_path)]),
);
const isInside = (path) => resolve(path) === root || resolve(path).startsWith(root + sep);

const sources = [];
const crateClippy = {};
for (const entry of await readdir(crates, { withFileTypes: true })) {
  if (!entry.isDirectory()) continue;
  const directory = resolve(crates, entry.name);
  for await (const path of files(directory)) {
    if (path.endsWith(".rs")) {
      sources.push({
        crate: entry.name,
        file: relative(root, path).replaceAll("\\", "/"),
        text: await readFile(path, "utf8"),
      });
    }
  }
  for (const name of ["clippy.toml", ".clippy.toml"]) {
    const text = await optional(resolve(directory, name));
    if (text !== null) crateClippy[entry.name] = text;
  }
}
const rootClippy = await readFile(resolve(root, "clippy.toml"), "utf8");

// 三类检查（docs/specs/rust-cli/remote-exec.md §15.6）：依赖图之外的波及范围、稳定依赖与接口快照。
const graph = internalGraph(packages, internal);
const failures = [
  ...checkGraph(policy, packages, internal, isInside),
  ...checkBlastRadius(policy, graph),
  ...checkStableDependencies(policy, graph),
  ...checkInterfaces(await readSources(root), JSON.parse(await readFile(SNAPSHOT, "utf8"))),
  ...checkClippy(policy, rootClippy, crateClippy),
  ...checkSources(policy, parseClippy(rootClippy).entries, sources),
  ...checkDefinitions(policy, sources),
];

if (failures.length) {
  for (const failure of failures) console.error(failure);
  process.exitCode = 1;
} else console.log("rust-cli boundaries: OK");
