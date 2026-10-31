// 散写数字的棘轮（docs/specs/rust-cli/constants.md §4）：
//   node --test scripts/rust-cli/boundaries/numbers.test.mjs
// 每条“不算的写法”一个正例、一个反例；范围、判定、--update 与推送前的“不许升高”各自几个用例。只用合成的源码。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  checkNumbers,
  checkRaises,
  countNumbers,
  formatBaseline,
  hitsIn,
  isScanned,
  testModuleFiles,
  updateBaseline,
} from "./numbers.mjs";

/** 一段代码里被算的数字（只看值的原文）。 */
function counted(code, allowed = []) {
  return hitsIn(code, allowed).map((hit) => hit.text);
}

test("0 与 1 不算，其余数字算", () => {
  assert.deepEqual(counted("let a = 0; let b = 1; let c = 2u8; let d = 0.0; let e = 1.0;"), [
    "2u8",
  ]);
  assert.deepEqual(counted("let x = 0u8 + 0x1 + 0b1 + 0o0;"), []);
  assert.deepEqual(counted("let x = 3; let y = -1;"), ["3"]);
});

test("数字的各种写法都认", () => {
  assert.deepEqual(counted("f(1_000, 0x7f, 0o600, 1e3, 2.5f64, 10_u64, 0b1010, 1.5e-3, 0xFFu8);"), [
    "1_000",
    "0x7f",
    "0o600",
    "1e3",
    "2.5f64",
    "10_u64",
    "0b1010",
    "1.5e-3",
    "0xFFu8",
  ]);
  // 标识符里的数字不是字面量。
  assert.deepEqual(counted("let u64_max = sha256(x2); let v = V3;"), []);
});

test("元组字段 .0 .1 .12 不算；范围的右端算", () => {
  assert.deepEqual(counted("let a = pair.0 + pair.1 + big.12; let b = t.0.1;"), []);
  assert.deepEqual(counted("for i in 0..5 {} let r = 2..=9;"), ["5", "2", "9"]);
});

test("数组长度不算，vec! 的元素个数算", () => {
  assert.deepEqual(counted("let a: [u8; 32] = [0; 16];"), []);
  assert.deepEqual(counted("let b = vec![0u8; 64 * 1024];"), ["64", "1024"]);
});

test("位移量不算，掩码算", () => {
  assert.deepEqual(counted("let a = x >> 8; let b = y << 16; x <<= 4; z.rotate_left(5);"), []);
  assert.deepEqual(counted("let c = x & 0xff;"), ["0xff"]);
});

test("2 作乘除取余的操作数不算，加减与比较里的 2 算", () => {
  assert.deepEqual(counted("let h = len / 2; let d = 2 * n; let p = n % 2; n *= 2;"), []);
  assert.deepEqual(counted("let q = n.is_multiple_of(2) && x.pow(2) > y.powi(2);"), []);
  assert.deepEqual(counted("let t = len / 3; i += 2; let ok = x.len() > 2;"), ["3", "2", "2"]);
});

test("下标与切片里的一位数不算，两位数与别的方法参数算", () => {
  assert.deepEqual(
    counted(
      "a[2]; a[3..]; a[..4]; a[i..5]; a[i + 2]; a[i - 3]; a[..=6]; row.get(3); it.nth(2); r.get::<_, Option<String>>(4)?;",
    ),
    [],
  );
  assert.deepEqual(counted("a[i + 12]; header[156]; it.take(3); a[10..];"), [
    "12",
    "156",
    "3",
    "10",
  ]);
});

test("注释与字符串里的数字不算", () => {
  assert.deepEqual(counted("// 30 秒\nlet s = \"port 8080\"; /* 99 */ let c = '7';"), []);
});

test("测试条目、属性与常量的右边不算；const fn 的函数体与 not(test) 的条目算", () => {
  const code = [
    "#[cfg(test)]",
    "mod tests { fn t() { let x = 42; } }",
    '#[cfg(any(test, feature = "testing"))]',
    "fn helper() -> u32 { 77 }",
    "#[cfg(not(test))]",
    "fn real() -> u32 { 5 }",
    "#[serde(default = 30)]",
    "const LIMIT: u64 = 30 * 1000;",
    "pub(crate) static TABLE: [u8; 3] = [9, 8, 7];",
    "impl X { const STEP: usize = 64; }",
    "const fn twice() -> u32 { 21 }",
    "fn generic<const N: usize>() -> usize { N + 3 }",
  ].join("\n");
  assert.deepEqual(counted(code), ["5", "21", "3"]);
});

test("NUMBER_ALLOWED 的模式全局放行，只放行值与位置都对得上的", () => {
  const allowed = [{ value: 10, before: /\.pow\(\s*$/, reason: "十进制的幂" }];
  assert.deepEqual(counted("let a = 10u64.pow(3); let b = x.pow(10);", allowed), ["10u64", "3"]);
});

test("扫描范围：只看 crates/*/src，测试文件、kernel-api 的 testing 与 cfg(test) 引进的文件不扫", () => {
  const sources = [
    {
      file: "crates/a/src/lib.rs",
      text: '#[cfg(test)]\nmod extra;\n#[cfg(test)]\n#[path = "x_check.rs"]\nmod x;\nmod real;',
    },
    { file: "crates/a/src/extra.rs", text: "fn a() { 5; } mod nested;" },
    { file: "crates/a/src/extra/nested.rs", text: "fn b() { 6; }" },
    { file: "crates/a/src/x_check.rs", text: "fn c() { 7; }" },
    { file: "crates/a/src/real.rs", text: "fn d() { 8; }" },
  ];
  const tests = testModuleFiles(sources);
  assert.deepEqual([...tests].sort(), [
    "crates/a/src/extra.rs",
    "crates/a/src/extra/nested.rs",
    "crates/a/src/x_check.rs",
  ]);
  for (const file of ["crates/a/src/lib.rs", "crates/a/src/real.rs", "crates/a/src/sub/mod.rs"])
    assert.ok(isScanned(file, tests), file);
  for (const file of [
    "crates/a/src/extra.rs",
    "crates/a/src/foo_tests.rs",
    "crates/a/src/foo_tests_more.rs",
    "crates/a/src/test_support.rs",
    "crates/a/src/test_more.rs",
    "crates/kernel-api/src/testing.rs",
    "crates/kernel-api/src/testing/store.rs",
    "crates/a/tests/it.rs",
    "crates/a/examples/demo.rs",
    "crates/a/build.rs",
  ])
    assert.ok(!isScanned(file, tests), file);
  const counts = countNumbers(sources);
  assert.deepEqual(Object.fromEntries([...counts].map(([f, h]) => [f, h.length])), {
    "crates/a/src/lib.rs": 0,
    "crates/a/src/real.rs": 1,
  });
});

const counts = (entries) =>
  new Map(
    Object.entries(entries).map(([file, values]) => [
      file,
      values.map((value, i) => ({ line: i + 1, text: String(value) })),
    ]),
  );
const baseline = (files) => ({
  total: Object.values(files).reduce((a, b) => a + b, 0),
  files,
});

test("判定：与基线相等通过；多了失败并列出行号；少了提示 --update；过期项与新文件", () => {
  const current = counts({ "crates/a/src/x.rs": [30, 60], "crates/a/src/y.rs": [] });
  assert.deepEqual(checkNumbers(current, baseline({ "crates/a/src/x.rs": 2 })), []);
  const more = checkNumbers(current, baseline({ "crates/a/src/x.rs": 1 }));
  assert.equal(more.length, 1);
  assert.match(more[0], /crates\/a\/src\/x\.rs: 2 numeric literals, baseline 1/);
  assert.match(more[0], /crates\/a\/src\/x\.rs:1: 30/);
  assert.match(more[0], /crates\/a\/src\/x\.rs:2: 60/);
  const fewer = checkNumbers(current, baseline({ "crates/a/src/x.rs": 3 }));
  assert.match(fewer.join("\n"), /baseline 3.*--update/s);
  const stale = checkNumbers(
    current,
    baseline({ "crates/a/src/gone.rs": 1, "crates/a/src/x.rs": 2 }),
  );
  assert.match(stale.join("\n"), /gone\.rs: stale baseline entry/);
  const fresh = checkNumbers(counts({ "crates/a/src/new.rs": [5] }), baseline({}));
  assert.match(fresh.join("\n"), /new\.rs: 1 numeric literals, baseline 0/);
  // 0 处的文件不写进基线。
  assert.match(
    checkNumbers(counts({ "crates/a/src/y.rs": [] }), baseline({ "crates/a/src/y.rs": 0 })).join(
      "\n",
    ),
    /baseline 0 must not be listed/,
  );
});

test("判定：total 不等于各项之和、键没排序都失败；输出排序稳定", () => {
  const current = counts({ "crates/a/src/x.rs": [3], "crates/b/src/y.rs": [4] });
  assert.match(
    checkNumbers(current, {
      total: 5,
      files: { "crates/a/src/x.rs": 1, "crates/b/src/y.rs": 1 },
    }).join("\n"),
    /total 5 != 2/,
  );
  assert.match(
    checkNumbers(current, {
      total: 2,
      files: { "crates/b/src/y.rs": 1, "crates/a/src/x.rs": 1 },
    }).join("\n"),
    /not sorted/,
  );
  const failures = checkNumbers(current, baseline({}));
  assert.deepEqual(failures, [...failures].sort());
});

test("--update 只降不升：取较小值、删过期项与 0、不写新文件，重写 total 与键序", () => {
  const current = counts({
    "crates/a/src/x.rs": [3],
    "crates/a/src/y.rs": [4, 5, 6],
    "crates/a/src/z.rs": [],
    "crates/a/src/new.rs": [7],
  });
  const next = updateBaseline(
    current,
    baseline({
      "crates/a/src/y.rs": 2,
      "crates/a/src/x.rs": 2,
      "crates/a/src/z.rs": 1,
      "crates/a/src/gone.rs": 4,
    }),
  );
  assert.deepEqual(next, { total: 3, files: { "crates/a/src/x.rs": 1, "crates/a/src/y.rs": 2 } });
  assert.deepEqual(Object.keys(next.files), ["crates/a/src/x.rs", "crates/a/src/y.rs"]);
  // 没有基线文件时写入现状（第一次建立）。
  assert.deepEqual(updateBaseline(current, null), {
    total: 5,
    files: { "crates/a/src/new.rs": 1, "crates/a/src/x.rs": 1, "crates/a/src/y.rs": 3 },
  });
  // 登记的升高（NUMBER_RAISES）：基线等于 was 时上限是 now。
  const raised = updateBaseline(current, baseline({ "crates/a/src/x.rs": 1 }), [
    { file: "crates/a/src/new.rs", was: 0, now: 1, reason: "拆文件" },
    { file: "crates/a/src/y.rs", was: 2, now: 3, reason: "不会生效：基线里是 0" },
  ]);
  assert.deepEqual(raised.files, { "crates/a/src/new.rs": 1, "crates/a/src/x.rs": 1 });
});

test("基线的文本：两格缩进、一个文件一行、末尾换行", () => {
  assert.equal(
    formatBaseline(baseline({ "crates/a/src/x.rs": 2 })),
    '{\n  "total": 2,\n  "files": {\n    "crates/a/src/x.rs": 2\n  }\n}\n',
  );
});

test("推送前不许升高：变大与新文件失败，登记了就放行，登记的旧值不符时失败", () => {
  const before = baseline({ "crates/a/src/x.rs": 2, "crates/a/src/y.rs": 5 });
  assert.deepEqual(checkRaises(before, baseline({ "crates/a/src/x.rs": 1 }), []), []);
  const up = checkRaises(before, baseline({ "crates/a/src/x.rs": 3, "crates/a/src/y.rs": 5 }), []);
  assert.match(up.join("\n"), /x\.rs: 2 -> 3 is not registered in NUMBER_RAISES/);
  const fresh = checkRaises(before, baseline({ ...before.files, "crates/a/src/n.rs": 1 }), []);
  assert.match(fresh.join("\n"), /n\.rs: 0 -> 1/);
  const registered = [
    { file: "crates/a/src/x.rs", was: 2, now: 3, reason: "用户决定" },
    { file: "crates/a/src/n.rs", was: 0, now: 1, reason: "从 y.rs 拆出" },
  ];
  assert.deepEqual(
    checkRaises(
      before,
      baseline({ "crates/a/src/n.rs": 1, "crates/a/src/x.rs": 3, "crates/a/src/y.rs": 4 }),
      registered,
    ),
    [],
  );
  const wrongWas = checkRaises(
    before,
    baseline({ "crates/a/src/x.rs": 3, "crates/a/src/y.rs": 5 }),
    [{ file: "crates/a/src/x.rs", was: 1, now: 3, reason: "旧值写错" }],
  );
  assert.match(wrongWas.join("\n"), /registered was 1, push base has 2/);
  const tooHigh = checkRaises(
    before,
    baseline({ "crates/a/src/x.rs": 4, "crates/a/src/y.rs": 5 }),
    registered,
  );
  assert.match(tooHigh.join("\n"), /2 -> 4.*registered now 3/s);
});
