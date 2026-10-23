#!/usr/bin/env node
// 内嵌搜索工具的第三方许可证声明（docs/specs/rust-cli/embedded-search.md §9）：
//   node scripts/rust-cli/vendor/notice.mjs [--check]
// 生成 packages/rust-cli/vendor/THIRD-PARTY-NOTICES.txt（随平台包分发；--check 只比较）。内容：
//   1. vendor/sources.json 里每个组件的许可证文件（rg 的命令行、bfs、Oniguruma、ugrep、PCRE2、brotli）；
//   2. 三个 vendor crate 经 crates.io 引入的包（`cargo metadata` 的依赖图）：-sys crate 自带的 C 库许可证（zlib、bzip2、
//      zstd）与每个包自己的许可证文件。
// 只读本地文件（vendor 目录与 cargo 的 registry 源码）；cargo 在 packages/rust-cli 下执行（工具链文件，README §9）。
import { execFile } from "node:child_process";
import { readFile, readdir, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const root = resolve(import.meta.dirname, "../../..");
const workspace = join(root, "packages/rust-cli");
const vendor = join(workspace, "vendor");
const output = join(vendor, "THIRD-PARTY-NOTICES.txt");

/** 三个 vendor crate（它们的依赖都是内嵌工具带进来的）。 */
const ROOTS = ["rust-cli-vendor-rg", "rust-cli-vendor-bfs", "rust-cli-vendor-ugrep"];

/** -sys crate 自带的 C 库与它们的许可证文件（相对 crate 目录）。zstd 是 BSD-3 / GPLv2 双许可，取 BSD-3。 */
const BUNDLED = {
  "libz-sys": { library: "zlib", files: ["src/zlib/LICENSE"] },
  "bzip2-sys": { library: "bzip2 1.0.8", files: ["bzip2-1.0.8/LICENSE"] },
  "zstd-sys": { library: "zstd 1.5.7（BSD-3-Clause，双许可里取 BSD）", files: ["zstd/LICENSE"] },
};

/** 包目录里算许可证的文件名。 */
const LICENSE_NAME = /^(LICEN[CS]E|COPYING|UNLICENSE|NOTICE)([-._].*)?$/i;

const rule = "=".repeat(78);

function section(title, texts) {
  return [rule, title, rule, "", ...texts.map((text) => text.trimEnd() + "\n")].join("\n");
}

export async function render({ sources, metadata, read, list }) {
  const parts = [
    "Third-party software embedded in zcode (rg, bfs, ugrep).",
    "zcode 内嵌的第三方软件（docs/specs/rust-cli/embedded-search.md §9）。由 scripts/rust-cli/vendor/notice.mjs 生成。",
    "",
  ];
  for (const component of sources.components) {
    const texts = [];
    for (const file of component.licenseFiles ?? []) {
      texts.push(`--- ${file}\n\n${await read(join(vendor, component.into, file))}`);
    }
    if (!texts.length) throw new Error(`${component.id}: sources.json 没有 licenseFiles`);
    parts.push(section(`${component.id} ${component.version} (${component.license})`, texts));
  }
  // 依赖图：从三个 vendor crate 出发的普通依赖（构建依赖 cc 不进二进制）。
  const byId = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const seen = new Set();
  const queue = metadata.packages.filter((pkg) => ROOTS.includes(pkg.name)).map((pkg) => pkg.id);
  if (queue.length !== ROOTS.length) throw new Error("cargo metadata 里没有全部的 vendor crate");
  while (queue.length) {
    const id = queue.shift();
    for (const dep of nodes.get(id)?.deps ?? []) {
      if (!dep.dep_kinds.some((kind) => kind.kind === null)) continue;
      if (!seen.has(dep.pkg)) {
        seen.add(dep.pkg);
        queue.push(dep.pkg);
      }
    }
  }
  const crates = [...seen]
    .map((id) => byId.get(id))
    .filter((pkg) => pkg.source)
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
  for (const pkg of crates) {
    const dir = dirname(pkg.manifest_path);
    const texts = [];
    const bundled = BUNDLED[pkg.name];
    if (bundled) {
      for (const file of bundled.files) {
        texts.push(`--- ${bundled.library}: ${file}\n\n${await read(join(dir, file))}`);
      }
    }
    for (const name of (await list(dir)).filter((name) => LICENSE_NAME.test(name)).sort()) {
      texts.push(`--- ${name}\n\n${await read(join(dir, name))}`);
    }
    if (!texts.length) throw new Error(`${pkg.name} ${pkg.version}: 包里没有许可证文件`);
    parts.push(section(`${pkg.name} ${pkg.version} (${pkg.license ?? "见下"}) — crates.io`, texts));
  }
  return parts.join("\n");
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const check = process.argv.includes("--check");
  const sources = JSON.parse(await readFile(join(vendor, "sources.json"), "utf8"));
  const { stdout } = await execFileAsync(
    "cargo",
    [
      "metadata",
      "--format-version",
      "1",
      "--locked",
      "--manifest-path",
      join(workspace, "Cargo.toml"),
    ],
    { cwd: workspace, maxBuffer: 256 * 1024 * 1024 },
  );
  const text = await render({
    sources,
    metadata: JSON.parse(stdout),
    read: (path) => readFile(path, "utf8"),
    list: async (dir) =>
      (await readdir(dir, { withFileTypes: true })).filter((e) => e.isFile()).map((e) => e.name),
  });
  if (check) {
    const current = await readFile(output, "utf8").catch(() => "");
    if (current !== text) {
      console.error(
        "THIRD-PARTY-NOTICES.txt 不是最新的：运行 node scripts/rust-cli/vendor/notice.mjs",
      );
      process.exitCode = 1;
    } else console.log("THIRD-PARTY-NOTICES.txt: OK");
  } else {
    await writeFile(output, text);
    console.log(`写入 ${output.slice(root.length + 1)}（${text.length} 字节）`);
  }
}
