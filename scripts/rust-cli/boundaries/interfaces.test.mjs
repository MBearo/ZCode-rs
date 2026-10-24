// 接口快照的测试（docs/specs/rust-cli/remote-exec.md §15.6、§18.7）：
//   node --test scripts/rust-cli/boundaries/interfaces.test.mjs
// 只用合成的源码：trait 的方法（有没有缺省实现）、线上结构的字段 / 变体与 serde 属性、常量、帧的编号；改动被发现。
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  checkInterfaces,
  extractFrameCodes,
  extractTrait,
  extractWire,
  snapshot,
  stripComments,
} from "./interfaces.mjs";

const PORT = `
/// 文件。
#[async_trait]
pub trait Files: Send + Sync {
    async fn read_file(
        &self,
        workspace: &WorkspaceRef, // 注释不算
        request: ReadFile,
    ) -> Result<FileContent, EnvError>;

    /// 有缺省实现。
    async fn stat(&self, _request: StatFile) -> Result<Option<FileStat>, EnvError> {
        Err(not_provided("stat { }"))
    }

    fn watch(&self, cursor: u64) -> BoxStream<'static, Result<Event, EnvError>>;
}
`;

const WIRE = `
pub const PROTOCOL: u32 = 1;
/// 一段。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Plain,
    #[serde(rename = "x.y")]
    Named { path: String, #[serde(default)] size: u64 },
    Wrapped(Vec<u8>),
}
pub struct Pair(BTreeSet<String>);
pub struct Plain {
    pub a: Option<u64>,
}
impl Kind {
    pub fn code(self) -> u8 {
        match self {
            Self::Hello => 0x01,
            Self::Reply => 0x11,
        }
    }
}
`;

test("trait：方法签名去掉空白与注释，记下有没有缺省实现；方法体里的大括号不干扰", () => {
  const trait = extractTrait(PORT, "Files");
  assert.equal(trait.bounds, ":Send + Sync");
  assert.deepEqual(
    trait.methods.map((method) => [method.name, method.default]),
    [
      ["read_file", false],
      ["stat", true],
      ["watch", false],
    ],
  );
  assert.equal(
    trait.methods[0].signature,
    "async fn read_file(&self,workspace:&WorkspaceRef,request:ReadFile,) -> Result<FileContent,EnvError>",
  );
  assert.equal(extractTrait(PORT, "Missing"), null);
});

test("线上结构：字段、变体、serde 属性、元组结构体与常量；帧的编号", () => {
  const items = extractWire(WIRE);
  assert.deepEqual(items.PROTOCOL, { kind: "const", value: ":u32 = 1" });
  assert.deepEqual(items.Item.attributes, ['tag = "kind",rename_all = "snake_case"']);
  assert.deepEqual(items.Item.variants, [
    { item: "Plain" },
    { attributes: ['rename = "x.y"'], item: "Named{path:String,#[serde(default)]size:u64}" },
    { item: "Wrapped(Vec<u8>)" },
  ]);
  assert.equal(items.Pair.tuple, "(BTreeSet<String>)");
  assert.deepEqual(items.Plain.fields, [{ item: "a:Option<u64>" }]);
  assert.deepEqual(extractFrameCodes(WIRE), { Hello: "0x01", Reply: "0x11" });
  assert.equal(stripComments('let a = "// 不是注释"; // 是注释'), 'let a = "// 不是注释"; ');
});

test("快照：没变时通过；端口或线上结构变了、端口不见了都报出", () => {
  const sources = {
    "crates/kernel-api/src/machine.rs": PORT,
    "crates/exec-wire/src/frame.rs": WIRE,
    "crates/exec-wire/src/lib_tests.rs": "pub struct Ignored;",
  };
  const committed = JSON.parse(JSON.stringify(snapshot(sources)));
  assert.equal(committed.wire["frame.rs"].Plain.kind, "struct");
  assert.equal(committed.wire["lib_tests.rs"], undefined);
  const missing = checkInterfaces(sources, committed);
  // machine.rs 只有 Files：别的端口找不到。
  assert.ok(missing.some((line) => line.includes("ports.EnvInfo is not found")));
  const changed = {
    ...sources,
    "crates/exec-wire/src/frame.rs": WIRE.replace("pub a: Option<u64>", "pub a: u64"),
  };
  const failures = checkInterfaces(changed, committed).filter(
    (line) => !line.includes("not found"),
  );
  assert.deepEqual(failures, [
    "interfaces: wire.frame.rs changed; update the snapshot in the same commit (node scripts/rust-cli/boundaries/interfaces.mjs --update)",
  ]);
});
