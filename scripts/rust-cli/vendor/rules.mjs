// 第三方源码的 vendor 规则（docs/specs/rust-cli/embedded-search.md §5–§7、§9）。纯函数，fetch.mjs 做 IO。
//
// packages/rust-cli/vendor/sources.json 的每个组件：从官方地址下载归档、核对 SHA256、解包到新建的空目录，只把
// `keep` 列出的路径复制进 `packages/rust-cli/vendor/<into>`，再按 `edits` 逐处做精确替换（每处必须恰好出现一次）。
// `--check` 不写文件，比较重新生成的结果与仓库里的文件是否逐字节相同。

import { createHash } from "node:crypto";

/** 归档里的相对路径是不是在 keep 列表里（列表项是文件或目录前缀，`/` 分隔）。 */
export function kept(relative, keep) {
  return keep.some((entry) => relative === entry || relative.startsWith(`${entry}/`));
}

/** 去掉归档的前 `strip` 层目录；层数不够时返回 null（例如归档根目录本身）。 */
export function stripPath(relative, strip) {
  const parts = relative.split("/").filter(Boolean);
  if (parts.length <= strip) return null;
  return parts.slice(strip).join("/");
}

/** 归档里不允许的路径：绝对路径、`..`、反斜杠（解包前就拒绝，不让归档写到目标目录之外）。 */
export function unsafePath(relative) {
  return (
    relative.startsWith("/") ||
    relative.includes("\\") ||
    relative.split("/").some((part) => part === "..")
  );
}

export function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

/** 按顺序做精确替换：每个 `old` 在当时的文本里必须恰好出现一次。 */
export function applyEdits(text, edits, file) {
  let result = text;
  for (const edit of edits) {
    const first = result.indexOf(edit.old);
    if (first === -1) throw new Error(`${file}: 找不到要替换的文字：${edit.old.slice(0, 60)}`);
    if (result.indexOf(edit.old, first + 1) !== -1) {
      throw new Error(`${file}: 要替换的文字出现了不止一次：${edit.old.slice(0, 60)}`);
    }
    result = result.slice(0, first) + edit.new + result.slice(first + edit.old.length);
  }
  return result;
}

/** 组件描述的必填项与形状。 */
export function validateComponent(component) {
  const required = ["id", "version", "url", "sha256", "license", "into", "strip", "keep"];
  for (const key of required) {
    if (component[key] === undefined) throw new Error(`${component.id ?? "?"}: 缺少 ${key}`);
  }
  if (!/^https:\/\/(github\.com|codeload\.github\.com|static\.crates\.io)\//.test(component.url)) {
    throw new Error(
      `${component.id}: 只从官方来源下载（GitHub release / tag、crates.io）：${component.url}`,
    );
  }
  if (!/^[0-9a-f]{64}$/.test(component.sha256))
    throw new Error(`${component.id}: sha256 不是 64 位十六进制`);
  if (unsafePath(component.into) || component.into.split("/").length > 3) {
    throw new Error(`${component.id}: into 必须是 vendor 下的相对路径`);
  }
  for (const entry of component.keep) {
    if (unsafePath(entry)) throw new Error(`${component.id}: keep 里有不安全的路径 ${entry}`);
  }
  for (const file of Object.keys(component.edits ?? {})) {
    if (!kept(file, component.keep))
      throw new Error(`${component.id}: 改动的文件 ${file} 不在 keep 里`);
  }
  return component;
}

/** 比较两份 {相对路径: Buffer}：返回缺少的、多出的、内容不同的路径。 */
export function compareTrees(expected, actual) {
  const missing = [];
  const extra = [];
  const changed = [];
  for (const [path, bytes] of expected) {
    const other = actual.get(path);
    if (!other) missing.push(path);
    else if (!other.equals(bytes)) changed.push(path);
  }
  for (const path of actual.keys()) if (!expected.has(path)) extra.push(path);
  return { missing, extra, changed };
}
