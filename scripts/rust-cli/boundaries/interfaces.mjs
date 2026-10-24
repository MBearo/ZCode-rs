// 接口快照（docs/specs/rust-cli/remote-exec.md §15.6、§18.7）：端口 trait 的方法签名与 exec-wire 的线上结构从源码提取，
// 与 `interfaces.json` 比较。改了端口或线上格式就必须同时改快照（`--update` 重写），审查时一眼可见。
//
// 纯函数（这里不读文件）：check-boundaries.mjs 收集源码后调用 `checkInterfaces`；`node interfaces.mjs --update` 重写快照。
// 不用 nightly 的 rustdoc JSON：只认本工程的写法（trait 的方法以 `fn` 开头、以 `;` 或方法体结束；结构体与枚举的字段 /
// 变体以顶层逗号分隔）。
import { readdir, readFile, writeFile } from "node:fs/promises";
import { relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

/** 快照覆盖的 trait：文件 → trait 名。 */
export const PORT_TRAITS = {
  "crates/kernel-api/src/machine.rs": [
    "EnvInfo",
    "Files",
    "Search",
    "Commands",
    "Shell",
    "Background",
    "Checkpoints",
    "Pdf",
  ],
  "crates/kernel-api/src/mcp.rs": ["McpTransport"],
  "crates/kernel-api/src/local_services.rs": ["Git", "Fetch"],
  "crates/kernel-api/src/env_registry.rs": ["EnvironmentRegistry"],
  "crates/exec-transport/src/link.rs": [
    "FrameSink",
    "FrameSource",
    "Link",
    "Listener",
    "Connector",
    "Provisioner",
  ],
  "crates/exec-server/src/ledger.rs": ["ExecLedger"],
  "crates/exec-server/src/auth.rs": ["Authenticator"],
};

/** 线上结构所在的目录（不含测试文件）。 */
export const WIRE_DIR = "crates/exec-wire/src/";

/** 去掉注释（保留字符串与字符字面量：serde 的 `rename = "…"` 要留下）。 */
export function stripComments(source) {
  let out = "";
  let i = 0;
  while (i < source.length) {
    const c = source[i];
    const next = source[i + 1];
    if (c === "/" && next === "/") {
      while (i < source.length && source[i] !== "\n") i += 1;
    } else if (c === "/" && next === "*") {
      let depth = 1;
      i += 2;
      while (i < source.length && depth > 0) {
        if (source[i] === "/" && source[i + 1] === "*") {
          depth += 1;
          i += 2;
        } else if (source[i] === "*" && source[i + 1] === "/") {
          depth -= 1;
          i += 2;
        } else i += 1;
      }
    } else if (c === '"') {
      let j = i + 1;
      while (j < source.length && source[j] !== '"') j += source[j] === "\\" ? 2 : 1;
      out += source.slice(i, j + 1);
      i = j + 1;
    } else if (c === "'" && /^'(\\.|[^\\'])'/.test(source.slice(i, i + 4))) {
      const literal = source.slice(i).match(/^'(\\.|[^\\'])'/)[0];
      out += literal;
      i += literal.length;
    } else {
      out += c;
      i += 1;
    }
  }
  return out;
}

const squash = (text) =>
  text
    .replace(/\s+/g, " ")
    .replace(/\s*([(),:;<>&[\]{}])\s*/g, "$1")
    .replace(/->/g, " -> ")
    .trim();

/** 从 `open`（一个 `{` 的位置）找到配对的 `}`。 */
function closing(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i += 1) {
    if (text[i] === "{") depth += 1;
    else if (text[i] === "}" && --depth === 0) return i;
  }
  return text.length;
}

/** 顶层逗号切分（不切 () <> [] {} 里面的）。 */
function splitTop(text) {
  const parts = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; i += 1) {
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

/** 一个 trait 的方法：`{ name, signature, default }`；签名去掉空白差异与属性。 */
export function extractTrait(source, name) {
  const code = stripComments(source);
  const head = new RegExp(`pub trait ${name}\\b([^{]*)\\{`).exec(code);
  if (!head) return null;
  const open = head.index + head[0].length - 1;
  const body = code.slice(open + 1, closing(code, open));
  const methods = [];
  let i = 0;
  while (i < body.length) {
    const found = /(?:async\s+)?fn\s+([A-Za-z0-9_]+)/.exec(body.slice(i));
    if (!found) break;
    const start = i + found.index;
    let depth = 0;
    let end = start;
    for (; end < body.length; end += 1) {
      const c = body[end];
      if (c === "(" || c === "<" || c === "[") depth += 1;
      else if ((c === ")" || c === "]" || (c === ">" && body[end - 1] !== "-")) && depth > 0)
        depth -= 1;
      else if ((c === ";" || c === "{") && depth === 0) break;
    }
    const isDefault = body[end] === "{";
    methods.push({ name: found[1], signature: squash(body.slice(start, end)), default: isDefault });
    i = isDefault ? closing(body, end) + 1 : end + 1;
  }
  return { bounds: squash(head[1]), methods };
}

/** 项上的属性（只留 serde 的）。 */
function serdeAttributes(text) {
  return [...text.matchAll(/#\[serde\(([^\]]*)\)\]/g)].map((match) => squash(match[1]));
}

/** 一个字段或变体：前面的属性（只留 serde 的）与本体分开；变体里字段的属性留在本体里。 */
function member(text) {
  const leading = /^(?:#\[[^\]]*\]\s*)*/.exec(text)[0];
  const attributes = serdeAttributes(leading);
  const bare = squash(text.slice(leading.length)).replace(/^pub /, "");
  return attributes.length ? { attributes, item: bare } : { item: bare };
}

/** exec-wire 的一个文件里全部公开的结构体、枚举与常量。 */
export function extractWire(source) {
  const code = stripComments(source);
  const items = {};
  const pattern = /((?:#\[[^\]]*\]\s*)*)pub (struct|enum|const) ([A-Za-z0-9_]+)/g;
  for (const match of code.matchAll(pattern)) {
    const [, attrs, kind, name] = match;
    const after = match.index + match[0].length;
    if (kind === "const") {
      const end = code.indexOf(";", after);
      items[name] = { kind, value: squash(code.slice(after, end)) };
      continue;
    }
    const rest = code.slice(after);
    const brace = rest.search(/[{;(]/);
    const generics = squash(rest.slice(0, brace));
    const entry = { kind, ...(generics ? { generics } : {}) };
    const serde = serdeAttributes(attrs);
    if (serde.length) entry.attributes = serde;
    if (rest[brace] === "{") {
      const open = after + brace;
      const body = code.slice(open + 1, closing(code, open));
      entry[kind === "enum" ? "variants" : "fields"] = splitTop(body).map(member);
    } else if (rest[brace] === "(") {
      const end = rest.indexOf(";", brace);
      entry.tuple = squash(rest.slice(brace, end));
    }
    items[name] = entry;
  }
  return items;
}

/** 帧类型的编号（`Kind::code` 的 match）。 */
export function extractFrameCodes(source) {
  const code = stripComments(source);
  const body = /fn code\(self\) -> u8 \{([\s\S]*?)\n {4}\}/.exec(code);
  if (!body) return {};
  return Object.fromEntries(
    [...body[1].matchAll(/Self::([A-Za-z]+) => (0x[0-9a-fA-F]+)/g)].map((match) => [
      match[1],
      match[2],
    ]),
  );
}

/** 由源码（相对 packages/rust-cli 的路径 → 文字）得到快照。 */
export function snapshot(sources) {
  const ports = {};
  for (const [file, names] of Object.entries(PORT_TRAITS)) {
    for (const name of names) {
      const text = sources[file];
      ports[name] = text === undefined ? null : extractTrait(text, name);
    }
  }
  const wire = {};
  for (const [file, text] of Object.entries(sources).sort(([a], [b]) => a.localeCompare(b))) {
    if (!file.startsWith(WIRE_DIR) || /(_tests|test_support)\.rs$/.test(file)) continue;
    const items = extractWire(text);
    if (Object.keys(items).length) wire[file.slice(WIRE_DIR.length)] = items;
  }
  const frames = extractFrameCodes(sources[`${WIRE_DIR}frame.rs`] ?? "");
  return { ports, wire, frames };
}

/** 快照与已提交的比较：不同就报出哪一项变了。 */
export function checkInterfaces(sources, committed) {
  const current = snapshot(sources);
  const failures = [];
  for (const section of ["ports", "wire", "frames"]) {
    const now = current[section];
    const before = committed?.[section] ?? {};
    for (const key of new Set([...Object.keys(now), ...Object.keys(before)])) {
      if (now[key] === null)
        failures.push(`interfaces: ${section}.${key} is not found in the source`);
      else if (JSON.stringify(now[key]) !== JSON.stringify(before[key]))
        failures.push(
          `interfaces: ${section}.${key} changed; update the snapshot in the same commit (node scripts/rust-cli/boundaries/interfaces.mjs --update)`,
        );
    }
  }
  return failures;
}

const ROOT = resolve(import.meta.dirname, "../../../packages/rust-cli");
/** 已提交的快照。 */
export const SNAPSHOT = resolve(import.meta.dirname, "interfaces.json");

async function* rustFiles(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) yield* rustFiles(path);
    else if (entry.name.endsWith(".rs")) yield path;
  }
}

/** 快照要的源码：端口 trait 所在的文件与 exec-wire 的全部文件（相对 packages/rust-cli）。 */
export async function readSources(root = ROOT) {
  const sources = {};
  for (const file of Object.keys(PORT_TRAITS)) {
    try {
      sources[file] = await readFile(resolve(root, file), "utf8");
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  try {
    for await (const path of rustFiles(resolve(root, WIRE_DIR))) {
      sources[relative(root, path).replaceAll("\\", "/")] = await readFile(path, "utf8");
    }
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  return sources;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const sources = await readSources();
  if (process.argv.includes("--update")) {
    await writeFile(SNAPSHOT, `${JSON.stringify(snapshot(sources), null, 2)}\n`);
    console.log(`wrote ${relative(process.cwd(), SNAPSHOT)}`);
  } else {
    const failures = checkInterfaces(sources, JSON.parse(await readFile(SNAPSHOT, "utf8")));
    for (const failure of failures) console.error(failure);
    if (failures.length) process.exitCode = 1;
    else console.log("rust-cli interfaces: OK");
  }
}
