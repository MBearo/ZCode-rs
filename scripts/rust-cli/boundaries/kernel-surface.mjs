// kernel-api 只加不改（docs/specs/rust-cli/app-protocol.md §9.4，README §3「kernel-api 按只加不改演进」）：
// 从 kernel-api 的源码提取公开的项（trait 的方法、结构体的公开字段、枚举的变体、函数、常量、类型别名、impl、重新导出、宏），
// 与已提交的快照 kernel-surface.json 比较。
//
// - 新增的项：允许，但要在同一个提交里记进快照（`--update`，只追加），审查时一眼可见。
// - 已有的项改了或删了：失败，除非逐项登记在 policy.mjs 的 KERNEL_API_CHANGES（`was` = 快照里的值，`now` = 现在的值，
//   删除写 null，`reason` 写用户的决定）。`--update` 不改已有的项，所以改动绕不过登记。
// - `--rebaseline`：整份重写（只给用户决定的集中整理，例如 R1-6 / 7 / 8），之后 KERNEL_API_CHANGES 要清空。
//
// 纯函数部分不读文件；check-boundaries.mjs 收集源码后调用 `checkSurface`。只认本工程的写法（不用 nightly 的 rustdoc JSON）。
import { createHash } from "node:crypto";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { stripComments } from "./interfaces.mjs";

/** kernel-api 的源码目录（相对 packages/rust-cli）。 */
export const SURFACE_DIR = "crates/kernel-api/src/";

/** 不算接口的文件：单测、`testing` feature 的替身。 */
export function isSurfaceFile(file) {
  if (!file.startsWith(SURFACE_DIR) || !file.endsWith(".rs")) return false;
  const rest = file.slice(SURFACE_DIR.length);
  return !(
    /(^|\/)[^/]*_tests(_\w+)?\.rs$/.test(rest) ||
    /(^|\/)test_support\.rs$/.test(rest) ||
    rest === "testing.rs" ||
    rest.startsWith("testing/")
  );
}

const squash = (text) =>
  text
    .replace(/\s+/g, " ")
    .replace(/\s*([(),:;<>&[\]{}])\s*/g, "$1")
    .replace(/->/g, " -> ")
    .trim();

/** 跳过 `i` 处的字符串或字符字面量，返回之后的位置；不是字面量时返回 null。 */
function skipLiteral(code, i) {
  const c = code[i];
  const prev = code[i - 1] ?? "";
  if ((c === "r" || (c === "b" && code[i + 1] === "r")) && !/[A-Za-z0-9_]/.test(prev)) {
    const head = code.slice(i, i + 260).match(/^b?r(#*)"/);
    if (head) {
      const close = `"${head[1]}`;
      const end = code.indexOf(close, i + head[0].length);
      return end === -1 ? code.length : end + close.length;
    }
  }
  if (c === '"' || (c === "b" && code[i + 1] === '"' && !/[A-Za-z0-9_]/.test(prev))) {
    let j = i + (c === "b" ? 2 : 1);
    while (j < code.length && code[j] !== '"') j += code[j] === "\\" ? 2 : 1;
    return j + 1;
  }
  if (c === "'") {
    const literal = code
      .slice(i)
      .match(/^'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'/);
    if (literal) return i + literal[0].length;
  }
  return null;
}

/** 顶层的项：到深度 0 的 `;`，或回到深度 0 的 `}`（紧跟的 `;` 算进去）。字面量里的括号不算。 */
export function splitItems(code) {
  const items = [];
  let depth = 0;
  let start = 0;
  let i = 0;
  while (i < code.length) {
    const skipped = skipLiteral(code, i);
    if (skipped !== null) {
      i = skipped;
      continue;
    }
    const c = code[i];
    if (c === "{" || c === "(" || c === "[") depth += 1;
    else if (c === ")" || c === "]") depth -= 1;
    else if (c === "}") {
      depth -= 1;
      if (depth === 0) {
        let end = i + 1;
        const after = /^\s*;/.exec(code.slice(end));
        if (after) end += after[0].length;
        items.push(code.slice(start, end));
        start = end;
        i = end;
        continue;
      }
    } else if (c === ";" && depth === 0) {
      items.push(code.slice(start, i + 1));
      start = i + 1;
    }
    i += 1;
  }
  if (code.slice(start).trim()) items.push(code.slice(start));
  return items.map((item) => item.trim()).filter(Boolean);
}

/** 项前面的属性与本体分开。 */
function attributesOf(item) {
  const attributes = [];
  let rest = item.trim();
  while (/^#!?\s*\[/.test(rest)) {
    let depth = 0;
    let i = 0;
    for (; i < rest.length; i += 1) {
      const skipped = skipLiteral(rest, i);
      if (skipped !== null) {
        i = skipped - 1;
        continue;
      }
      if (rest[i] === "[") depth += 1;
      else if (rest[i] === "]" && --depth === 0) break;
    }
    attributes.push(rest.slice(0, i + 1));
    rest = rest.slice(i + 1).trim();
  }
  return { attributes, rest };
}

/** 只在测试或替身里编译的项。 */
const isTestOnly = (attributes) =>
  attributes.some(
    (attribute) =>
      /\bcfg\s*\(\s*(?:any\s*\(|all\s*\()?\s*test\b/.test(attribute) ||
      /\bfeature\s*=\s*"testing"/.test(attribute),
  );

/** 留在值里的属性：影响形状或序列化的（derive、serde、repr、non_exhaustive、default）。 */
const shapeAttributes = (attributes) =>
  attributes
    .filter((attribute) =>
      /^#\s*\[\s*(?:derive|serde|repr|non_exhaustive|default)\b/.test(attribute),
    )
    .map((attribute) => squash(attribute.replace(/^#\s*\[/, "").replace(/\]$/, "")))
    .map((attribute) => `#[${attribute}]`);

/** 顶层逗号切分（不切 () <> [] {} 里面的；`->` 的 `>` 不算括号）。 */
function splitTop(text) {
  const parts = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; i += 1) {
    const skipped = skipLiteral(text, i);
    if (skipped !== null) {
      i = skipped - 1;
      continue;
    }
    const c = text[i];
    if ("([{<".includes(c)) depth += 1;
    else if (")]}>".includes(c) && !(c === ">" && text[i - 1] === "-")) depth -= 1;
    else if (c === "," && depth === 0) {
      parts.push(text.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(text.slice(start));
  return parts.map((part) => part.trim()).filter(Boolean);
}

/** 第一个深度 0 的 `{`（尖括号、圆括号与方括号里的不算）。 */
function bodyStart(text) {
  let depth = 0;
  for (let i = 0; i < text.length; i += 1) {
    const skipped = skipLiteral(text, i);
    if (skipped !== null) {
      i = skipped - 1;
      continue;
    }
    const c = text[i];
    if ("([<".includes(c)) depth += 1;
    else if (")]>".includes(c) && !(c === ">" && text[i - 1] === "-")) depth -= 1;
    else if (c === "{" && depth === 0) return i;
  }
  return -1;
}

/** `{ … }` 的内容（`open` 是 `{` 的位置，到文字末尾的 `}`）。 */
const inner = (text, open) => text.slice(open + 1, text.lastIndexOf("}"));

/** 展开 `a::{b, c::{d, e}}` 这样的重新导出。 */
function expandUse(path) {
  const brace = path.indexOf("{");
  if (brace === -1) return [squash(path)];
  const prefix = path.slice(0, brace);
  const body = path.slice(brace + 1, path.lastIndexOf("}"));
  return splitTop(body).flatMap((part) => expandUse(`${prefix}${part}`));
}

const hash = (text) => createHash("sha256").update(text).digest("hex").slice(0, 16);

const PUBLIC = /^pub\s+(?!\()/;
const NAME = /[A-Za-z_][A-Za-z0-9_]*/;
const LONG_MACRO = 120;

/** 一个文件（模块名 `module`）的公开项：键 → 值。 */
export function extractSurface(source, module) {
  const items = {};
  collect(splitItems(stripComments(source)), module, items);
  return items;
}

function collect(list, module, items) {
  const put = (key, value) => {
    let name = `${module}::${key}`;
    for (let n = 2; name in items; n += 1) name = `${module}::${key}#${n}`;
    items[name] = value;
  };
  for (const raw of list) {
    const { attributes, rest } = attributesOf(raw);
    if (!rest || isTestOnly(attributes)) continue;
    const shape = shapeAttributes(attributes);
    const prefix = shape.length ? `${shape.join(" ")} ` : "";
    let match;
    if ((match = /^(?:unsafe\s+)?impl\b/.exec(rest))) {
      const open = bodyStart(rest);
      if (open === -1) continue;
      const header = squash(rest.slice(0, open));
      const target = header.replace(/^(?:unsafe )?impl(?:<.*?>(?=[ A-Za-z&(]))?\s*/, "");
      if (/(?:^|[ >)])for /.test(` ${target}`) && !target.startsWith("for<")) {
        put(header, "");
        continue;
      }
      const type = NAME.exec(target.replace(/^&('?\w+ )?/, ""))?.[0] ?? target;
      for (const member of splitItems(inner(rest, open))) {
        const { attributes: memberAttributes, rest: body } = attributesOf(member);
        if (isTestOnly(memberAttributes) || !PUBLIC.test(body)) continue;
        const fn = /^pub\s+(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_]\w*)/.exec(body);
        const constant = /^pub\s+const\s+([A-Za-z_]\w*)\s*:/.exec(body);
        if (fn) {
          const start = bodyStart(body);
          put(
            `${type}::${fn[1]}`,
            squash((start === -1 ? body : body.slice(0, start)).replace(PUBLIC, "")),
          );
        } else if (constant) put(`${type}::${constant[1]}`, squash(body.replace(PUBLIC, "")));
      }
    } else if ((match = /^macro_rules!\s*([A-Za-z_]\w*)/.exec(rest))) {
      put(`macro_rules! ${match[1]}`, hash(squash(rest)));
    } else if ((match = /^([a-z_][a-z0-9_]*)!\s*[([{]/.exec(rest))) {
      const args = squash(rest.slice(match[0].length - 1).replace(/;$/, ""));
      const body = args.slice(1, -1);
      // 短的调用（`string_id!(SessionId)`）整个是一项；长的（`hook_events!(PreToolUse, …)`）按顶层逗号逐个成项，加一个
      // 参数是新增，不是改动。
      if (body.length <= LONG_MACRO) put(`${match[1]}!(${body})`, "");
      else for (const part of splitTop(body)) put(`${match[1]}!(…)::${squash(part)}`, "");
    } else if (!PUBLIC.test(rest)) {
      continue;
    } else if ((match = /^pub\s+use\s+([\s\S]+);$/.exec(rest))) {
      for (const path of expandUse(match[1])) put(`use ${path}`, "");
    } else if ((match = /^pub\s+mod\s+([A-Za-z_]\w*)\s*\{/.exec(rest))) {
      collect(splitItems(inner(rest, rest.indexOf("{"))), `${module}::${match[1]}`, items);
    } else if ((match = /^pub\s+struct\s+([A-Za-z_]\w*)/.exec(rest))) {
      const name = match[1];
      const after = rest.slice(match[0].length);
      const open = bodyStart(after);
      const tuple = after.search(/[(;]/);
      if (open !== -1 && (tuple === -1 || open < tuple)) {
        put(name, `${prefix}${squash(after.slice(0, open))}`.trim());
        for (const field of splitTop(inner(after, open))) {
          const { attributes: fieldAttributes, rest: body } = attributesOf(field);
          if (!PUBLIC.test(body)) continue;
          const fieldShape = shapeAttributes(fieldAttributes);
          const text = squash(body.replace(PUBLIC, ""));
          const fieldName = NAME.exec(text)?.[0] ?? text;
          put(
            `${name}.${fieldName}`,
            `${fieldShape.length ? `${fieldShape.join(" ")} ` : ""}${text}`,
          );
        }
      } else put(name, `${prefix}${squash(after.replace(/;$/, ""))}`.trim());
    } else if ((match = /^pub\s+enum\s+([A-Za-z_]\w*)/.exec(rest))) {
      const name = match[1];
      const after = rest.slice(match[0].length);
      const open = bodyStart(after);
      put(name, `${prefix}${squash(after.slice(0, open))}`.trim());
      for (const variant of splitTop(inner(after, open))) {
        const { attributes: variantAttributes, rest: body } = attributesOf(variant);
        const variantShape = shapeAttributes(variantAttributes);
        const text = squash(body);
        put(
          `${name}::${NAME.exec(text)?.[0] ?? text}`,
          `${variantShape.length ? `${variantShape.join(" ")} ` : ""}${text}`,
        );
      }
    } else if ((match = /^pub\s+(?:unsafe\s+)?trait\s+([A-Za-z_]\w*)/.exec(rest))) {
      const name = match[1];
      const after = rest.slice(match[0].length);
      const open = bodyStart(after);
      put(name, squash(after.slice(0, open)));
      for (const member of splitItems(inner(after, open))) {
        const { attributes: memberAttributes, rest: body } = attributesOf(member);
        if (isTestOnly(memberAttributes)) continue;
        const fn = /^(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_]\w*)/.exec(body);
        const associated = /^(type|const)\s+([A-Za-z_]\w*)/.exec(body);
        if (fn) {
          const start = bodyStart(body);
          put(
            `${name}::${fn[1]}`,
            start === -1
              ? squash(body.replace(/;$/, ""))
              : `${squash(body.slice(0, start))} {default}`,
          );
        } else if (associated) put(`${name}::${associated[1]} ${associated[2]}`, squash(body));
      }
    } else if (
      (match =
        /^pub\s+(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s+([A-Za-z_]\w*)/.exec(
          rest,
        ))
    ) {
      const start = bodyStart(rest);
      put(match[1], squash((start === -1 ? rest : rest.slice(0, start)).replace(PUBLIC, "")));
    } else if ((match = /^pub\s+(?:const|static(?:\s+mut)?|type)\s+([A-Za-z_]\w*)/.exec(rest))) {
      put(match[1], squash(rest.slice(match[0].length).replace(/;$/, "")));
    }
  }
}

/** 模块名：`lib.rs` 是 `lib`，`a/b.rs` 是 `a::b`，`a/mod.rs` 是 `a`。 */
export function moduleOf(file) {
  const rest = file
    .slice(SURFACE_DIR.length)
    .replace(/\.rs$/, "")
    .replace(/\/mod$/, "");
  return rest.split("/").join("::");
}

/** 源码（相对 packages/rust-cli 的路径 → 文字）的公开项，键按字母序。 */
export function surfaceOf(sources) {
  const items = {};
  for (const [file, text] of Object.entries(sources).sort(([a], [b]) => a.localeCompare(b))) {
    if (!isSurfaceFile(file)) continue;
    Object.assign(items, extractSurface(text, moduleOf(file)));
  }
  return Object.fromEntries(Object.entries(items).sort(([a], [b]) => a.localeCompare(b)));
}

/** 与快照比较：已有的项改了或删了要登记；新增的要记进快照。 */
export function checkSurface(current, committed, changes) {
  const failures = [];
  const used = new Set();
  const doc = "app-protocol.md §9.4";
  for (const [key, was] of Object.entries(committed)) {
    const now = key in current ? current[key] : null;
    if (now === was) continue;
    const entry = changes.find((change) => change.key === key);
    if (entry && entry.was === was && entry.now === now) {
      used.add(entry);
      if (!entry.reason?.trim()) {
        failures.push(`KERNEL_API_CHANGES.${key}: write the reason (the user's decision)`);
      }
      continue;
    }
    failures.push(
      now === null
        ? `kernel-api only grows: ${key} was removed (was \`${was}\`); keep it, or register the removal in KERNEL_API_CHANGES with the user's decision (${doc})`
        : `kernel-api only grows: ${key} changed (was \`${was}\`, now \`${now}\`); keep the old item and add a new one, or register the change in KERNEL_API_CHANGES with the user's decision (${doc})`,
    );
  }
  for (const key of Object.keys(current)) {
    if (!(key in committed)) {
      failures.push(
        `kernel-api: ${key} is new; additions are allowed, record it in the same commit (node scripts/rust-cli/boundaries/kernel-surface.mjs --update)`,
      );
    }
  }
  for (const entry of changes) {
    if (!used.has(entry)) {
      failures.push(
        `KERNEL_API_CHANGES.${entry.key}: was / now do not match the snapshot and the source (dead entry)`,
      );
    }
  }
  return failures;
}

/** `--update`：只把新增的项追加进快照，已有的项不动。 */
export function appendAdditions(current, committed) {
  const merged = { ...committed };
  for (const [key, value] of Object.entries(current)) if (!(key in merged)) merged[key] = value;
  return Object.fromEntries(Object.entries(merged).sort(([a], [b]) => a.localeCompare(b)));
}

const ROOT = resolve(import.meta.dirname, "../../../packages/rust-cli");
/** 已提交的快照。 */
export const SURFACE_SNAPSHOT = resolve(import.meta.dirname, "kernel-surface.json");

async function* rustFiles(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) yield* rustFiles(path);
    else if (entry.name.endsWith(".rs")) yield path;
  }
}

/** kernel-api 的源码（相对 packages/rust-cli）。 */
export async function readSurfaceSources(root = ROOT) {
  const sources = {};
  for await (const path of rustFiles(resolve(root, SURFACE_DIR))) {
    sources[relative(root, path).replaceAll("\\", "/")] = await readFile(path, "utf8");
  }
  return sources;
}

export async function readSnapshot(path = SURFACE_SNAPSHOT) {
  return JSON.parse(await readFile(path, "utf8")).items;
}

const format = (items) => `${JSON.stringify({ items }, null, 2)}\n`;

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const current = surfaceOf(await readSurfaceSources());
  if (process.argv.includes("--rebaseline")) {
    await writeFile(SURFACE_SNAPSHOT, format(current));
    console.log(
      `rewrote ${relative(process.cwd(), SURFACE_SNAPSHOT)} (${Object.keys(current).length} items); empty KERNEL_API_CHANGES in the same commit`,
    );
  } else if (process.argv.includes("--update")) {
    const committed = await readSnapshot();
    const merged = appendAdditions(current, committed);
    const added = Object.keys(merged).length - Object.keys(committed).length;
    await writeFile(SURFACE_SNAPSHOT, format(merged));
    console.log(`appended ${added} new items to ${relative(process.cwd(), SURFACE_SNAPSHOT)}`);
  } else {
    const { KERNEL_API_CHANGES } = await import("./policy.mjs");
    const failures = checkSurface(current, await readSnapshot(), KERNEL_API_CHANGES);
    for (const failure of failures) console.error(failure);
    if (failures.length) process.exitCode = 1;
    else console.log(`rust-cli kernel-api surface: OK (${Object.keys(current).length} items)`);
  }
}
