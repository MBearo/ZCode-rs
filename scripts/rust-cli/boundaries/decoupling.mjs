// 解耦检查（docs/specs/rust-cli/app-protocol.md §9）：协议适配层不入侵内核。纯函数，这里不做 IO：
// check-boundaries.mjs 收集依赖图、源码与 feature 表后调用；boundaries/decoupling.test.mjs 每条规则一个合成的违规。
//
// 四条规则（kernel-api 只加不改的第五条在 kernel-surface.mjs）：
// 1. 依赖：前端只被入口依赖，协议框架只被前端与入口依赖，协议 crate 互不依赖；规划中的 crate 的依赖也过层规则。
// 2. 词汇：中立层（基础、内核、适配器、平台、协议框架）与别的协议 crate 里没有某种协议的名字、方法名与概念。
// 3. 按前端分支：中立层的代码里没有“当前前端是谁”的写法；登记的形态枚举的变体固定。
// 4. feature：中立层的 crate 只声明登记的 cargo feature。
import { isTestFile, stringLiterals, stripCode, reverseClosure } from "./check.mjs";
import { stripComments } from "./interfaces.mjs";

const ENTRY = "entry";
const FRONTEND = "frontend";
const SHARED = "frontend-shared";

/** 一个 crate 的层：已建的看 LAYERS，规划中的看 PLANNED_CRATES。 */
function layerOf(policy, crate) {
  return policy.LAYERS[crate] ?? policy.PLANNED_CRATES?.[crate]?.layer;
}

/** 正向闭包：`crate` 直接或间接依赖的全部 crate。 */
function forwardClosure(graph, crate) {
  const seen = new Set();
  const queue = [crate];
  while (queue.length) {
    for (const next of graph.get(queue.shift()) ?? []) {
      if (!seen.has(next)) {
        seen.add(next);
        queue.push(next);
      }
    }
  }
  return seen;
}

/** 规则 1：依赖。`graph` 是 crate → 它依赖的本工程 crate（普通、构建与测试依赖都算）。 */
export function checkProtocolLayering(policy, graph) {
  const failures = [];
  for (const crate of graph.keys()) {
    const layer = layerOf(policy, crate);
    if (layer !== FRONTEND && layer !== SHARED) continue;
    const allowed = layer === FRONTEND ? [ENTRY] : [FRONTEND, ENTRY];
    for (const dependent of [...reverseClosure(graph, crate)].sort()) {
      const dependentLayer = layerOf(policy, dependent);
      if (allowed.includes(dependentLayer)) continue;
      failures.push(
        `protocol layering: ${dependent} (${dependentLayer}) depends on ${crate} (${layer}); ${
          layer === FRONTEND
            ? "frontend crates are only assembled by entry crates"
            : "the protocol framework is only used by frontend and entry crates"
        } (app-protocol.md §9.1)`,
      );
    }
  }
  const groups = Object.entries(policy.PROTOCOLS ?? {});
  for (const [name, group] of groups) {
    for (const crate of group.crates.filter((entry) => graph.has(entry))) {
      const closure = forwardClosure(graph, crate);
      for (const [other, otherGroup] of groups) {
        if (other === name) continue;
        for (const target of otherGroup.crates.filter((entry) => closure.has(entry))) {
          failures.push(
            `protocol layering: ${crate} (${name}) depends on ${target} (${other}) directly or through other crates; protocols must not depend on each other (app-protocol.md §9.1)`,
          );
        }
      }
    }
    for (const crate of group.crates) {
      const layer = layerOf(policy, crate);
      if (layer !== FRONTEND) {
        failures.push(
          `PROTOCOLS.${name}: ${crate} must be a frontend crate (registered or planned), found ${layer ?? "nothing"}`,
        );
      }
    }
  }
  for (const [crate, planned] of Object.entries(policy.PLANNED_CRATES ?? {})) {
    if (graph.has(crate)) {
      failures.push(
        `PLANNED_CRATES.${crate}: the crate exists; register it in LAYERS and ALLOWED (README §3.2) and remove it from PLANNED_CRATES`,
      );
    }
    const rule = policy.LAYER_RULES[planned.layer];
    if (!rule) {
      failures.push(`PLANNED_CRATES.${crate}: unknown layer ${planned.layer}`);
      continue;
    }
    if (rule === "*") continue;
    for (const dependency of planned.allowed) {
      const dependencyLayer = layerOf(policy, dependency);
      if (!rule.crates.includes(dependency) && !rule.layers.includes(dependencyLayer)) {
        failures.push(
          `PLANNED_CRATES.${crate}: ${planned.layer} crates must not depend on ${dependency} (${dependencyLayer ?? "unknown"}, LAYER_RULES)`,
        );
      }
    }
  }
  return failures;
}

/** 把一个文件里登记的例外片段换成等长的空格（片段不跨行）；返回换过的文字与用到的例外。 */
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

/** 一段文字里每个匹配的行号（`re` 不带 g 时也全部找出）。 */
function matchLines(text, re) {
  const global = new RegExp(re.source, re.flags.includes("g") ? re.flags : `${re.flags}g`);
  return [...text.matchAll(global)].map((match) => ({
    line: lineOf(text, match.index),
    found: match[0],
  }));
}

/** 规则 2：词汇。`sources` 是 `{ crate, file, text }`（file 相对 packages/rust-cli）。 */
export function checkVocabulary(policy, sources) {
  const failures = [];
  const used = new Set();
  const groups = Object.entries(policy.PROTOCOLS ?? {});
  const protocolOf = new Map();
  for (const [name, group] of groups) for (const crate of group.crates) protocolOf.set(crate, name);
  for (const { crate, file, text: raw } of sources) {
    const layer = layerOf(policy, crate);
    const neutral = policy.PROTOCOL_NEUTRAL_LAYERS.includes(layer);
    const owner = protocolOf.get(crate);
    if (!neutral && owner === undefined) continue;
    const { text, used: hit } = applyExceptions(raw, file, policy.PROTOCOL_TERM_EXCEPTIONS);
    for (const exception of hit) used.add(exception);
    const code = stripCode(text);
    const literals = stringLiterals(text);
    for (const [name, group] of groups) {
      if (owner === name) continue;
      const where =
        owner === undefined ? `${crate} (${layer})` : `${crate} (the ${owner} protocol)`;
      for (const term of group.terms) {
        // comments：整段文字（代码、注释、字面量）；否则代码（去掉注释与字面量）加上每个字符串字面量。
        const lines = new Map();
        for (const { line, found } of matchLines(group.comments ? text : code, term.re)) {
          lines.set(line, found);
        }
        if (!group.comments) {
          for (const { value, line } of literals) {
            const match = term.re.exec(value);
            if (match) lines.set(line, match[0]);
          }
        }
        for (const [line, found] of [...lines].sort(([a], [b]) => a - b)) {
          failures.push(
            `${file}:${line}: "${found}" is ${name} protocol vocabulary (${term.id}); ${where} must not know it (app-protocol.md §9.2)`,
          );
        }
      }
      const methods = new Set(group.methods);
      for (const { value, line } of literals) {
        if (methods.has(value)) {
          failures.push(
            `${file}:${line}: "${value}" is a ${name} protocol method (${group.methodId ?? `${name}-method`}); ${where} must not know it (app-protocol.md §9.2)`,
          );
        }
      }
    }
  }
  for (const exception of policy.PROTOCOL_TERM_EXCEPTIONS) {
    if (!used.has(exception)) {
      failures.push(
        `PROTOCOL_TERM_EXCEPTIONS: "${exception.fragment}" is not in ${exception.file} (dead entry)`,
      );
    }
  }
  return failures;
}

/** 一个枚举在源码里的变体名（按顶层逗号切分，去掉属性；找不到时为 null）。 */
export function frozenEnumVariants(source, name) {
  const code = stripCode(source);
  const head = new RegExp(`\\benum\\s+${name}\\b[^{;]*\\{`).exec(code);
  if (!head) return null;
  let depth = 0;
  let start = head.index + head[0].length;
  const variants = [];
  for (let i = start; i < code.length; i += 1) {
    const c = code[i];
    if ("([{<".includes(c)) depth += 1;
    else if (c === "}" && depth === 0) {
      variants.push(code.slice(start, i));
      break;
    } else if (")]}>".includes(c)) depth -= 1;
    else if (c === "," && depth === 0) {
      variants.push(code.slice(start, i));
      start = i + 1;
    }
  }
  return variants
    .map((variant) => variant.replace(/#\[[^\]]*\]/g, "").trim())
    .filter(Boolean)
    .map((variant) => /^[A-Za-z_][A-Za-z0-9_]*/.exec(variant)?.[0] ?? variant);
}

/** 只在测试里编译的文件（同 check.mjs 的口径）。 */
const isTestOnlyFile = (file) =>
  isTestFile(file) || /(_tests_\w+|\/test_support|\/test_more)\.rs$/.test(file);

const FEATURE_IN_CFG =
  /(?:#!?\[\s*cfg(?:_attr)?\s*\(|\bcfg!\s*\()[^\]]*?\bfeature\s*=\s*"([^"]+)"/g;

/** 规则 3：按前端分支（代码里的写法、cfg 的 feature、形态枚举的变体）。 */
export function checkFrontendBranching(policy, sources) {
  const failures = [];
  const used = new Set();
  const allowedFeatures = Object.keys(policy.NEUTRAL_FEATURES);
  const byFile = new Map(sources.map((source) => [source.file, source]));
  for (const { crate, file, text: raw } of sources) {
    if (!policy.PROTOCOL_NEUTRAL_LAYERS.includes(layerOf(policy, crate))) continue;
    // 测试用已有的形态（例如两种 Surface 各测一遍）不是新的分支；词汇检查照样看测试。
    if (isTestOnlyFile(file)) continue;
    const code = stripCode(raw);
    const { text: open, used: hit } = applyExceptions(
      code,
      file,
      policy.FRONTEND_BRANCH_EXCEPTIONS,
    );
    for (const exception of hit) used.add(exception);
    for (const pattern of policy.FRONTEND_BRANCH_PATTERNS) {
      for (const { line, found } of matchLines(open, pattern.re)) {
        failures.push(
          `${file}:${line}: "${found}" branches on the frontend (${pattern.id}); the kernel takes capability flags, not the name of a frontend (app-protocol.md §9.3)`,
        );
      }
    }
    const withStrings = stripComments(raw);
    for (const match of withStrings.matchAll(FEATURE_IN_CFG)) {
      if (allowedFeatures.includes(match[1])) continue;
      failures.push(
        `${file}:${lineOf(withStrings, match.index)}: cfg feature "${match[1]}" (cfg-feature) is not in NEUTRAL_FEATURES; neutral crates have no per-frontend features (app-protocol.md §9.3)`,
      );
    }
  }
  for (const exception of policy.FRONTEND_BRANCH_EXCEPTIONS) {
    if (!used.has(exception)) {
      failures.push(
        `FRONTEND_BRANCH_EXCEPTIONS: "${exception.fragment}" is not in the code of ${exception.file} (dead entry)`,
      );
    }
  }
  for (const frozen of policy.FROZEN_ENUMS) {
    const source = byFile.get(frozen.file);
    const variants = source ? frozenEnumVariants(source.text, frozen.name) : null;
    if (!variants) {
      failures.push(`FROZEN_ENUMS: enum ${frozen.name} not found in ${frozen.file}`);
      continue;
    }
    const added = variants.filter((variant) => !frozen.variants.includes(variant));
    const removed = frozen.variants.filter((variant) => !variants.includes(variant));
    if (added.length || removed.length) {
      failures.push(
        `${frozen.file}: enum ${frozen.name} has variants [${variants.join(", ")}], FROZEN_ENUMS says [${frozen.variants.join(", ")}]; a new variant is a new frontend branch in the kernel (app-protocol.md §9.3)`,
      );
    }
  }
  return failures;
}

/** 规则 4：中立层的 crate 只声明登记的 cargo feature。`packages` 是 `{ crate, features: [名字] }`。 */
export function checkFeatures(policy, packages) {
  const failures = [];
  const allowed = Object.keys(policy.NEUTRAL_FEATURES);
  for (const { crate, features } of packages) {
    if (!policy.PROTOCOL_NEUTRAL_LAYERS.includes(layerOf(policy, crate))) continue;
    for (const feature of features.filter((name) => !allowed.includes(name))) {
      failures.push(
        `crates/${crate}/Cargo.toml: feature "${feature}" is not in NEUTRAL_FEATURES; neutral crates have no per-frontend features (app-protocol.md §9.3)`,
      );
    }
  }
  return failures;
}
