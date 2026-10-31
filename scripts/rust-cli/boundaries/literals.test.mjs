// 容易变的字符串只在常量模块里（工作线 F121，docs/specs/rust-cli/README.md §4.1 第 6 条）：
//   node --test scripts/rust-cli/boundaries/literals.test.mjs
// 每条规则一个合成的违规；所有者与例外放行；测试的范围；注释不算；JS 的字面量；死项。
import assert from "node:assert/strict";
import test from "node:test";

import { checkVolatileLiterals, jsStringLiterals } from "./literals.mjs";
import * as policy from "./policy.mjs";

const rust = (file, text) => ({ file, text });
// 合成的源码里没有真实例外登记的文件：缺省不带例外（否则全是死项）。
const NO_EXCEPTIONS = { ...policy, VOLATILE_EXCEPTIONS: [] };
const run = (rustSources, js = [], rules = NO_EXCEPTIONS) =>
  checkVolatileLiterals(rules, rustSources, js);
const PRODUCT = "crates/x/src/lib.rs";
const TEST = "crates/x/src/lib_tests.rs";

test("模型 ID：产品与测试的字面量都报，常量模块放行，注释不算", () => {
  for (const id of [
    "glm-5.3",
    "GLM-5.3-Flash",
    "deepseek-v4-pro",
    "claude-opus-5-5",
    "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
    "personal:anthropic/claude-opus-5",
    "gpt-5.4",
    "kimi-k3",
  ]) {
    for (const file of [PRODUCT, TEST]) {
      const found = run([rust(file, `let m = "${id}";`)]);
      assert.equal(found.length, 1, `${file}: ${id}`);
      assert.match(found[0], /model id/);
    }
  }
  // 不是模型 ID：中性的名字、请求方案的名字、`.opus` 扩展名、更长的词。
  for (const text of ['"test-model"', '"deepseek-official"', '".opus"', '"xglm-5"', '"glmx"'])
    assert.deepEqual(run([rust(PRODUCT, `let m = ${text};`)]), [], text);
  assert.deepEqual(run([rust(PRODUCT, "// 缺省是 glm-5.3\nlet m = MODEL;")]), []);
  assert.deepEqual(
    run([rust("crates/base/src/models.rs", 'pub const GLM_5_3: &str = "glm-5.3";')]),
    [],
  );
});

test("主机名：小写的已知主机与子域名报，品牌写法与长得像的不报", () => {
  for (const host of [
    "https://api.anthropic.com/v1",
    "https://api.z.ai/api/anthropic",
    "z.ai",
    "https://open.bigmodel.cn/api/anthropic",
    "https://api.deepseek.com/anthropic",
    "https://registry.npmjs.org",
    "https://zcode.z.ai",
    "https://eu.openrouter.ai/api/v1",
  ]) {
    const found = run([rust(TEST, `let u = "${host}";`)]);
    assert.equal(found.length, 1, host);
    assert.match(found[0], /host name/);
  }
  for (const text of ['"Waiting for Z.AI authorization"', '"https://gw.example.com"', '"xz.aix"'])
    assert.deepEqual(run([rust(PRODUCT, `let u = ${text};`)]), [], text);
  assert.deepEqual(
    run([rust("crates/base/src/hosts.rs", 'pub const ZAI_API: &str = "https://api.z.ai";')]),
    [],
  );
});

test("JS：字符串与模板串里的报，注释不算；脚本一侧的常量模块放行", () => {
  const js = (file, text) => ({ file, text });
  const found = run(
    [],
    [
      js(
        "scripts/rust-cli/node-interop-x.mjs",
        '// glm-5.3\nconst m = "glm-5.3";\nconst u = `https://api.z.ai/${p}`;\n',
      ),
    ],
  );
  assert.deepEqual(
    found.map((line) => line.split(":").slice(0, 2).join(":")),
    ["scripts/rust-cli/node-interop-x.mjs:2", "scripts/rust-cli/node-interop-x.mjs:3"],
  );
  assert.deepEqual(
    run(
      [],
      [
        js(
          "scripts/rust-cli/fixture/model.mjs",
          'export const GLM = "glm-5.3";\nexport const ZAI = "https://api.z.ai";',
        ),
      ],
    ),
    [],
  );
  // 只在本仓库的对齐脚本不查。
  assert.deepEqual(
    run(
      [],
      [js(`scripts/rust-cli/${["or", "acle"].join("")}/fixture.mjs`, 'const m = "glm-5.3";')],
    ),
    [],
  );
  assert.deepEqual(
    jsStringLiterals("const a = 'x'; /* \"y\" */ const b = `z\n${c}`;").map((l) => [
      l.line,
      l.value,
    ]),
    [
      [1, "x"],
      [1, "z\n${c}"],
    ],
  );
});

test("例外按文件与规则放行；登记了没用到的是死项；所有者里没有定义的也报", () => {
  const rules = {
    ...policy,
    VOLATILE_EXCEPTIONS: [
      { files: [TEST], rules: ["model id"], reason: "解析器的输入样本" },
      { files: ["crates/y/src/z_tests.rs"], rules: ["host name"], reason: "死项" },
    ],
  };
  const found = checkVolatileLiterals(
    rules,
    [
      rust(TEST, 'let m = "glm-5.3"; let u = "https://api.z.ai";'),
      rust("crates/base/src/hosts.rs", ""),
    ],
    [],
  );
  assert.equal(found.filter((line) => /model id/.test(line)).length, 0);
  assert.equal(found.filter((line) => line.startsWith(`${TEST}:1: host name`)).length, 1);
  assert.ok(found.some((line) => /z_tests\.rs has no host name literal \(dead entry/.test(line)));
  assert.ok(found.some((line) => /hosts\.rs defines no host name \(dead owner\)/.test(line)));
});

test("parity 测试不查", () => {
  for (const file of [
    "crates/model/src/anthropic_parity_tests.rs",
    "crates/cli/tests/parity_tools.rs",
    "crates/cli/tests/parity/fixture.rs",
  ])
    assert.deepEqual(run([rust(file, 'let m = "claude-opus-5-5";')]), [], file);
});

test("环境变量名：产品代码里报，测试与示例程序里不报；names.rs 与登记的例外放行", () => {
  const rules = {
    ...policy,
    VOLATILE_EXCEPTIONS: [
      { files: ["crates/x/src/signals.rs"], rules: ["env var name"], reason: "不是环境变量" },
    ],
  };
  const found = checkVolatileLiterals(
    rules,
    [
      rust(PRODUCT, 'let k = var("ZCODE_MODEL"); let p = var("_ZCODE_ASSUME_TOOL_SEARCH");'),
      rust(TEST, 'env.insert("ZCODE_MODEL", "x");'),
      rust("crates/x/examples/demo.rs", 'let k = var("ZCODE_MODEL");'),
      rust("crates/base/src/names.rs", 'pub const ZCODE_MODEL: &str = "ZCODE_MODEL";'),
      rust("crates/x/src/signals.rs", 'received("CTRL_C", 0)'),
    ],
    [],
  );
  const env = found.filter((line) => /env var name/.test(line));
  assert.equal(env.length, 2, env.join("\n"));
  assert.ok(env.every((line) => line.startsWith(`${PRODUCT}:1: env var name`)));
  // 单个词、小写、带别的字符的不算（样子认不出，收在 names::env 里由约定管）。
  for (const text of ['"PATH"', '"windir"', '"ZCODE-MODEL"', '"ZCODE_MODEL="', '"Zcode_Model"'])
    assert.deepEqual(
      run([rust(PRODUCT, `let k = ${text};`)]).filter((line) => /env var name/.test(line)),
      [],
      text,
    );
});
