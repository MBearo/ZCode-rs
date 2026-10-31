// 散写数字的棘轮（docs/specs/rust-cli/constants.md §4，工作线 F122）：业务代码里有含义的数字是命名常量，散写的处数
// 按文件记在 numbers.json，只许降低。
//
//   node scripts/rust-cli/boundaries/numbers.mjs --update           降低基线（只降不升；没有基线时写入现状）
//   node scripts/rust-cli/boundaries/numbers.mjs --report           按 crate 的处数
//   node scripts/rust-cli/boundaries/numbers.mjs --report tools     一个 crate 里按文件的处数
//   node scripts/rust-cli/boundaries/numbers.mjs --report crates/tools/src/bash.rs   一个文件里被算的每一处
//
// 纯函数（countNumbers、checkNumbers、updateBaseline、checkRaises）给 check-boundaries.mjs 与 pre-push 用，
// numbers.test.mjs 每条规则一个用例；读写文件只在命令行部分。
import { readdir, readFile, realpath, writeFile } from "node:fs/promises";
import { relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

import { isTestOnly, stripCode } from "./check.mjs";

export const BASELINE = resolve(import.meta.dirname, "numbers.json");
const UPDATE = "node scripts/rust-cli/boundaries/numbers.mjs --update";

// ── 扫描范围 ──

/** 扫描的文件：`crates/<crate>/src/` 下的 .rs，不含测试文件、kernel-api 的测试替身与 `#[cfg(test)]` 引进的文件。 */
export function isScanned(file, testModules = new Set()) {
  if (!/^crates\/[^/]+\/src\/.+\.rs$/.test(file)) return false;
  if (isTestOnly(file)) return false;
  if (/^crates\/kernel-api\/src\/testing(\.rs|\/)/.test(file)) return false;
  return !testModules.has(file);
}

/** 属性 `#[cfg(…)]` 只在测试里编译：含 `test` 或 `feature = "testing"`，又不是 `not(test)`。 */
function isTestCfg(inner) {
  return /\btest\b|"testing"/.test(inner) && !/\bnot\s*\(\s*test\s*\)/.test(inner);
}

const CFG = /#\[\s*cfg\s*\(((?:[^()]|\([^()]*\))*)\)\s*\]/g;
const MOD = /^\s*((?:#\[[^\]]*\]\s*)*)(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;/;

/** 一个文件里的 `mod x;` 对应的文件（`#[path]` 按所在目录；mod.rs / lib.rs / main.rs 之外的文件的子模块在同名目录里）。 */
function moduleFiles(file, attributes, name) {
  const directory = file.slice(0, file.lastIndexOf("/"));
  const path = attributes.match(/#\[\s*path\s*=\s*"([^"]+)"\s*\]/);
  if (path) return [normalize(`${directory}/${path[1]}`)];
  const stem = file.slice(directory.length + 1, -".rs".length);
  const parent = ["lib", "main", "mod"].includes(stem) ? directory : `${directory}/${stem}`;
  return [`${parent}/${name}.rs`, `${parent}/${name}/mod.rs`];
}

function normalize(path) {
  const out = [];
  for (const part of path.split("/")) {
    if (part === "..") out.pop();
    else if (part !== ".") out.push(part);
  }
  return out.join("/");
}

/** 一个文件声明的子模块文件。`onlyTest` 时只要 `#[cfg(test)]` 的 `mod x;`，否则全部。 */
function declaredFiles(file, text, onlyTest) {
  // stripCode 只把注释与字面量的内容换成空格，位置不变：在代码里找声明，`#[path = "…"]` 的值按同一位置从原文取。
  const code = stripCode(text);
  const out = [];
  const take = (start, length) => {
    const match = text.slice(start, start + length).match(MOD);
    if (match) out.push(...moduleFiles(file, match[1], match[2]));
  };
  if (onlyTest) {
    for (const match of code.matchAll(CFG)) {
      if (!isTestCfg(match[1])) continue;
      const start = match.index + match[0].length;
      const declaration = code.slice(start).match(MOD);
      if (declaration) take(start, declaration[0].length);
    }
  } else {
    const any = /(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?\bmod\s+\w+\s*;/g;
    for (const match of code.matchAll(any)) take(match.index, match[0].length);
  }
  return out;
}

/** 只在测试里编译的模块文件：`#[cfg(test)]` 的 `mod x;` 引进的文件，以及它们再引进的文件。 */
export function testModuleFiles(sources) {
  const texts = new Map(sources.map(({ file, text }) => [file, text]));
  const found = new Set();
  const pending = [];
  const add = (children) => {
    for (const child of children) {
      if (texts.has(child) && !found.has(child)) {
        found.add(child);
        pending.push(child);
      }
    }
  };
  for (const [file, text] of texts) add(declaredFiles(file, text, true));
  while (pending.length) {
    const file = pending.pop();
    add(declaredFiles(file, texts.get(file), false));
  }
  return found;
}

// ── 预处理 ──

function blank(code, from, to) {
  return code.slice(0, from) + code.slice(from, to).replace(/[^\n]/g, " ") + code.slice(to);
}

/** 从 `from` 起的条目结尾：深度 0 的 `;`，或第一个深度 0 的 `{…}` 的配对 `}`。 */
function itemEnd(code, from) {
  let depth = 0;
  for (let i = from; i < code.length; i += 1) {
    const c = code[i];
    if (c === "(" || c === "[") depth += 1;
    else if (c === ")" || c === "]") depth -= 1;
    else if (c === ";" && depth === 0) return i + 1;
    else if (c === "{" && depth === 0) {
      let braces = 0;
      for (; i < code.length; i += 1) {
        if (code[i] === "{") braces += 1;
        else if (code[i] === "}" && --braces === 0) return i + 1;
      }
    }
  }
  return code.length;
}

/** 去掉测试条目、属性与常量条目之后的代码（行号不变）。`code` 已经过 stripCode。 */
function prepare(code) {
  for (const match of [...code.matchAll(CFG)].reverse()) {
    if (isTestCfg(match[1]))
      code = blank(code, match.index, itemEnd(code, match.index + match[0].length));
  }
  code = code.replace(/#!?\[(?:[^[\]]|\[(?:[^[\]]|\[[^[\]]*\])*\])*\]/g, (s) =>
    s.replace(/[^\n]/g, " "),
  );
  // `const NAME: T = …;` 与 `static NAME: T = …;`（含关联常量）；泛型的 `<const N: usize>` 前面是 `<` 或 `,`，不算。
  const definitions = /(?<![<,]\s*)\b(?:const|static)\s+(?:mut\s+)?[A-Z_][A-Z0-9_]*\s*:/g;
  for (const match of [...code.matchAll(definitions)].reverse()) {
    code = blank(code, match.index, itemEnd(code, match.index + match[0].length));
  }
  return code;
}

// ── 数字与例外 ──

const SUFFIX = "(?:_?(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64))?";
const NUMBER = new RegExp(
  `(?:0x[0-9a-fA-F][0-9a-fA-F_]*|0o[0-7][0-7_]*|0b[01][01_]*|\\d[\\d_]*(?:\\.\\d[\\d_]*)?(?:[eE][+-]?\\d[\\d_]*)?)${SUFFIX}`,
  "g",
);

function valueOf(text) {
  const digits = text.replace(new RegExp(`${SUFFIX}$`), "").replaceAll("_", "");
  if (/^0x/i.test(digits)) return parseInt(digits.slice(2), 16);
  if (digits.startsWith("0o")) return parseInt(digits.slice(2), 8);
  if (digits.startsWith("0b")) return parseInt(digits.slice(2), 2);
  return Number(digits);
}

/** 下标与切片里的一位数：`[N]`、`[N..]`、`[..N]`、`[a..N]`、`[i + N]`、`[i - N]`，`.get(N)`、`.nth(N)`。 */
function isSmallIndex(before, after) {
  if (
    /\[\s*([\w.]+\s*[+-]\s*)?(\.\.=?)?\s*$/.test(before) &&
    /^(\s*[+-]\s*[\w.]+)?\s*(\]|\.\.)/.test(after)
  )
    return true;
  if (/\[[^[\]]*\.\.=?\s*$/.test(before) && /^(\s*[+-]\s*[\w.]+)?\s*\]/.test(after)) return true;
  return /\.(get|nth)(::<.*>)?\(\s*$/.test(before) && /^\s*\)/.test(after);
}

/** 不算的写法（constants.md §4.2 的 6 条）与 NUMBER_ALLOWED 的全局模式。 */
function isExempt(value, before, after, allowed) {
  if (value === 0 || value === 1) return true;
  if (/(^|[^.])\.$/.test(before)) return true; // 元组字段 `.0`
  if (/;\s*$/.test(before) && /^\s*\]/.test(after)) return true; // 数组长度
  if (/(<<|>>)=?\s*$/.test(before) || /\.rotate_(left|right)\(\s*$/.test(before)) return true; // 位移量
  if (
    value === 2 &&
    (/(?<![*/%])[*/%]=?\s*$/.test(before) ||
      /^\s*[*/%]/.test(after) ||
      /\.(is_multiple_of|pow|powi)\(\s*$/.test(before))
  )
    return true; // 折半、加倍、奇偶、平方
  if (Number.isInteger(value) && value <= 9 && isSmallIndex(before, after)) return true;
  return allowed.some(
    (entry) =>
      entry.value === value &&
      (!entry.before || entry.before.test(before)) &&
      (!entry.after || entry.after.test(after)),
  );
}

/** 一段源码里被算的数字：`{ line, text, value }`。 */
export function hitsIn(text, allowed = []) {
  const hits = [];
  prepare(stripCode(text))
    .split("\n")
    .forEach((line, index) => {
      for (const match of line.matchAll(NUMBER)) {
        const start = match.index;
        const end = start + match[0].length;
        if (/[A-Za-z0-9_]/.test(line[start - 1] ?? "") || /[A-Za-z0-9_]/.test(line[end] ?? ""))
          continue;
        const value = valueOf(match[0]);
        if (isExempt(value, line.slice(0, start), line.slice(end), allowed)) continue;
        hits.push({ line: index + 1, text: match[0], value });
      }
    });
  return hits;
}

/** 每个扫描的文件（含 0 处的）→ 被算的数字。 */
export function countNumbers(sources, allowed = []) {
  const tests = testModuleFiles(sources);
  const out = new Map();
  for (const { file, text } of [...sources].sort((a, b) => (a.file < b.file ? -1 : 1))) {
    if (isScanned(file, tests)) out.set(file, hitsIn(text, allowed));
  }
  return out;
}

// ── 基线与判定 ──

const sum = (files) => Object.values(files).reduce((a, b) => a + b, 0);
const isSorted = (keys) => keys.every((key, i) => i === 0 || keys[i - 1] < key);

/** 现在的处数与基线比：多了、少了、过期项、0 项、total 与键序。 */
export function checkNumbers(counts, baseline) {
  const failures = [];
  const files = baseline?.files ?? {};
  const keys = Object.keys(files);
  if (!isSorted(keys)) failures.push(`numbers.json: files are not sorted; run ${UPDATE}`);
  if (baseline && baseline.total !== sum(files))
    failures.push(
      `numbers.json: total ${baseline.total} != ${sum(files)} (sum of files); run ${UPDATE}`,
    );
  for (const file of keys) {
    if (!counts.has(file))
      failures.push(`${file}: stale baseline entry (file not scanned); run ${UPDATE}`);
    else if (files[file] === 0)
      failures.push(`${file}: baseline 0 must not be listed; run ${UPDATE}`);
  }
  for (const [file, hits] of counts) {
    const limit = files[file] ?? 0;
    if (hits.length > limit) {
      failures.push(
        [
          `${file}: ${hits.length} numeric literals, baseline ${limit}. Name them as constants (docs/specs/rust-cli/constants.md §2):`,
          ...hits.map((hit) => `  ${file}:${hit.line}: ${hit.text}`),
        ].join("\n"),
      );
    } else if (hits.length < limit) {
      failures.push(
        `${file}: ${hits.length} numeric literals, baseline ${limit}; lower the baseline in the same commit: ${UPDATE}`,
      );
    }
  }
  return failures.sort();
}

/** `--update`：已有的文件取较小值（登记的升高把上限换成 `now`），删掉过期项与 0，新文件不写；没有基线时写入现状。 */
export function updateBaseline(counts, baseline, raises = []) {
  const files = {};
  for (const [file, hits] of counts) {
    let limit = baseline ? baseline.files[file] : hits.length;
    const raise = raises.find((entry) => entry.file === file);
    if (baseline && raise && (limit ?? 0) === raise.was) limit = Math.max(limit ?? 0, raise.now);
    if (limit === undefined) continue;
    const next = Math.min(hits.length, limit);
    if (next > 0) files[file] = next;
  }
  const sorted = Object.fromEntries(
    Object.keys(files)
      .sort()
      .map((key) => [key, files[key]]),
  );
  return { total: sum(sorted), files: sorted };
}

export function formatBaseline(baseline) {
  return `${JSON.stringify(baseline, null, 2)}\n`;
}

/** 推送前：HEAD 的基线与推送基线的比（不许升高，除非 NUMBER_RAISES 逐项登记）。 */
export function checkRaises(before, after, raises) {
  const failures = [];
  for (const [file, now] of Object.entries(after.files)) {
    const was = before.files[file] ?? 0;
    if (now <= was) continue;
    const raise = raises.find((entry) => entry.file === file);
    if (!raise)
      failures.push(
        `${file}: ${was} -> ${now} is not registered in NUMBER_RAISES (scripts/rust-cli/boundaries/policy.mjs)`,
      );
    else if (raise.was !== was)
      failures.push(`${file}: ${was} -> ${now}: registered was ${raise.was}, push base has ${was}`);
    else if (raise.now < now)
      failures.push(`${file}: ${was} -> ${now}: registered now ${raise.now}`);
  }
  return failures;
}

// ── 命令行 ──

const root = resolve(import.meta.dirname, "../../../packages/rust-cli");

async function* walk(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name !== "target") yield* walk(path);
    } else if (path.endsWith(".rs")) yield path;
  }
}

/** packages/rust-cli/crates 下全部 .rs：`{ file, text }`，`file` 相对 packages/rust-cli。 */
export async function readSources() {
  const sources = [];
  for await (const path of walk(resolve(root, "crates"))) {
    sources.push({
      file: relative(root, path).split(sep).join("/"),
      text: await readFile(path, "utf8"),
    });
  }
  return sources;
}

export async function readBaseline(path = BASELINE) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}

function report(counts, target) {
  const lines = [];
  if (target?.endsWith(".rs")) {
    for (const hit of counts.get(target) ?? []) lines.push(`${target}:${hit.line}: ${hit.text}`);
    return lines;
  }
  const groups = new Map();
  for (const [file, hits] of counts) {
    const crate = file.split("/")[1];
    if (target && crate !== target) continue;
    const key = target ? file : crate;
    groups.set(key, (groups.get(key) ?? 0) + hits.length);
  }
  const rows = [...groups]
    .filter(([, n]) => n > 0)
    .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1));
  for (const [key, n] of rows) lines.push(`${String(n).padStart(6)}  ${key}`);
  lines.push(`${String(rows.reduce((a, [, n]) => a + n, 0)).padStart(6)}  total`);
  return lines;
}

async function main(argv) {
  const policy = await import("./policy.mjs");
  const counts = countNumbers(await readSources(), policy.NUMBER_ALLOWED);
  const at = argv.indexOf("--report");
  if (at !== -1) {
    console.log(report(counts, argv[at + 1]).join("\n"));
    return 0;
  }
  if (argv.includes("--update")) {
    const before = await readBaseline();
    const next = updateBaseline(counts, before, policy.NUMBER_RAISES);
    await writeFile(BASELINE, formatBaseline(next));
    console.log(`numbers.json: total ${before?.total ?? "(none)"} -> ${next.total}`);
    return 0;
  }
  console.error("usage: numbers.mjs --update | --report [crate | crates/<crate>/src/<file>.rs]");
  return 2;
}

if (process.argv[1] && import.meta.url === pathToFileURL(await realpath(process.argv[1])).href)
  process.exitCode = await main(process.argv.slice(2));
