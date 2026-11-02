// 枢纽 DTO 的构造（工作线 F126，docs/specs/rust-cli/README.md §4.1 第 7 条）：kernel-api 以外的 crate 不用完整的结构体
// 字面量构造这些类型，用 kernel-api 给的构造函数（`T::new(..)`；测试另有 `testing::` 的 builder）；要改别的字段时用结构
// 更新 `T { 字段: 值, ..T::new(..) }`。这样 DTO 加字段时只改 kernel-api，不波及各 crate 的调用方与测试。
//
// 纯函数，这里不做 IO：check-boundaries.mjs 收集源码后调用；boundaries/constructors.test.mjs 每条规则一个合成的违规。
// 规则表在 policy-vocabulary.mjs 的 DTO_CONSTRUCTORS 与 DTO_CONSTRUCTOR_EXCEPTIONS：
// 1. 只看代码（注释与字符串去掉之后）；定义（struct / enum / impl / `->` 之后）与模式（`let`、`=>`、`=`、`..` 的剩余
//    模式）不算构造。
// 2. 带 `..` 的结构更新不算完整字面量（基底是构造函数或已有的值）。
// 3. 例外按文件登记，每项写类型与理由；登记了却没有命中的是死项。`skipCrates` 里的 crate 有同名的别的类型（执行协议的
//    镜像 DTO），整个 crate 不查这个类型。
// 4. 枚举变体没有结构更新：`defaults` 列出可以省掉的字段，只有它们都是缺省值的字面量才算（该用构造函数）。
import { stripCode } from "./check.mjs";

const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/// `open` 处的 `{` 对应的 `}`（code 已去掉注释与字面量）。
function closing(code, open) {
  let depth = 0;
  for (let i = open; i < code.length; i += 1) {
    if (code[i] === "{") depth += 1;
    else if (code[i] === "}") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/// 花括号里顶层的各项（逗号切分，括号与 turbofish `::<…>` 里的逗号不算）。
function items(body) {
  const parts = [];
  let depth = 0;
  let angle = 0;
  let last = 0;
  for (let i = 0; i < body.length; i += 1) {
    const c = body[i];
    if ("([{".includes(c)) depth += 1;
    else if (")]}".includes(c)) depth -= 1;
    else if (depth === 0 && body.startsWith("::<", i)) {
      angle += 1;
      i += 2;
    } else if (depth === 0 && angle > 0 && c === ">" && !"=-".includes(body[i - 1])) angle -= 1;
    else if (depth === 0 && angle === 0 && c === ",") {
      parts.push(body.slice(last, i));
      last = i + 1;
    }
  }
  if (body.slice(last).trim()) parts.push(body.slice(last));
  return parts.map((part) => part.trim());
}

const DEFINITION =
  /(?:\bstruct|\benum|\bimpl|\bfor|\blet|->\s*&?\s*(?:mut\s+)?|\bunion|\btrait|\bmod)\s*$/;

/// 字面量里 `name: value` 的各项（简写 `name` 的值是它自己）。
function fields(parts) {
  const out = {};
  for (const part of parts) {
    const field = part.match(/^([A-Za-z_][A-Za-z0-9_]*)\s*(?::(?!:)\s*([\s\S]*))?$/);
    if (field) out[field[1]] = (field[2] ?? field[1]).replace(/\s+/g, "");
  }
  return out;
}

/// `type`（可以带 `Enum::Variant`）在源码里的完整字面量：`[{ line }]`。`defaults`（字段 → 缺省值的写法）给出时，
/// 只算这些字段都是缺省值的字面量：枚举变体没有结构更新，带了别的值时构造函数写不出来。
export function fullLiterals(text, type, defaults) {
  const code = stripCode(text);
  const pattern = new RegExp(
    `(?<![A-Za-z0-9_:])(?:[A-Za-z_][A-Za-z0-9_]*::)*${escape(type)}\\s*\\{`,
    "g",
  );
  const found = [];
  for (const match of code.matchAll(pattern)) {
    if (DEFINITION.test(code.slice(Math.max(0, match.index - 40), match.index))) continue;
    const open = match.index + match[0].length - 1;
    const close = closing(code, open);
    if (close < 0) continue;
    if (/^\s*(?:=>|=(?!=)|:(?!:))/.test(code.slice(close + 1, close + 8))) continue;
    const parts = items(code.slice(open + 1, close));
    if (parts.some((item) => item.startsWith(".."))) continue;
    if (defaults) {
      // 字符串去掉了内容，比较用原文。
      const given = fields(items(text.slice(open + 1, close)));
      const plain = Object.entries(defaults).every(([name, values]) =>
        values.some((value) => given[name] === value.replace(/\s+/g, "")),
      );
      if (!plain) continue;
    }
    found.push({ line: code.slice(0, match.index).split("\n").length });
  }
  return found;
}

/// `sources`：`{ crate, file, text }`，路径相对 packages/rust-cli。返回违规清单。
export function checkConstructors(policy, sources) {
  const failures = [];
  const used = new Set();
  const { owner, types } = policy.DTO_CONSTRUCTORS;
  for (const { crate, file, text } of sources) {
    if (crate === owner) continue;
    for (const { name, hint, skipCrates = [], defaults } of types) {
      if (skipCrates.includes(crate) || !text.includes(name.split("::").at(-1))) continue;
      const found = fullLiterals(text, name, defaults);
      if (!found.length) continue;
      const exception = policy.DTO_CONSTRUCTOR_EXCEPTIONS.find(
        (entry) => entry.files.includes(file) && entry.types.includes(name),
      );
      if (exception) {
        used.add(`${name}|${file}`);
        continue;
      }
      for (const { line } of found) {
        failures.push(
          `${file}:${line}: a full ${name} literal outside ${owner}; use ${hint} (struct update for the other fields; README §4.1)`,
        );
      }
    }
  }
  for (const exception of policy.DTO_CONSTRUCTOR_EXCEPTIONS) {
    for (const file of exception.files) {
      for (const name of exception.types) {
        if (!used.has(`${name}|${file}`)) {
          failures.push(
            `DTO_CONSTRUCTOR_EXCEPTIONS: ${file} has no full ${name} literal (dead entry: ${exception.reason})`,
          );
        }
      }
    }
  }
  return failures;
}
