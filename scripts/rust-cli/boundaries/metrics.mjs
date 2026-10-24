#!/usr/bin/env node
// 结构指标（docs/specs/rust-cli/remote-exec.md §15.6、§18.7）：只出报告，不做门禁；看变化方向，不设阈值。
//
//   node scripts/rust-cli/boundaries/metrics.mjs [--root <检出的根>] [--commits <N>] [--json <文件>]
//
// - 每个 crate：Ca（被几个 crate 依赖）、Ce（依赖几个）、I = Ce /（Ca + Ce）、A = trait /（trait + struct + enum）、
//   D = |A + I − 1|（Martin）；依赖按全部种类算（与依赖表、稳定依赖检查的口径相同）；类型只数产品代码（不含测试文件）。
// - 端口宽度：接口快照覆盖的每个 trait 的方法数（boundaries/interfaces.mjs）。
// - 接口宽度：每条依赖边上，依赖方的产品代码经 `rust_cli_<被依赖方>::…` 用到的不同名字数（粗测：按路径的最后一段）。
// - 整图：传播成本（可达矩阵含对角线的密度，MacCormack 2006）与 NCCD（CCD / 同样大小的平衡二叉树的 CCD，Lakos）。
// - 共改：近 N 个不是合并的提交里，每个提交改到几个 crate（平均与分布）。
// 不同粒度（crate / 模块 / 文件）的数不能互相比。
import { execFile } from "node:child_process";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";
import { parseArgs, promisify } from "node:util";

import { instability, internalGraph, isTestFile, stripCode } from "./check.mjs";
import { PORT_TRAITS, extractTrait, readSources } from "./interfaces.mjs";

const run = promisify(execFile);

/** 可达集合（不含自己）。 */
function reach(graph, crate) {
  const seen = new Set();
  const stack = [...(graph.get(crate) ?? [])];
  while (stack.length) {
    const next = stack.pop();
    if (seen.has(next)) continue;
    seen.add(next);
    stack.push(...(graph.get(next) ?? []));
  }
  return seen;
}

/** 传播成本：可达矩阵（含对角线）里 1 的比例。 */
export function propagationCost(graph) {
  const n = graph.size;
  if (n === 0) return 0;
  let ones = 0;
  for (const crate of graph.keys()) ones += reach(graph, crate).size + 1;
  return ones / (n * n);
}

/** NCCD：CCD（每个 crate 依赖的 crate 数含自己，求和）除以 n 个节点的平衡二叉树的 CCD = (n+1)·log2(n+1) − n。 */
export function nccd(graph) {
  const n = graph.size;
  if (n === 0) return 0;
  let ccd = 0;
  for (const crate of graph.keys()) ccd += reach(graph, crate).size + 1;
  return ccd / ((n + 1) * Math.log2(n + 1) - n);
}

/** 产品代码里 trait / struct / enum 的个数（去掉注释与字符串之后数声明）。 */
export function abstractness(sources) {
  let traits = 0;
  let concrete = 0;
  for (const text of sources) {
    const code = stripCode(text);
    traits += (code.match(/(^|[^A-Za-z0-9_])trait\s+[A-Z][A-Za-z0-9_]*/g) ?? []).length;
    concrete += (code.match(/(^|[^A-Za-z0-9_])(struct|enum)\s+[A-Z][A-Za-z0-9_]*/g) ?? []).length;
  }
  return { traits, concrete, a: traits + concrete === 0 ? 0 : traits / (traits + concrete) };
}

/**
 * 依赖方的产品代码经 `rust_cli_<被依赖方>::` 用到的不同名字：路径里第一个大写开头的段（类型、trait），没有时是最后一段
 * （函数、常量、模块）；`use` 的花括号展开，`as` 的别名不算。
 */
export function referencedNames(sources, dependency) {
  const crate = `rust_cli_${dependency.replaceAll("-", "_")}`;
  const names = new Set();
  const pattern = new RegExp(
    `\\b${crate}::((?:[A-Za-z0-9_]+::)*)(\\{[^;]*?\\}|[A-Za-z0-9_]+)`,
    "g",
  );
  const add = (segments) => {
    const parts = segments.filter((part) => part && part !== "self" && part !== "*");
    const name = parts.find((part) => /^[A-Z]/.test(part)) ?? parts.at(-1);
    if (name) names.add(name);
  };
  for (const text of sources) {
    const code = stripCode(text);
    for (const match of code.matchAll(pattern)) {
      const prefix = match[1].split("::").filter(Boolean);
      if (match[2].startsWith("{")) {
        for (const part of match[2].slice(1, -1).split(",")) {
          const path = part.trim().replace(/\s+as\s+\w+$/, "");
          if (path) add([...prefix, ...path.replace(/[{}\s]/g, "").split("::")]);
        }
      } else {
        // 路径后面紧跟的 `::名字` 也看（`ids::SessionId::new` 取 SessionId）。
        const tail = code.slice(match.index + match[0].length).match(/^(?:::[A-Za-z0-9_]+)*/)[0];
        add([...prefix, match[2], ...tail.split("::").filter(Boolean)]);
      }
    }
  }
  return names;
}

async function* rustFiles(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name !== "target") yield* rustFiles(path);
    } else if (entry.name.endsWith(".rs")) yield path;
  }
}

async function coChange(root, commits) {
  try {
    const { stdout } = await run(
      "git",
      [
        "log",
        "--no-merges",
        "-n",
        String(commits),
        "--name-only",
        "--format=@%H",
        "--",
        "packages/rust-cli/crates",
      ],
      { cwd: root, maxBuffer: 64 * 1024 * 1024 },
    );
    const counts = [];
    for (const block of stdout.split("@").slice(1)) {
      const crates = new Set(
        block
          .split("\n")
          .slice(1)
          .map((line) => line.match(/^packages\/rust-cli\/crates\/([^/]+)\//)?.[1])
          .filter(Boolean),
      );
      if (crates.size) counts.push(crates.size);
    }
    const mean = counts.length ? counts.reduce((a, b) => a + b, 0) / counts.length : 0;
    return { commits: counts.length, mean, max: Math.max(0, ...counts) };
  } catch {
    return { commits: 0, mean: 0, max: 0, note: "not a git checkout" };
  }
}

export async function measure(root, { commits = 50 } = {}) {
  const workspace = resolve(root, "packages/rust-cli");
  const crates = resolve(workspace, "crates");
  const { stdout } = await run(
    "cargo",
    [
      "metadata",
      "--format-version",
      "1",
      "--no-deps",
      "--manifest-path",
      resolve(workspace, "Cargo.toml"),
    ],
    { cwd: workspace, maxBuffer: 64 * 1024 * 1024 },
  );
  const metadata = JSON.parse(stdout);
  const members = new Set(metadata.workspace_members);
  const crateOf = (manifest) => relative(crates, resolve(manifest, "..")).split(sep)[0];
  const packages = metadata.packages
    .filter((entry) => members.has(entry.id))
    .map((entry) => ({ crate: crateOf(entry.manifest_path), dependencies: entry.dependencies }));
  const internal = new Map(
    metadata.packages
      .filter((entry) => members.has(entry.id))
      .map((entry) => [entry.name, crateOf(entry.manifest_path)]),
  );
  const graph = internalGraph(packages, internal);
  const sources = new Map([...graph.keys()].map((crate) => [crate, []]));
  for (const crate of graph.keys()) {
    for await (const path of rustFiles(resolve(crates, crate))) {
      const file = relative(workspace, path).replaceAll("\\", "/");
      if (!isTestFile(file) && !/(_tests_\w+|\/test_support|\/test_more)\.rs$/.test(file))
        sources.get(crate).push(await readFile(path, "utf8"));
    }
  }
  const stability = instability(graph);
  const perCrate = [...graph.keys()].sort().map((crate) => {
    const { ca, ce, i } = stability.get(crate);
    const { traits, concrete, a } = abstractness(sources.get(crate));
    return { crate, ca, ce, i, traits, types: concrete, a, d: Math.abs(a + i - 1) };
  });
  const edges = [];
  for (const [crate, targets] of graph) {
    for (const target of [...targets].sort()) {
      edges.push({
        from: crate,
        to: target,
        names: referencedNames(sources.get(crate), target).size,
      });
    }
  }
  edges.sort((x, y) => y.names - x.names || x.from.localeCompare(y.from));
  const portSources = await readSources(workspace);
  const ports = [];
  for (const [file, names] of Object.entries(PORT_TRAITS)) {
    for (const name of names) {
      const trait = portSources[file] ? extractTrait(portSources[file], name) : null;
      if (trait) ports.push({ trait: name, methods: trait.methods.length });
    }
  }
  return {
    crates: graph.size,
    edges: edges.length,
    propagationCost: propagationCost(graph),
    nccd: nccd(graph),
    perCrate,
    ports,
    interfaceWidth: edges,
    coChange: await coChange(root, commits),
  };
}

function render(result) {
  const fixed = (value) => value.toFixed(2);
  const lines = [
    `crates ${result.crates}，依赖边 ${result.edges}，传播成本 ${result.propagationCost.toFixed(3)}，NCCD ${fixed(result.nccd)}`,
    `共改：近 ${result.coChange.commits} 个提交，平均每个改 ${fixed(result.coChange.mean)} 个 crate，最多 ${result.coChange.max} 个`,
    "",
    "crate              Ca  Ce     I  trait  type     A     D",
  ];
  for (const row of result.perCrate) {
    lines.push(
      `${row.crate.padEnd(17)} ${String(row.ca).padStart(3)} ${String(row.ce).padStart(3)} ${fixed(row.i).padStart(5)} ${String(row.traits).padStart(6)} ${String(row.types).padStart(5)} ${fixed(row.a).padStart(5)} ${fixed(row.d).padStart(5)}`,
    );
  }
  lines.push(
    "",
    "端口宽度（方法数）：" + result.ports.map((p) => `${p.trait} ${p.methods}`).join("，"),
  );
  lines.push(
    "接口宽度（依赖边上用到的不同名字数，前 15）：" +
      result.interfaceWidth
        .slice(0, 15)
        .map((edge) => `${edge.from}→${edge.to} ${edge.names}`)
        .join("，"),
  );
  return lines.join("\n");
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const { values } = parseArgs({
    options: {
      root: { type: "string", default: resolve(import.meta.dirname, "../../..") },
      commits: { type: "string", default: "50" },
      json: { type: "string" },
    },
  });
  const result = await measure(resolve(values.root), { commits: Number(values.commits) });
  console.log(render(result));
  if (values.json) await writeFile(values.json, `${JSON.stringify(result, null, 2)}\n`);
}
