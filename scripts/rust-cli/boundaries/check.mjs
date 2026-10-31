// packages/rust-cli 的边界检查（纯函数部分，docs/specs/rust-cli/README.md §3.1、§3.2、§4）。
//
// 输入是 Cargo 解析好的依赖图（`cargo metadata`）、源文件与 clippy.toml 的文字；输出是违规清单。
// README 表格与规则表的一致性只在本仓库检查（scripts/rust-cli/repo-only/readme.mjs，工作线 F60）。
// 读文件、跑 cargo 在 check-boundaries.mjs，这里不做 IO，测试直接构造输入。

const IO_FEATURE_NAMES = (policy) => Object.keys(policy.TOKIO_IO_FEATURES);

/// 把 Rust 源码里的注释与字符串 / 字符字面量的内容换成空格（换行保留），之后的正则只看代码。
export function stripCode(source) {
  return scanCode(source).code;
}

/// 源码里的字符串字面量（普通、原始与字节串；不含注释与字符字面量）：`{ value, line }`，`value` 是引号之间的原文。
export function stringLiterals(source) {
  return scanCode(source).literals;
}

/// stripCode 与 stringLiterals 共用的扫描：去掉注释与字面量内容的代码，加上每个字符串字面量的位置与原文。
function scanCode(source) {
  const out = [];
  const literals = [];
  // 字面量按出现顺序记录，行号从上一个位置往后数换行，整个文件只扫一遍。
  let counted = 0;
  let line = 1;
  const lineAt = (index) => {
    for (; counted < index; counted += 1) if (source[counted] === "\n") line += 1;
    return line;
  };
  const length = source.length;
  let i = 0;
  const blank = (text) => text.replace(/[^\n]/g, " ");
  while (i < length) {
    const c = source[i];
    const next = source[i + 1];
    if (c === "/" && next === "/") {
      const end = source.indexOf("\n", i);
      const stop = end === -1 ? length : end;
      out.push(blank(source.slice(i, stop)));
      i = stop;
    } else if (c === "/" && next === "*") {
      let depth = 1;
      let j = i + 2;
      while (j < length && depth > 0) {
        if (source[j] === "/" && source[j + 1] === "*") {
          depth += 1;
          j += 2;
        } else if (source[j] === "*" && source[j + 1] === "/") {
          depth -= 1;
          j += 2;
        } else j += 1;
      }
      out.push(blank(source.slice(i, j)));
      i = j;
    } else if (
      (c === "r" || (c === "b" && next === "r")) &&
      /^b?r#*"/.test(source.slice(i, i + 260)) &&
      !/[A-Za-z0-9_]/.test(source[i - 1] ?? "")
    ) {
      const head = source.slice(i).match(/^b?r(#*)"/);
      const close = `"${head[1]}`;
      const end = source.indexOf(close, i + head[0].length);
      const stop = end === -1 ? length : end + close.length;
      const value = source.slice(i + head[0].length, stop - close.length);
      literals.push({ value, line: lineAt(i) });
      out.push(head[0] + blank(value) + close);
      i = stop;
    } else if (c === '"') {
      let j = i + 1;
      while (j < length && source[j] !== '"') j += source[j] === "\\" ? 2 : 1;
      literals.push({ value: source.slice(i + 1, j), line: lineAt(i) });
      out.push('"' + blank(source.slice(i + 1, j)) + '"');
      i = j + 1;
    } else if (c === "'") {
      const literal = source
        .slice(i)
        .match(/^'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'/);
      if (literal) {
        out.push("'" + blank(literal[0].slice(1, -1)) + "'");
        i += literal[0].length;
      } else {
        out.push(c);
        i += 1;
      }
    } else {
      out.push(c);
      i += 1;
    }
  }
  return { code: out.join(""), literals };
}

/// clippy.toml 的禁令条目：`{ path = "…", reason = "…", allow-invalid = true }`（一行一条）。
export function parseClippy(text) {
  const entries = [];
  const other = [];
  let inMethods = false;
  for (const raw of text.split("\n")) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;
    if (/^disallowed-methods\s*=\s*\[/.test(line)) {
      inMethods = true;
      continue;
    }
    if (inMethods && line === "]") {
      inMethods = false;
      continue;
    }
    if (inMethods) {
      const path = line.match(/\bpath\s*=\s*"([^"]+)"/);
      const reason = line.match(/\breason\s*=\s*"([^"]*)"/);
      entries.push({
        path: path?.[1] ?? "",
        reason: reason?.[1] ?? "",
        allowInvalid: /\ballow-invalid\s*=\s*true\b/.test(line),
        line,
      });
    } else other.push(line.replace(/\s+/g, " "));
  }
  return { entries, other };
}

export function categoryOf(policy, path) {
  return policy.IO_CATEGORIES.find((category) =>
    category.prefixes.some((prefix) => path.startsWith(prefix)),
  );
}

function escape(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

const CONSTRUCTORS = new Set(["new", "new_v4", "new_v6", "now", "builder"]);

/// 一条禁令在源码里的写法：`Type::method`、`module::function(`，以及实例方法 `.method(`。`.open(` 这样的方法名
/// 多个类型都有（`OpenOptions::open` 与命名管道的 `ClientOptions::open`），所以实例方法的写法只在文件里提到这个
/// 类型（`\bType`，`Path` 也认 `PathBuf`）时才算。
export function callPatterns(path) {
  const segments = path.split("::");
  const last = segments.at(-1);
  const parent = segments.at(-2) ?? "";
  if (/^[A-Z]/.test(parent)) {
    const patterns = [{ call: new RegExp(`\\b${escape(parent)}::${escape(last)}\\b`) }];
    if (!CONSTRUCTORS.has(last)) {
      patterns.push({
        call: new RegExp(`\\.${escape(last)}\\s*\\(`),
        type: new RegExp(`\\b${escape(parent)}`),
      });
    }
    return patterns;
  }
  return [{ call: new RegExp(`\\b${escape(parent)}::${escape(last)}\\s*\\(`) }];
}

/// 豁免覆盖的代码：属性之后的下一个函数（到配对的 `}`）或语句（到 `;`）。`code` 已去掉注释与字符串。
export function exemptedItem(code, from) {
  let i = from;
  const skipSpace = () => {
    while (i < code.length && /\s/.test(code[i])) i += 1;
  };
  skipSpace();
  // 其他外部属性。
  while (code.startsWith("#[", i)) {
    let depth = 0;
    for (; i < code.length; i += 1) {
      if (code[i] === "[") depth += 1;
      else if (code[i] === "]" && --depth === 0) {
        i += 1;
        break;
      }
    }
    skipSpace();
  }
  const start = i;
  let depth = 0;
  for (; i < code.length; i += 1) {
    const c = code[i];
    if (c === "(" || c === "[") depth += 1;
    else if (c === ")" || c === "]") depth -= 1;
    else if (c === ";" && depth === 0) return code.slice(start, i + 1);
    else if (c === "{" && depth === 0) {
      let braces = 0;
      for (; i < code.length; i += 1) {
        if (code[i] === "{") braces += 1;
        else if (code[i] === "}" && --braces === 0) return code.slice(start, i + 1);
      }
    }
  }
  return code.slice(start);
}

/// 一段代码里调用了哪些 IO 类别的被禁方法。`file` 是整个文件（实例方法要求文件里提到它的类型）。
export function categoriesIn(policy, entries, code, file = code) {
  const found = new Set();
  for (const entry of entries) {
    const category = categoryOf(policy, entry.path);
    const hit = callPatterns(entry.path).some(
      ({ call, type }) => call.test(code) && (!type || type.test(file)),
    );
    if (category && hit) found.add(category.name);
  }
  return found;
}

export function isTestFile(file) {
  return (
    file.endsWith("_tests.rs") ||
    /(^|\/)(tests|examples|benches)\//.test(file.replaceAll("\\", "/"))
  );
}

const SPAWN =
  /\b(?:tokio::spawn|tokio::task::spawn(?:_blocking|_local)?|std::thread::spawn|thread::spawn)\s*\(/;
const OUTER_EXEMPTION = /#\[\s*(expect|allow)\s*\([^\]]*?\bclippy::disallowed_methods\b[^\]]*\]/g;
const INNER_EXEMPTION = /#!\[\s*(?:expect|allow)\s*\([^\]]*?\bclippy::disallowed_methods\b/;

/// 源文件：行数、豁免的形式与类别、任务启动、内核里的单调时钟。
export function checkSources(policy, clippyEntries, sources) {
  const failures = [];
  for (const { crate, file, text } of sources) {
    if (text.split("\n").length > policy.MAX_LINES) {
      failures.push(`${file}: exceeds ${policy.MAX_LINES} lines`);
    }
    if (isTestFile(file)) continue;
    const code = stripCode(text);
    if (INNER_EXEMPTION.test(code)) {
      failures.push(
        `${file}: crate / module level exemptions of clippy::disallowed_methods are not allowed; exempt the function (README §4)`,
      );
    }
    for (const match of code.matchAll(OUTER_EXEMPTION)) {
      const line = code.slice(0, match.index).split("\n").length;
      if (match[1] === "allow") {
        failures.push(
          `${file}:${line}: use #[expect(clippy::disallowed_methods, reason = …)], not allow`,
        );
      }
      const item = exemptedItem(code, match.index + match[0].length);
      const categories = categoriesIn(policy, clippyEntries, item, code);
      if (categories.size === 0) {
        failures.push(
          `${file}:${line}: exemption covers no recognizable disallowed call; write the call with its path (e.g. fs::read, Command::new)`,
        );
      }
      for (const name of categories) {
        const category = policy.IO_CATEGORIES.find((entry) => entry.name === name);
        if (!category.entries.includes(crate)) {
          failures.push(
            `${file}:${line}: ${crate} is not an entry crate for ${name} IO (entries: ${category.entries.join(", ")})`,
          );
        }
      }
    }
    if (crate !== "base" && SPAWN.test(code)) {
      failures.push(`${file}: start tasks through rust_cli_base::TaskScope / owned / blocking`);
    }
    if (policy.LAYERS[crate] === "kernel" && /\bInstant\b/.test(code)) {
      failures.push(
        `${file}: kernel crates take time from the injected Clock, not Instant (README §4)`,
      );
    }
  }
  return failures;
}

/// 只在测试里编译的文件：isTestFile 之外，`#[path]` 引进测试模块的 `*_tests_*.rs` 与 `#[cfg(test)]` 的
/// `test_support.rs` / `test_more.rs`（同一个名字只定义一处的检查不看测试）。
export function isTestOnly(file) {
  return isTestFile(file) || /(_tests_\w+|\/test_support|\/test_more)\.rs$/.test(file);
}

/// 同一个名字只定义一处（README §4.1，F55）：产品代码的字符串字面量里，登记的名字只出现在它的定义处。
export function checkDefinitions(policy, sources) {
  const failures = [];
  const defined = new Set();
  for (const { file, text } of sources) {
    if (isTestOnly(file)) continue;
    for (const { value, line } of stringLiterals(text)) {
      for (const definition of policy.SINGLE_DEFINITIONS) {
        if (!definition.pattern.test(value)) continue;
        if (definition.owners.includes(file)) defined.add(`${definition.name}|${file}`);
        else {
          failures.push(
            `${file}:${line}: ${definition.name} is defined only in ${definition.owners.join(", ")}; use ${definition.hint} (README §4.1)`,
          );
        }
      }
    }
  }
  for (const definition of policy.SINGLE_DEFINITIONS) {
    for (const owner of definition.owners) {
      if (!defined.has(`${definition.name}|${owner}`)) {
        failures.push(
          `SINGLE_DEFINITIONS: ${owner} does not define ${definition.name} (dead entry)`,
        );
      }
    }
  }
  return failures;
}

/// clippy.toml：每条禁令有类别；allow-invalid 只给登记的前缀；crate 级配置 = 根配置去掉放开的类别。
export function checkClippy(policy, rootText, crateTexts) {
  const failures = [];
  const root = parseClippy(rootText);
  for (const entry of root.entries) {
    if (!categoryOf(policy, entry.path)) {
      failures.push(`clippy.toml: "${entry.path}" belongs to no IO category in IO_CATEGORIES`);
    }
    if (
      entry.allowInvalid &&
      !policy.ALLOW_INVALID_PREFIXES.some((p) => entry.path.startsWith(p))
    ) {
      failures.push(
        `clippy.toml: "${entry.path}" may not use allow-invalid (only platform- or feature-gated paths)`,
      );
    }
  }
  for (const [crate, text] of Object.entries(crateTexts)) {
    const open = policy.CRATE_LEVEL_OPEN[crate];
    if (!open) {
      failures.push(
        `crates/${crate}/clippy.toml: only crates in CRATE_LEVEL_OPEN may have their own clippy.toml`,
      );
      continue;
    }
    const own = parseClippy(text);
    const key = (entry) => `${entry.path}|${entry.reason}|${entry.allowInvalid}`;
    const expected = root.entries
      .filter((entry) => !open.includes(categoryOf(policy, entry.path)?.name))
      .map(key);
    const actual = own.entries.map(key);
    for (const missing of expected.filter((entry) => !actual.includes(entry))) {
      failures.push(`crates/${crate}/clippy.toml: missing root entry ${missing.split("|")[0]}`);
    }
    for (const extra of actual.filter((entry) => !expected.includes(entry))) {
      failures.push(
        `crates/${crate}/clippy.toml: entry ${extra.split("|")[0]} differs from the root config`,
      );
    }
    if (own.other.join("\n") !== root.other.join("\n")) {
      failures.push(
        `crates/${crate}/clippy.toml: settings other than disallowed-methods differ from the root config`,
      );
    }
  }
  for (const crate of Object.keys(policy.CRATE_LEVEL_OPEN)) {
    if (!(crate in crateTexts))
      failures.push(`crates/${crate}/clippy.toml: missing (CRATE_LEVEL_OPEN)`);
  }
  return failures;
}

/// 依赖图：层、依赖表（= 实际）、path 依赖、第三方归属（= 实际）、tokio 的 IO 特性（= 实际）。
/// `packages` 是 `{ crate, manifest, dependencies }`，dependencies 来自 cargo metadata（name 是真实包名，
/// 改名在 rename 里）；`internal` 把包名映射到 crate 目录名。
export function checkGraph(policy, packages, internal, isInside) {
  const failures = [];
  const names = new Set(packages.map((entry) => entry.crate));
  for (const crate of names) {
    if (!(crate in policy.ALLOWED)) failures.push(`crates/${crate}: not registered in ALLOWED`);
    if (!(crate in policy.LAYERS)) failures.push(`crates/${crate}: not registered in LAYERS`);
  }
  for (const crate of Object.keys(policy.ALLOWED)) {
    if (!names.has(crate)) failures.push(`ALLOWED.${crate}: no such crate (dead entry)`);
  }
  // 依赖表本身不能越层。
  for (const [crate, allowed] of Object.entries(policy.ALLOWED)) {
    const rule = policy.LAYER_RULES[policy.LAYERS[crate]];
    if (allowed === "*" || rule === "*" || !rule) continue;
    for (const dependency of allowed) {
      const layer = policy.LAYERS[dependency];
      if (!rule.crates.includes(dependency) && !rule.layers.includes(layer)) {
        failures.push(
          `ALLOWED.${crate}: ${policy.LAYERS[crate]} crates must not depend on ${dependency} (${layer}, README §3.1)`,
        );
      }
    }
  }
  const owners = {};
  const used = new Set();
  const tokio = {};
  for (const { crate, manifest, dependencies } of packages) {
    const allowed = policy.ALLOWED[crate];
    const actual = new Set();
    for (const dependency of dependencies) {
      if (dependency.path && !isInside(dependency.path)) {
        failures.push(`${manifest}: path dependency "${dependency.path}" leaves packages/rust-cli`);
      }
      const target = internal.get(dependency.name);
      if (target) {
        actual.add(target);
        if (allowed !== undefined && allowed !== "*" && !allowed.includes(target)) {
          failures.push(
            `${manifest}: ${crate} must not depend on ${target} (not in ALLOWED.${crate})`,
          );
        }
        continue;
      }
      const name = dependency.name;
      used.add(name);
      const product = dependency.kind !== "dev";
      if (name in policy.THIRD_PARTY_OWNERS) {
        if (product) (owners[name] ??= new Set()).add(crate);
        if (product && !policy.THIRD_PARTY_OWNERS[name].includes(crate)) {
          failures.push(
            `${manifest}: only ${policy.THIRD_PARTY_OWNERS[name].join(", ")} may depend on ${name} (IO library)`,
          );
        }
      } else if (policy.TEST_ONLY.includes(name)) {
        if (product) failures.push(`${manifest}: ${name} may only be a dev-dependency`);
      } else if (!policy.PURE.includes(name)) {
        failures.push(
          `${manifest}: third-party dependency ${name} is not classified; add it to THIRD_PARTY_OWNERS (IO) or PURE`,
        );
      }
      if (name === "tokio" && product) {
        for (const feature of dependency.features) {
          if (IO_FEATURE_NAMES(policy).includes(feature)) (tokio[feature] ??= new Set()).add(crate);
        }
      }
    }
    if (Array.isArray(allowed)) {
      for (const dead of allowed.filter((entry) => !actual.has(entry))) {
        failures.push(`ALLOWED.${crate}: ${dead} is not an actual dependency (dead entry)`);
      }
    }
  }
  for (const [name, expected] of Object.entries(policy.THIRD_PARTY_OWNERS)) {
    for (const dead of expected.filter((crate) => !owners[name]?.has(crate))) {
      failures.push(`THIRD_PARTY_OWNERS.${name}: ${dead} does not depend on it (dead entry)`);
    }
  }
  for (const name of [...policy.PURE, ...policy.TEST_ONLY]) {
    if (!used.has(name))
      failures.push(`${name}: listed as PURE / TEST_ONLY but nothing depends on it (dead entry)`);
  }
  for (const [feature, expected] of Object.entries(policy.TOKIO_IO_FEATURES)) {
    const actual = tokio[feature] ?? new Set();
    for (const crate of actual) {
      if (!expected.includes(crate)) {
        failures.push(
          `crates/${crate}: tokio feature "${feature}" is not registered for it in TOKIO_IO_FEATURES`,
        );
      }
    }
    for (const dead of expected.filter((crate) => !actual.has(crate))) {
      failures.push(`TOKIO_IO_FEATURES.${feature}: ${dead} does not declare it (dead entry)`);
    }
  }
  return failures;
}

/// 依赖图（docs/specs/rust-cli/remote-exec.md §15.6）：crate → 它依赖的本工程 crate（普通、构建与测试依赖都算，与依赖表的
/// 口径相同）。
export function internalGraph(packages, internal) {
  const graph = new Map(packages.map((entry) => [entry.crate, new Set()]));
  for (const { crate, dependencies } of packages) {
    for (const dependency of dependencies) {
      const target = internal.get(dependency.name);
      if (target && target !== crate) graph.get(crate).add(target);
    }
  }
  return graph;
}

/// 每个 crate 的入度 Ca、出度 Ce 与不稳定度 I = Ce / (Ca + Ce)（两者都是 0 时 I = 0）。
export function instability(graph) {
  const result = new Map([...graph.keys()].map((crate) => [crate, { ca: 0, ce: 0, i: 0 }]));
  for (const [crate, targets] of graph) {
    result.get(crate).ce = targets.size;
    for (const target of targets) result.get(target).ca += 1;
  }
  for (const entry of result.values()) {
    entry.i = entry.ca + entry.ce === 0 ? 0 : entry.ce / (entry.ca + entry.ce);
  }
  return result;
}

/// 稳定依赖原则（§15.6 第 4 条）：每条边 A → B 要求 I(B) ≤ I(A)；例外登记在 `SDP_EXCEPTIONS`（键 `A->B`，值是理由）。
export function checkStableDependencies(policy, graph) {
  const failures = [];
  const values = instability(graph);
  const used = new Set();
  const round = (value) => value.toFixed(2);
  for (const [crate, targets] of graph) {
    for (const target of targets) {
      const [from, to] = [values.get(crate).i, values.get(target).i];
      if (to <= from + 1e-9) continue;
      const key = `${crate}->${target}`;
      if (key in policy.SDP_EXCEPTIONS) used.add(key);
      else
        failures.push(
          `stable dependencies: ${crate} (I=${round(from)}) depends on the less stable ${target} (I=${round(to)}); depend on something more stable or register ${key} in SDP_EXCEPTIONS with a reason`,
        );
    }
  }
  for (const key of Object.keys(policy.SDP_EXCEPTIONS)) {
    if (!used.has(key)) failures.push(`SDP_EXCEPTIONS.${key}: no longer an exception (dead entry)`);
  }
  return failures;
}

/// 反向依赖闭包：直接或间接依赖 `crate` 的全部 crate（改它会重编、可能要改的范围）。
export function reverseClosure(graph, crate) {
  const dependents = new Map([...graph.keys()].map((name) => [name, []]));
  for (const [name, targets] of graph)
    for (const target of targets) dependents.get(target)?.push(name);
  const seen = new Set();
  const queue = [crate];
  while (queue.length) {
    for (const next of dependents.get(queue.shift()) ?? []) {
      if (!seen.has(next)) {
        seen.add(next);
        queue.push(next);
      }
    }
  }
  return seen;
}

/// 波及范围（§15.6 第 2 条）：`BLAST_RADIUS` 给出的反向依赖闭包必须与实际相等；任何一个的闭包里都不能有
/// `BLAST_FORBIDDEN_LAYERS` 的 crate（内核与前端不认识远端）。
export function checkBlastRadius(policy, graph) {
  const failures = [];
  for (const [crate, expected] of Object.entries(policy.BLAST_RADIUS)) {
    if (!graph.has(crate)) {
      failures.push(`BLAST_RADIUS.${crate}: no such crate (dead entry)`);
      continue;
    }
    const actual = reverseClosure(graph, crate);
    const missing = expected.filter((name) => !actual.has(name));
    const extra = [...actual].filter((name) => !expected.includes(name)).sort();
    if (missing.length || extra.length) {
      failures.push(
        `blast radius: crates depending on ${crate} are [${[...actual].sort().join(", ")}], BLAST_RADIUS.${crate} says [${[...expected].sort().join(", ")}]`,
      );
    }
    for (const name of actual) {
      if (policy.BLAST_FORBIDDEN_LAYERS.includes(policy.LAYERS[name])) {
        failures.push(
          `blast radius: ${name} (${policy.LAYERS[name]}) depends on ${crate}; kernel and frontend crates must not know the remote crates`,
        );
      }
    }
  }
  return failures;
}
