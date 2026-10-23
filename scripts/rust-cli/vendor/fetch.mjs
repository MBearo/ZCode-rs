#!/usr/bin/env node
// 第三方源码的 vendor（docs/specs/rust-cli/embedded-search.md §5–§7、§9）：
//   node scripts/rust-cli/vendor/fetch.mjs [--check] [<组件 id>…]
// 从 packages/rust-cli/vendor/sources.json 的官方地址下载归档，核对 SHA256，先在 target/vendor-work 下新建的空目录里
// 解包与检查，再把 keep 列出的文件复制进 packages/rust-cli/vendor/<into>（整个目录重建），按 edits 做精确替换。
// --check 不写文件，只核对仓库里的 vendor 目录与重新生成的结果逐字节相同。build.rs 不联网；联网只在这个脚本里。
import { execFile } from "node:child_process";
import { lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve, sep } from "node:path";
import { promisify } from "node:util";

import {
  applyEdits,
  compareTrees,
  kept,
  sha256,
  stripPath,
  unsafePath,
  validateComponent,
} from "./rules.mjs";

const execFileAsync = promisify(execFile);
const root = resolve(import.meta.dirname, "../../..");
const vendor = join(root, "packages/rust-cli/vendor");
// 下载与解包的临时目录放在 cargo 的 target 下（git 忽略、`cargo clean` 一起删），不写系统临时目录。
const scratch = join(root, "packages/rust-cli/target/vendor-work");

/** 目录下的普通文件（{相对路径: 内容}）；符号链接记进 `links`，不跟随。 */
async function walk(directory, base = directory, out = new Map(), links = []) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    const name = relative(base, path).split(sep).join("/");
    const info = await lstat(path);
    if (info.isSymbolicLink()) links.push(name);
    else if (info.isDirectory()) await walk(path, base, out, links);
    else if (info.isFile()) out.set(name, await readFile(path));
  }
  out.links = links;
  return out;
}

/** 下载归档；连接断开时最多再试两次（内容由 SHA256 核对，重试不影响结果）。 */
async function download(component) {
  for (let attempt = 1; ; attempt += 1) {
    try {
      const response = await fetch(component.url, { redirect: "follow" });
      if (!response.ok) {
        throw new Error(`${component.id}: 下载失败 ${response.status} ${component.url}`);
      }
      return Buffer.from(await response.arrayBuffer());
    } catch (error) {
      if (attempt >= 3) throw error;
      console.error(
        `${component.id}: 第 ${attempt} 次下载失败（${error.cause?.code ?? error.message}），再试`,
      );
      await new Promise((done) => setTimeout(done, 2000 * attempt));
    }
  }
}

/** 下载、核对、在新建的空目录里解包，返回 {相对路径: 内容}（已去掉前 strip 层、只留 keep、做过 edits）。 */
async function materialize(component) {
  const bytes = await download(component);
  const digest = sha256(bytes);
  if (digest !== component.sha256) {
    throw new Error(`${component.id}: SHA256 不符：期望 ${component.sha256}，得到 ${digest}`);
  }
  await mkdir(scratch, { recursive: true });
  const work = await mkdtemp(join(scratch, `fetch-${component.id}-`));
  try {
    const archive = join(work, "archive.tar.gz");
    await writeFile(archive, bytes);
    const { stdout } = await execFileAsync("tar", ["-tzf", archive], {
      maxBuffer: 256 * 1024 * 1024,
    });
    const bad = stdout.split("\n").filter((line) => line && unsafePath(line));
    if (bad.length)
      throw new Error(`${component.id}: 归档里有不安全的路径：${bad.slice(0, 3).join("、")}`);
    const extracted = join(work, "x");
    await mkdir(extracted);
    await execFileAsync("tar", ["-xzf", archive, "-C", extracted], { maxBuffer: 64 * 1024 * 1024 });
    const files = new Map();
    const all = await walk(extracted);
    for (const link of all.links) {
      const inner = stripPath(link, component.strip);
      if (inner !== null && kept(inner, component.keep)) {
        throw new Error(`${component.id}: 要保留的路径是符号链接，拒绝：${inner}`);
      }
    }
    for (const [path, content] of all) {
      const inner = stripPath(path, component.strip);
      if (inner !== null && kept(inner, component.keep)) files.set(inner, content);
    }
    for (const entry of component.keep) {
      if (![...files.keys()].some((path) => kept(path, [entry]))) {
        throw new Error(`${component.id}: keep 里的 ${entry} 在归档里没有`);
      }
    }
    for (const [file, edits] of Object.entries(component.edits ?? {})) {
      const text = files.get(file);
      if (!text) throw new Error(`${component.id}: 要改的文件 ${file} 不在归档里`);
      files.set(file, Buffer.from(applyEdits(text.toString("utf8"), edits, file), "utf8"));
    }
    return files;
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

async function readTree(directory) {
  try {
    return await walk(directory);
  } catch (error) {
    if (error.code === "ENOENT") return new Map();
    throw error;
  }
}

const argv = process.argv.slice(2);
const check = argv.includes("--check");
const wanted = argv.filter((arg) => arg !== "--check");
const sources = JSON.parse(await readFile(join(vendor, "sources.json"), "utf8"));
const components = sources.components.map(validateComponent);
const unknown = wanted.filter((id) => !components.some((component) => component.id === id));
if (unknown.length) throw new Error(`不认识的组件：${unknown.join("、")}`);

let failed = false;
for (const component of components) {
  if (wanted.length && !wanted.includes(component.id)) continue;
  const target = join(vendor, component.into);
  if (!target.startsWith(vendor + sep)) throw new Error(`${component.id}: into 越出 vendor 目录`);
  const files = await materialize(component);
  if (check) {
    const { missing, extra, changed } = compareTrees(files, await readTree(target));
    if (missing.length || extra.length || changed.length) {
      failed = true;
      console.error(`${component.id}: 与归档不符`, { missing, extra, changed });
    } else console.log(`${component.id} ${component.version}: OK（${files.size} 个文件）`);
    continue;
  }
  await rm(target, { recursive: true, force: true });
  for (const [path, content] of files) {
    const destination = join(target, path);
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(destination, content);
  }
  console.log(
    `${component.id} ${component.version}: 写入 ${relative(root, target)}（${files.size} 个文件）`,
  );
}
if (failed) process.exitCode = 1;
