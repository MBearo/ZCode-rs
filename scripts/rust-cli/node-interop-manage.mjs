// rust-cli 与 Node CLI 的会话互操作检查的第十三段：会话的改名与删除（修复线 F29，docs/specs/rust-cli/node-store.md §5.1、
// tui-manage.md §10.6）。
//
// 1. rust-cli 改名（`SessionStore::rename`）：Node 读到的标题与来源（custom）、不变的活动时间，与 Node
//    `setCustomSessionTitle` 的写法相同；
// 2. rust-cli 删除（`SessionStore::delete`）：Node 的 `listSessions` 不再列出、`getSession` 有删除时刻，消息保留；
// 3. Node 改名（`updateSession{title, titleSource: custom}`）与软删除（`softDeleteSession`）：rust-cli 的项目页查询读到
//    新标题、不再列出删除的会话、冷读取报不存在；
// 4. 首条输入：rust-cli 查询给出的首条输入与 Node 读到的首条真实用户输入的文字相同。
import { spawnSync } from "node:child_process";

/** Node 读到的首条真实用户输入的第一个文字 part。 */
async function nodeFirstPrompt(store, sessionID) {
  const messages = await store.messages({ sessionID });
  for (const message of messages) {
    if (message.info.role !== "user" || message.info.semantics?.origin !== "real_user") continue;
    const part = message.parts.find((p) => p.type === "text" && !p.synthetic);
    if (part) return part.text;
  }
  return undefined;
}

export async function checkManage({ store, dir, rust, binary, expectEqual }) {
  const { sessions } = rust("manage-write", dir);
  const [first, second] = sessions;
  const before = await store.getSession(first);
  // 1. rust-cli 改名。
  rust("rename", dir, first, "Renamed by rust-cli");
  const renamed = await store.getSession(first);
  expectEqual(
    "Node reads the rust-cli rename",
    [renamed?.title, renamed?.titleSource, renamed?.time.updated],
    ["Renamed by rust-cli", "custom", before?.time.updated],
  );
  // 4. 首条输入两边一样。
  const page = rust("query", dir, "/w/manage");
  expectEqual(
    "rust-cli first prompt equals Node's first user text",
    page.sessions.map((s) => s.first_prompt),
    [await nodeFirstPrompt(store, first)],
  );
  expectEqual("rust-cli reads its own rename", page.sessions[0]?.title, "Renamed by rust-cli");
  // 2. rust-cli 删除。
  rust("delete", dir, first);
  const deleted = await store.getSession(first);
  expectEqual("Node sees the deletion time", typeof deleted?.time.deleted, "number");
  expectEqual(
    "Node no longer lists the deleted session",
    (await store.listSessions({ directory: "/w/manage", roots: true })).map((s) => s.id),
    [],
  );
  expectEqual(
    "the deleted session keeps its messages",
    (await store.messages({ sessionID: first })).length > 0,
    true,
  );
  // 3. Node 改名与删除，rust-cli 读。
  await store.updateSession({ id: second, title: "Renamed by Node", titleSource: "custom" });
  const all = rust("query", dir);
  expectEqual(
    "rust-cli reads Node's rename and drops the deleted session",
    all.sessions.filter((s) => sessions.includes(s.id)).map((s) => [s.id, s.title]),
    [[second, "Renamed by Node"]],
  );
  await store.softDeleteSession(second);
  const after = rust("query", dir, "/w/manage-other");
  expectEqual("rust-cli drops the session Node deleted", after.sessions.length, 0);
  const read = spawnSync(binary, ["read", dir, second], { encoding: "utf8" });
  expectEqual(
    "rust-cli cannot load the session Node deleted",
    [read.status !== 0, read.stderr.includes("not found")],
    [true, true],
  );
}
