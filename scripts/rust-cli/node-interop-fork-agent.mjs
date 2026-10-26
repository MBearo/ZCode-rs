// rust-cli 与 Node CLI 的会话互操作检查的第二十七段：fork 子代理的子会话（docs/specs/rust-cli/node-store.md §4.8.1、
// agents.md §8.1.3，修复线 L-A）。
//
// rust-cli 写一个父会话（一轮输入、一条 fork 的 Agent 调用，结果是“已启动”、关联到子会话）与 fork 的子会话
//（`rust-cli/fork_parent`、directive、子代理的回答；继承的前缀不写进子会话）。检查：
// 1. 两个会话过 App 的 strict schema；
// 2. Node 读出子会话：第一条输入是 directive，没有父会话的消息（App 的子代理视图不重复显示父的历史）；
// 3. Node 的子代理查询按 tool part 的 `childSessionId` 找到子会话；rust-cli 读回子会话的两条消息。
import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode } from "./node-root.mjs";

export async function checkForkAgent(ctx) {
  const { nodeRoot, store, dir, rust, expectEqual } = ctx;
  const written = rust("fork-agent-write", dir);
  expectEqual(
    "the App schema accepts the parent and the fork's child session",
    await appSchemaRejections(nodeRoot, store, [written.parent, written.child]),
    [],
  );
  const child = await store.messages({ sessionID: written.child });
  const texts = child.map((message) => [
    message.info.role,
    message.parts
      .filter((part) => part.type === "text")
      .map((part) => part.text)
      .join(""),
  ]);
  expectEqual(
    "Node reads the fork's child session without the parent's messages",
    texts,
    [
      ["user", written.directive],
      ["assistant", "Fork report: alpha"],
    ],
  );
  const { collectSubagentChildSessionIds } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol/subagent-session-query.ts",
  );
  const parent = await store.getSession(written.parent);
  expectEqual(
    "Node finds the fork's child session",
    collectSubagentChildSessionIds(parent, await store.messages({ sessionID: written.parent })),
    [written.child],
  );
  expectEqual(
    "rust-cli reads the fork's own messages back",
    rust("read", dir, written.child).messages.length,
    2,
  );
}
