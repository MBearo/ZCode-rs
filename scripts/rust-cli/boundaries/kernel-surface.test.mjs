// kernel-api 只加不改的测试（docs/specs/rust-cli/app-protocol.md §9.4）：
//   node --test scripts/rust-cli/boundaries/kernel-surface.test.mjs
// 只用合成的源码：提取的项（trait 方法、结构体字段、枚举变体、函数、常量、impl、重新导出、宏）；新增要记进快照，
// 改动与删除不登记就失败；--update 只追加。
import assert from "node:assert/strict";
import { test } from "node:test";

import { appendAdditions, checkSurface, extractSurface, surfaceOf } from "./kernel-surface.mjs";

const SESSION_API = `//! 会话 API。
use serde::{Deserialize, Serialize};
pub use crate::ids::SessionId;

/// 命令。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Submit { text: String, delivery: Delivery },
    Interrupt,
}

pub struct CreateSession {
    pub id: Option<SessionId>,
    #[serde(default)]
    pub plan: bool,
    secret: u32,
}

pub struct Pair(pub u32, pub String);

#[async_trait]
pub trait SessionHandle: Send + Sync {
    fn id(&self) -> &SessionId;
    async fn send(&self, command: CommandEnvelope) -> Ack;
    async fn goal(&self) -> Option<Goal> {
        None
    }
}

impl CreateSession {
    pub fn new(id: SessionId) -> Self {
        let text = "{ not a block }";
        Self { id: Some(id), plan: false, secret: 0 }
    }
    fn private(&self) {}
}

impl Default for Pair {
    fn default() -> Self { Pair(0, String::new()) }
}

pub const LIMIT: usize = 64;
pub type Events = Vec<KernelEvent>;
pub fn parse(text: &str) -> Result<Command, String> where Command: Sized { todo() }
pub(crate) fn hidden() {}
fn private() {}

string_id!(
    /// 文档注释不算。
    CommandId
);

#[cfg(test)]
mod tests {
    pub fn ignored() {}
}
`;

const sources = (text = SESSION_API) => ({
  "crates/kernel-api/src/session_api.rs": text,
  "crates/kernel-api/src/session_api_tests.rs": "pub fn ignored() {}",
  "crates/kernel-api/src/testing.rs": "pub struct Fake;",
  "crates/kernel-api/src/testing/run.rs": "pub struct AlsoFake;",
});

test("提取：公开的项逐个成键；私有项、测试文件、testing 替身、文档注释与 cfg(test) 不算", () => {
  const items = extractSurface(SESSION_API, "session_api");
  const keys = Object.keys(items);
  for (const key of [
    "session_api::use crate::ids::SessionId",
    "session_api::Command",
    "session_api::Command::Submit",
    "session_api::Command::Interrupt",
    "session_api::CreateSession",
    "session_api::CreateSession.id",
    "session_api::CreateSession.plan",
    "session_api::Pair",
    "session_api::SessionHandle",
    "session_api::SessionHandle::id",
    "session_api::SessionHandle::send",
    "session_api::SessionHandle::goal",
    "session_api::CreateSession::new",
    "session_api::impl Default for Pair",
    "session_api::LIMIT",
    "session_api::Events",
    "session_api::parse",
    "session_api::string_id!(CommandId)",
  ]) {
    assert.ok(keys.includes(key), `missing ${key}; got\n${keys.join("\n")}`);
  }
  for (const absent of ["secret", "private", "hidden", "ignored"]) {
    assert.ok(!keys.some((key) => key.endsWith(absent)), `${absent} should not be extracted`);
  }
  assert.equal(items["session_api::CreateSession.plan"], "#[serde(default)] plan:bool");
  assert.equal(
    items["session_api::SessionHandle::goal"],
    "async fn goal(&self) -> Option<Goal> {default}",
  );
  assert.equal(items["session_api::Command::Submit"], "Submit{text:String,delivery:Delivery}");
  assert.match(items["session_api::Command"], /derive\(Debug,Clone,Serialize,Deserialize\)/);
  const surface = surfaceOf(sources());
  assert.ok(
    !Object.keys(surface).some((key) => key.startsWith("testing") || key.includes("_tests")),
  );
});

test("检查：没变时通过；新增要记进快照（--update），改动与删除不登记就失败", () => {
  const committed = surfaceOf(sources());
  assert.deepEqual(checkSurface(committed, committed, []), []);

  const added = surfaceOf(
    sources(SESSION_API.replace("    Interrupt,", "    Interrupt,\n    Shutdown,")),
  );
  const addition = checkSurface(added, committed, []);
  assert.equal(addition.length, 1);
  assert.match(addition[0], /session_api::Command::Shutdown is new.*--update/);
  // --update 只追加：新增进快照，之后通过。
  const updated = appendAdditions(added, committed);
  assert.deepEqual(checkSurface(added, updated, []), []);

  const changed = surfaceOf(
    sources(
      SESSION_API.replace(
        "async fn send(&self, command: CommandEnvelope) -> Ack;",
        "async fn send(&self, command: CommandEnvelope, urgent: bool) -> Ack;",
      ),
    ),
  );
  const change = checkSurface(changed, committed, []);
  assert.equal(change.length, 1);
  assert.match(change[0], /session_api::SessionHandle::send changed/);
  // --update 不改已有的项：改动仍然失败。
  assert.deepEqual(checkSurface(changed, appendAdditions(changed, committed), []), change);

  const removed = surfaceOf(sources(SESSION_API.replace("    pub plan: bool,\n", "")));
  const removal = checkSurface(removed, committed, []);
  assert.ok(removal.some((line) => /session_api::CreateSession\.plan was removed/.test(line)));
});

test("检查：登记的改动（was 等于快照、now 等于现在、写了理由）通过；对不上的登记是死项", () => {
  const committed = surfaceOf(sources());
  const key = "session_api::SessionHandle::send";
  const changed = surfaceOf(
    sources(
      SESSION_API.replace(
        "command: CommandEnvelope) -> Ack;",
        "command: CommandEnvelope) -> Receipt;",
      ),
    ),
  );
  const entry = {
    key,
    was: committed[key],
    now: changed[key],
    reason: "用户决定 2026-10-25：回执改名",
  };
  assert.deepEqual(checkSurface(changed, committed, [entry]), []);
  const removed = surfaceOf(sources(SESSION_API.replace("    pub plan: bool,\n", "")));
  const removal = {
    key: "session_api::CreateSession.plan",
    was: committed["session_api::CreateSession.plan"],
    now: null,
    reason: "用户决定：plan 开关挪到别处",
  };
  assert.deepEqual(checkSurface(removed, committed, [removal]), []);
  // 没写理由、was 对不上、now 对不上：都报。
  assert.ok(
    checkSurface(changed, committed, [{ ...entry, reason: "" }]).some((line) =>
      line.includes("reason"),
    ),
  );
  assert.ok(
    checkSurface(changed, committed, [{ ...entry, was: "x" }]).some((line) =>
      line.includes("dead"),
    ),
  );
  assert.ok(checkSurface(committed, committed, [entry]).some((line) => line.includes("dead")));
});
