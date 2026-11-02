// boundaries/constructors.mjs 的单测：每条规则一个合成的违规（工作线 F126）。
import assert from "node:assert/strict";
import { test } from "node:test";

import { checkConstructors, fullLiterals } from "./constructors.mjs";

const policy = (exceptions = []) => ({
  DTO_CONSTRUCTORS: {
    owner: "kernel-api",
    types: [
      { name: "Dto", hint: "Dto::new(a)" },
      { name: "Wire", hint: "Wire::new(a)", skipCrates: ["exec-wire"] },
      { name: "Transport::Stdio", hint: "Transport::stdio(command)", defaults: { env: ["None"] } },
    ],
  },
  DTO_CONSTRUCTOR_EXCEPTIONS: exceptions,
});
const source = (crate, text, file = `crates/${crate}/src/lib.rs`) => ({ crate, file, text });

test("完整字面量：owner 以外报错（带行号与该用的构造函数），owner 自己可以写", () => {
  const text = "fn f() -> u8 {\n    let x = Dto { a: 1, b: 2 };\n    0\n}\n";
  const failures = checkConstructors(policy(), [source("tools", text), source("kernel-api", text)]);
  assert.equal(failures.length, 1);
  assert.match(
    failures[0],
    /^crates\/tools\/src\/lib\.rs:2: a full Dto literal outside kernel-api; use Dto::new\(a\)/,
  );
});

test("不算构造：结构更新、剩余模式、let / match 的模式、定义、返回类型、注释与字符串", () => {
  const text = [
    "pub struct Dto {\n    a: u8,\n}",
    "impl Dto {\n    fn new(a: u8) -> Self { Self { a } }\n}",
    "fn g() -> Dto {\n    Dto { a: 1, ..Dto::new(0) }\n}",
    "fn h(x: Dto) {\n    let Dto { a, b } = x;\n    if matches!(x, Dto { a: 1, .. }) {}\n    match x {\n        Dto { a, b } => {}\n    }\n}",
    '// Dto { a: 1 }\nconst S: &str = "Dto { a: 1 }";',
    "fn k(t: Vec<(u8, u8)>) -> Dto {\n    Dto { a: Vec::<(u8, u8)>::new(), ..base() }\n}",
  ].join("\n");
  assert.deepEqual(checkConstructors(policy(), [source("tools", text)]), []);
});

test("skipCrates：有同名镜像类型的 crate 不查", () => {
  const text = "fn f() { let _ = Wire { a: 1 }; }\n";
  assert.deepEqual(checkConstructors(policy(), [source("exec-wire", text)]), []);
  assert.equal(checkConstructors(policy(), [source("tools", text)]).length, 1);
});

test("枚举变体：可省的字段都是缺省值时报错；带了别的值时构造函数写不出来，不报", () => {
  const plain = 'fn f() { let _ = Transport::Stdio { command: "x".to_owned(), env: None }; }\n';
  const custom = 'fn f() { let _ = Transport::Stdio { command: "x".to_owned(), env: Some(e) }; }\n';
  assert.equal(fullLiterals(plain, "Transport::Stdio", { env: ["None"] }).length, 1);
  assert.equal(checkConstructors(policy(), [source("tools", plain)]).length, 1);
  assert.deepEqual(checkConstructors(policy(), [source("tools", custom)]), []);
});

test("例外：登记的文件不报；登记了却没有命中是死项", () => {
  const text = "fn f() { let _ = Dto { a: 1 }; }\n";
  const used = [
    { files: ["crates/tools/src/lib.rs"], types: ["Dto"], reason: "执行管线装出上下文" },
  ];
  assert.deepEqual(checkConstructors(policy(used), [source("tools", text)]), []);
  const dead = [{ files: ["crates/tools/src/gone.rs"], types: ["Dto"], reason: "旧的" }];
  const failures = checkConstructors(policy(dead), [source("tools", "fn f() {}\n")]);
  assert.equal(failures.length, 1);
  assert.match(
    failures[0],
    /DTO_CONSTRUCTOR_EXCEPTIONS: crates\/tools\/src\/gone\.rs has no full Dto literal \(dead entry/,
  );
});
