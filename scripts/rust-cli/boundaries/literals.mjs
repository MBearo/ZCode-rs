// 容易变的字符串只在常量模块里（工作线 F121，docs/specs/rust-cli/README.md §4.1 第 6 条）：模型 ID 与主机名只许出现在
// base 的 `models.rs`、`hosts.rs` 与脚本一侧的 `fixture/model.mjs`；业务代码读的环境变量名只许出现在 base 的
// `names.rs`。纯函数，这里不做 IO：check-boundaries.mjs 收集源码后调用；boundaries/literals.test.mjs 每条规则一个
// 合成的违规。
//
// 规则表在 policy.mjs 的 VOLATILE_LITERALS 与 VOLATILE_EXCEPTIONS：
// 1. 只看字符串字面量（Rust 的普通、原始与字节串；JS 的单双引号与模板串），注释不算。
// 2. 每条规则有作用范围：`tests: true` 时测试也查（模型 ID、主机名），否则只查产品代码（环境变量名）。
// 3. 例外按文件登记，每项写类别与理由；登记了却没有命中的是死项。
import { isTestFile, stringLiterals } from "./check.mjs";

/** 只在测试里编译的 Rust 文件（同 check.mjs、vendor.mjs 的口径），加上示例程序。 */
const isTestOnlyFile = (file) =>
  isTestFile(file) || /(_tests_\w+|\/test_support|\/test_more)\.rs$/.test(file);

/** JS 源码里的字符串字面量：`{ value, line }`。注释跳过；模板串整段算一个字面量（`${}` 里的表达式也在内）。 */
export function jsStringLiterals(source) {
  const literals = [];
  let line = 1;
  let i = 0;
  const length = source.length;
  while (i < length) {
    const c = source[i];
    const next = source[i + 1];
    if (c === "\n") {
      line += 1;
      i += 1;
    } else if (c === "/" && next === "/") {
      const end = source.indexOf("\n", i);
      i = end === -1 ? length : end;
    } else if (c === "/" && next === "*") {
      const end = source.indexOf("*/", i + 2);
      const stop = end === -1 ? length : end + 2;
      line += source.slice(i, stop).split("\n").length - 1;
      i = stop;
    } else if (c === '"' || c === "'" || c === "`") {
      let j = i + 1;
      while (j < length && source[j] !== c && (c === "`" || source[j] !== "\n"))
        j += source[j] === "\\" ? 2 : 1;
      const value = source.slice(i + 1, j);
      literals.push({ value, line });
      line += value.split("\n").length - 1;
      i = j + 1;
    } else i += 1;
  }
  return literals;
}

/** 这条规则在这个文件里要不要查：所有者、范围与例外（例外登记了这个文件与规则时记为用到）。 */
function applies(rule, file, test, exceptions, used) {
  if (rule.owners.includes(file)) return false;
  if (test && !rule.tests) return false;
  const exception = exceptions.find(
    (entry) => entry.files.includes(file) && entry.rules.includes(rule.name),
  );
  if (exception) {
    used.add(`${rule.name}|${file}`);
    return false;
  }
  return true;
}

/**
 * `rust`：`{ file, text }`，路径相对 packages/rust-cli（`crates/...`）；`js`：`{ file, text }`，路径相对仓库根
 * （`scripts/rust-cli/...`）。返回违规清单。
 */
export function checkVolatileLiterals(policy, rust, js) {
  const failures = [];
  const used = new Set();
  const visit = (file, test, literals) => {
    for (const rule of policy.VOLATILE_LITERALS) {
      const matched = literals.filter(({ value }) => rule.pattern.test(value));
      if (!matched.length) continue;
      if (!applies(rule, file, test, policy.VOLATILE_EXCEPTIONS, used)) continue;
      for (const { line, value } of matched) {
        failures.push(
          `${file}:${line}: ${rule.name} "${value.slice(0, 80)}" belongs in ${rule.owners.join(" or ")}; ${rule.hint} (README §4.1)`,
        );
      }
    }
  };
  for (const { file, text } of rust) {
    if (policy.VOLATILE_UNCHECKED.some((pattern) => pattern.test(file))) continue;
    visit(file, isTestOnlyFile(file) || file.includes("/examples/"), stringLiterals(text));
  }
  for (const { file, text } of js) {
    if (policy.VOLATILE_UNCHECKED.some((pattern) => pattern.test(file))) continue;
    visit(file, true, jsStringLiterals(text));
  }
  for (const exception of policy.VOLATILE_EXCEPTIONS) {
    for (const file of exception.files) {
      for (const name of exception.rules) {
        if (!used.has(`${name}|${file}`)) {
          failures.push(
            `VOLATILE_EXCEPTIONS: ${file} has no ${name} literal (dead entry: ${exception.reason})`,
          );
        }
      }
    }
  }
  for (const rule of policy.VOLATILE_LITERALS) {
    for (const owner of rule.owners) {
      const sources = owner.endsWith(".rs") ? rust : js;
      const text = sources.find((source) => source.file === owner)?.text;
      if (text === undefined) continue;
      const literals = owner.endsWith(".rs") ? stringLiterals(text) : jsStringLiterals(text);
      if (!literals.some(({ value }) => rule.pattern.test(value)))
        failures.push(`VOLATILE_LITERALS: ${owner} defines no ${rule.name} (dead owner)`);
    }
  }
  return failures;
}
