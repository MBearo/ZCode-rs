// 厂商错误的解耦检查（docs/specs/rust-cli/provider-errors.md §2.1、§2.5、§11）：厂商码与厂商专有的字符串只在第 1 层
// （model 的 `errors/` 目录，厂商分类器）；第 2 层（`errors/dispose.rs`、transport）、query 等只认中性分类。纯函数，这里不做
// IO：check-boundaries.mjs 收集源码后调用；boundaries/vendor.test.mjs 每条规则一个合成的违规。
//
// 规则（规则表是 policy.mjs 的 VENDOR_ERROR_TERMS 与 VENDOR_ERROR_TERM_EXCEPTIONS）：
// 1. 厂商的值：结束原因与业务码的字符串字面量，以及处理模型错误的 crate 里代码中的数字字面量，只在这个厂商的分类器文件；
//    选厂商的写法（`Vendor::Zhipu`）只在分类器目录与 profiles.rs。
// 2. 协议的错误词：Anthropic / OpenAI 协议的错误类型、错误码与结束原因的字符串字面量，只在分类器目录。
// 3. 测试文件不查规则 1 的值与规则 2（测试要把厂商的原始正文端到端喂进去）；选厂商的写法照查。
// 4. 例外只按文件与精确片段，没用到的是死项。
import { isTestFile, stringLiterals, stripCode } from "./check.mjs";

/** 只在测试里编译的文件（同 check.mjs、decoupling.mjs 的口径；crate 的 `tests/` 目录由 isTestFile 认）。 */
const isTestOnlyFile = (file) =>
  isTestFile(file) || /(_tests_\w+|\/test_support|\/test_more)\.rs$/.test(file);

/** `file` 是否在 `owner` 里：以 `/` 结尾的是目录；`.rs` 文件连同它只在测试里编译的 `<名字>_*.rs`。 */
function ownedBy(file, owner) {
  if (owner.endsWith("/")) return file.startsWith(owner);
  if (file === owner) return true;
  return isTestOnlyFile(file) && file.startsWith(`${owner.slice(0, -".rs".length)}_`);
}

const ownedByAny = (file, owners) => owners.some((owner) => ownedBy(file, owner));

/** 把一个文件里登记的例外片段换成等长的空格（同 decoupling.mjs 的写法）；返回换过的文字与用到的例外。 */
function applyExceptions(text, file, exceptions) {
  let out = text;
  const used = new Set();
  for (const exception of exceptions) {
    if (exception.file !== file) continue;
    if (out.includes(exception.fragment)) {
      used.add(exception);
      out = out.split(exception.fragment).join(" ".repeat(exception.fragment.length));
    }
  }
  return { text: out, used };
}

const lineOf = (text, index) => text.slice(0, index).split("\n").length;

// 代码里的整数字面量：前面不是名字字符或 `.`（`0x1302`、`u1302`、`x.1302` 不算），可带 `_` 与整数后缀，后面不是名字字符
// 或 `.数字`（浮点数不算）。
const INTEGER =
  /(?<![A-Za-z0-9_.])(\d[\d_]*)(?:[ui](?:8|16|32|64|128|size))?(?![A-Za-z0-9_]|\.\d)/g;

/**
 * 厂商错误词的检查。`sources` 是 `{ crate, file, text }`（file 相对 packages/rust-cli），与 checkVocabulary 相同。
 * 返回违规清单（每个文件按行号排序），最后是没用到的例外。
 */
export function checkVendorTerms(policy, sources) {
  const terms = policy.VENDOR_ERROR_TERMS;
  const exceptions = policy.VENDOR_ERROR_TERM_EXCEPTIONS;
  const failures = [];
  const used = new Set();
  // 字符串字面量的值 → 它属于谁（同一个值只登记一处）。
  const literalOwners = new Map();
  const codeOwners = new Map();
  for (const vendor of terms.vendors) {
    for (const value of vendor.values) {
      literalOwners.set(value, { kind: "vendor", id: `${vendor.id}-value`, owners: vendor.owners });
    }
    for (const code of vendor.codes) {
      const owner = { kind: "vendor", id: `${vendor.id}-code`, owners: vendor.owners, vendor };
      literalOwners.set(code, owner);
      codeOwners.set(code, owner);
    }
  }
  for (const group of terms.protocol.groups) {
    for (const value of group.values) {
      literalOwners.set(value, { kind: "protocol", id: group.id, owners: terms.protocol.owners });
    }
  }
  for (const { crate, file, text: raw } of sources) {
    const { text, used: hit } = applyExceptions(raw, file, exceptions);
    for (const exception of hit) used.add(exception);
    const code = stripCode(text);
    const test = isTestOnlyFile(file);
    // 同一行同一个值只报一次。
    const hits = new Map();
    const report = (line, found, owner) => {
      hits.set(`${line}|${found}|${owner.id}`, { line, found, owner });
    };
    // 规则 1 的选厂商写法：测试文件也查。
    for (const vendor of terms.vendors) {
      const { re, owners } = vendor.selector;
      if (ownedByAny(file, owners)) continue;
      const global = new RegExp(re.source, re.flags.includes("g") ? re.flags : `${re.flags}g`);
      for (const match of code.matchAll(global)) {
        report(lineOf(code, match.index), match[0], {
          kind: "vendor",
          id: `${vendor.id}-selector`,
          owners,
        });
      }
    }
    if (!test) {
      // 规则 1 的值与码、规则 2：字符串字面量与登记的值完全相同。
      for (const { value, line } of stringLiterals(text)) {
        const owner = literalOwners.get(value);
        if (owner && !ownedByAny(file, owner.owners)) report(line, value, owner);
      }
      // 规则 1 的码：处理模型错误的 crate 里，代码中的数字字面量。
      for (const match of code.matchAll(INTEGER)) {
        const owner = codeOwners.get(match[1].replaceAll("_", ""));
        if (!owner || !owner.vendor.numericCrates.includes(crate)) continue;
        if (!ownedByAny(file, owner.owners)) report(lineOf(code, match.index), match[0], owner);
      }
    }
    for (const { line, found, owner } of [...hits.values()].sort((a, b) => a.line - b.line)) {
      failures.push(
        `${file}:${line}: "${found}" is a ${owner.kind} error value (${owner.id}); only the vendor classifier (${owner.owners.join(", ")}) may know it (provider-errors.md §2.5)`,
      );
    }
  }
  for (const exception of exceptions) {
    if (!used.has(exception)) {
      failures.push(
        `VENDOR_ERROR_TERM_EXCEPTIONS: "${exception.fragment}" is not in ${exception.file} (dead entry)`,
      );
    }
  }
  return failures;
}
