// 厂商错误解耦检查的测试（docs/specs/rust-cli/provider-errors.md §2.1、§2.5）：
//   node --test scripts/rust-cli/boundaries/vendor.test.mjs
// 用合成的源码：先确认合成的基线没有违规，再逐项加一个违规，证明每条规则会失败。最后一个用例读真实的 errors/zhipu.rs，
// 比对规则表里的码与分类器的码表，防止两边漂移。
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { test } from "node:test";

import { stringLiterals } from "./check.mjs";
import * as real from "./policy.mjs";
import { checkVendorTerms } from "./vendor.mjs";

const policy = { ...real, VENDOR_ERROR_TERM_EXCEPTIONS: [] };

const ZHIPU = "crates/model/src/errors/zhipu.rs";

/** 合成的基线：登记的值都在允许的位置；别处只有不算的写法。 */
function sources(extra = []) {
  return [
    {
      crate: "model",
      file: ZHIPU,
      text: [
        "//! 智谱的码，例如 1302（注释里的码不算）。",
        'const CODES: [(&str, u8); 2] = [("1302", 1), ("1113", 2)];',
        'fn finish(raw: &str) -> bool { matches!(raw, "sensitive" | "network_error") }',
        "const RAW: u32 = 1302;",
        "fn vendor() -> Vendor { Vendor::Zhipu }",
      ].join("\n"),
    },
    {
      crate: "model",
      file: "crates/model/src/errors/zhipu_tests.rs",
      text: 'assert_eq!(code(&json!({"code": 1302})), Some("1302".into()));\n',
    },
    {
      crate: "model",
      file: "crates/model/src/errors/openai.rs",
      text: 'match code { "insufficient_quota" | "context_length_exceeded" => {}, _ => {} }\n',
    },
    {
      crate: "model",
      file: "crates/model/src/errors/mod.rs",
      text: "match vendor { Vendor::Zhipu => zhipu::response(), Vendor::Generic => None }\n",
    },
    {
      crate: "model",
      file: "crates/model/src/profiles.rs",
      text: "if zhipu { Vendor::Zhipu } else { Vendor::Generic }\n",
    },
    {
      crate: "model",
      file: "crates/model/src/profiles_tests.rs",
      text: "assert_eq!(vendor(URL), Vendor::Zhipu);\n",
    },
    {
      crate: "model",
      file: "crates/model/src/profiles_select_tests.rs",
      text: "assert_eq!(vendor(URL), Vendor::Zhipu);\n",
    },
    {
      crate: "model",
      file: "crates/model/src/model_io_reply.rs",
      // 带连字符的写法与更长的字面量不算；注释里的词与码不算。
      text: [
        'let reason = "content-filter"; // content_filter 与 1302 都只是说明',
        'let key = "content_filter_results";',
        "/* insufficient_quota 1113 */ let n = 0x1302;",
      ].join("\n"),
    },
    {
      crate: "net",
      file: "crates/net/src/websocket.rs",
      text: "let code = 1006; let other: u16 = 1005;\n",
    },
    {
      crate: "env-local",
      file: "crates/env-local/src/shell/socket.rs",
      text: "Close { code: 1006, reason: String::new() }\n",
    },
    {
      crate: "query",
      file: "crates/query/src/recovery.rs",
      text: "match error { ModelError::QuotaExhausted { .. } => 1, _ => 2 }\nlet port = 13020;\nlet f = 1302.5;\n",
    },
    ...extra,
  ];
}

const check = (extra, over = {}) => checkVendorTerms({ ...policy, ...over }, sources(extra));

test("合成的基线没有违规", () => {
  assert.deepEqual(check([]), []);
});

test("消息的格式：文件、行号、找到的值、类别与 id、允许的位置", () => {
  const failures = check([
    { crate: "query", file: "crates/query/src/bad.rs", text: 'let a = 1;\nlet b = "sensitive";\n' },
  ]);
  assert.deepEqual(failures, [
    `crates/query/src/bad.rs:2: "sensitive" is a vendor error value (zhipu-value); only the vendor classifier (${ZHIPU}) may know it (provider-errors.md §2.5)`,
  ]);
});

test("规则 1：智谱的结束原因在分类器文件之外报错（errors/ 里的别的文件也算之外）", () => {
  for (const [file, crate] of [
    ["crates/model/src/transport.rs", "model"],
    ["crates/model/src/errors/mod.rs", "model"],
    ["crates/model/src/errors/dispose.rs", "model"],
    ["crates/query/src/recovery_more.rs", "query"],
    ["crates/cli/src/main.rs", "cli"],
  ]) {
    for (const value of ["sensitive", "network_error"]) {
      const failures = check([{ crate, file, text: `let r = "${value}";\n` }]);
      assert.ok(
        failures.some(
          (line) => line.startsWith(`${file}:1: "${value}"`) && line.includes("zhipu-value"),
        ),
        `${file} ${value}: ${failures.join("\n")}`,
      );
    }
  }
});

test("规则 1：智谱的业务码作为字符串字面量在分类器文件之外报错（任何 crate）", () => {
  for (const [file, crate] of [
    ["crates/model/src/errors/anthropic.rs", "model"],
    ["crates/query/src/texts.rs", "query"],
    ["crates/net/src/retry.rs", "net"],
  ]) {
    const failures = check([{ crate, file, text: 'if code == "1302" { return; }\n' }]);
    assert.ok(
      failures.some((line) => line.startsWith(`${file}:1: "1302"`) && line.includes("zhipu-code")),
      `${file}: ${failures.join("\n")}`,
    );
  }
  // 原始字符串也算；`500` 不在表里（与 HTTP 状态同名）。
  const raw = check([
    {
      crate: "query",
      file: "crates/query/src/raw.rs",
      text: 'let a = r#"20097"#;\nlet b = "500";\n',
    },
  ]);
  assert.equal(raw.length, 1);
  assert.ok(raw[0].startsWith('crates/query/src/raw.rs:1: "20097"'));
});

test("规则 1：处理模型错误的 crate 里，代码中的数字码报错（带后缀与下划线也算）", () => {
  for (const [crate, text] of [
    ["model", "if code == 1302 { retry(); }\n"],
    ["query", "const LIMIT: u16 = 1113u16;\n"],
    ["session", "let c = 1_302;\n"],
    ["kernel-api", "match n { 3008..=3010 => true, _ => false }\n"],
    ["protocol-zcode", "let codes = [1261, 1];\n"],
  ]) {
    const file = `crates/${crate}/src/codes.rs`;
    const failures = check([{ crate, file, text }]);
    assert.ok(
      failures.some((line) => line.startsWith(`${file}:1:`) && line.includes("zhipu-code")),
      `${crate} ${JSON.stringify(text)}: ${failures.join("\n")}`,
    );
  }
  // 码的数字在分类器文件里不算。
  assert.deepEqual(
    check([{ crate: "model", file: ZHIPU, text: "const X: u32 = 1302;\n" }]).filter((line) =>
      line.startsWith(ZHIPU),
    ),
    [],
  );
});

test("规则 1：数字码只查处理模型错误的 crate；net、env-local 的 WebSocket 关闭码不算；注释、十六进制、浮点数不算", () => {
  assert.deepEqual(
    check([
      { crate: "net", file: "crates/net/src/close.rs", text: "let code = 1006; let x = 1302;\n" },
      { crate: "env-local", file: "crates/env-local/src/x.rs", text: "Close { code: 1005 }\n" },
      {
        crate: "kernel-api",
        file: "crates/kernel-api/src/env_shell.rs",
        text: "/// 没有 close 帧时是 1006 与错误说明。\npub code: u16,\n",
      },
      {
        crate: "tui",
        file: "crates/tui/src/x.rs",
        text: "let a = 0x1302; let b = 1302.0; let c = width.1302; let d = v1302;\n",
      },
    ]),
    [],
  );
});

test("规则 1：选厂商的写法 Vendor::Zhipu 只在 errors/ 与 profiles.rs（测试文件也查）", () => {
  for (const [file, crate, text] of [
    ["crates/model/src/transport.rs", "model", "if vendor == Vendor::Zhipu { wait(); }\n"],
    [
      "crates/model/src/anthropic_stream.rs",
      "model",
      "use crate::errors::Vendor::{Generic, Zhipu};\n",
    ],
    ["crates/query/src/run.rs", "query", "let v = Vendor :: Zhipu;\n"],
    // 测试文件不豁免这一条。
    ["crates/model/src/transport_read_tests.rs", "model", "let v = Vendor::Zhipu;\n"],
    ["crates/model/tests/suite/zhipu.rs", "model", "let v = Vendor::Zhipu;\n"],
    // 同名前缀的产品文件不是 profiles.rs 的测试文件。
    ["crates/model/src/profiles_more.rs", "model", "let v = Vendor::Zhipu;\n"],
  ]) {
    const failures = check([{ crate, file, text }]);
    assert.ok(
      failures.some((line) => line.startsWith(`${file}:1:`) && line.includes("zhipu-selector")),
      `${file}: ${failures.join("\n")}`,
    );
  }
  // 注释与字符串里的这个写法不算；别的厂商名不算。
  assert.deepEqual(
    check([
      {
        crate: "model",
        file: "crates/model/src/transport.rs",
        text: '// 按 Vendor::Zhipu 选\nlet s = "Vendor::Zhipu";\nlet v = Vendor::Generic;\n',
      },
    ]),
    [],
  );
});

test("规则 2：协议的错误词在 errors/ 之外报错（每组一个）", () => {
  for (const [value, id] of [
    ["insufficient_quota", "quota"],
    ["credit_balance_exhausted", "quota"],
    ["content_filter", "content-filter"],
    ["overloaded_error", "overloaded"],
    ["rate_limit_exceeded", "rate-limit"],
    ["request_too_large", "too-large"],
    ["model_context_window_exceeded", "context-overflow"],
  ]) {
    for (const [file, crate] of [
      ["crates/model/src/transport.rs", "model"],
      ["crates/model/src/openai_chat_stream.rs", "model"],
      ["crates/query/src/recovery.rs", "query"],
      ["crates/kernel-api/src/message.rs", "kernel-api"],
    ]) {
      const failures = check([{ crate, file, text: `let a = 1;\nlet t = "${value}";\n` }]);
      assert.ok(
        failures.some(
          (line) =>
            line.startsWith(`${file}:2: "${value}" is a protocol error value (${id})`) &&
            line.includes("(crates/model/src/errors/)"),
        ),
        `${file} ${value}: ${failures.join("\n")}`,
      );
    }
  }
  // errors/ 里的任何文件都可以（含第 2 层的 dispose.rs 之外的分类器文件与 vocab.rs）。
  assert.deepEqual(
    check([
      {
        crate: "model",
        file: "crates/model/src/errors/vocab.rs",
        text: 'const Q: [&str; 2] = ["billing_error", "rate_limit_error"];\n',
      },
    ]),
    [],
  );
});

test("测试文件不查值、码与协议词（crate 的 tests/ 目录、_tests.rs、_tests_*.rs、test_support.rs、test_more.rs）", () => {
  const text =
    'let a = "overloaded_error";\nlet b = "1302";\nlet c = 1302;\nlet d = "sensitive";\n';
  assert.deepEqual(
    check(
      [
        "crates/model/tests/suite/failover.rs",
        "crates/cli/tests/zhipu.rs",
        "crates/model/src/openai_chat_stream_tests.rs",
        "crates/query/src/run_tests_more.rs",
        "crates/query/src/test_support.rs",
        "crates/session/src/test_more.rs",
      ].map((file) => ({ crate: file.split("/")[1], file, text })),
    ),
    [],
  );
  // 同名的产品文件照查。
  const product = check([{ crate: "query", file: "crates/query/src/support.rs", text }]);
  assert.equal(product.length, 4);
});

test("例外只按文件与精确片段放过，没用到的例外是死项", () => {
  const file = "crates/query/src/legacy.rs";
  const extra = [{ crate: "query", file, text: 'let r = "sensitive";\n' }];
  const exception = { file, fragment: 'let r = "sensitive"', why: "测试" };
  assert.deepEqual(check(extra, { VENDOR_ERROR_TERM_EXCEPTIONS: [exception] }), []);
  // 同一个片段在别的文件里不放过。
  const elsewhere = check(
    [
      ...extra,
      { crate: "query", file: "crates/query/src/other.rs", text: 'let r = "sensitive";\n' },
    ],
    { VENDOR_ERROR_TERM_EXCEPTIONS: [exception] },
  );
  assert.ok(elsewhere.some((line) => line.startsWith("crates/query/src/other.rs:1")));
  const dead = check(extra, {
    VENDOR_ERROR_TERM_EXCEPTIONS: [{ ...exception, fragment: "不存在的片段" }],
  });
  assert.ok(
    dead.some(
      (line) => line.includes("VENDOR_ERROR_TERM_EXCEPTIONS") && line.includes("dead entry"),
    ),
  );
  assert.ok(dead.some((line) => line.startsWith(`${file}:1: "sensitive"`)));
});

test("规则表：同一个值只登记一处", () => {
  const all = [
    ...real.VENDOR_ERROR_TERMS.vendors.flatMap((vendor) => [...vendor.values, ...vendor.codes]),
    ...real.VENDOR_ERROR_TERMS.protocol.groups.flatMap((group) => group.values),
  ];
  assert.deepEqual(
    all.filter((value, index) => all.indexOf(value) !== index),
    [],
  );
});

test("规则表的智谱码 = errors/zhipu.rs 的 CODES 去掉 500（防漂移）", async () => {
  const path = resolve(import.meta.dirname, "../../../packages/rust-cli", ZHIPU);
  const source = await readFile(path, "utf8");
  const head = source.indexOf("const CODES");
  assert.notEqual(head, -1, `${ZHIPU}: const CODES not found`);
  const table = source.slice(head, source.indexOf("];", head));
  const codes = stringLiterals(table)
    .map(({ value }) => value)
    .filter((value) => /^\d+$/.test(value) && value !== "500");
  const zhipu = real.VENDOR_ERROR_TERMS.vendors.find((vendor) => vendor.id === "zhipu");
  const sort = (list) => [...list].sort((a, b) => Number(a) - Number(b));
  assert.deepEqual(sort(zhipu.codes), sort(codes));
  // 结束原因也要在分类器里（改名或删掉时一起改规则表）。
  for (const value of zhipu.values) {
    assert.ok(
      stringLiterals(source).some((literal) => literal.value === value),
      `${ZHIPU} has no "${value}"`,
    );
  }
});
