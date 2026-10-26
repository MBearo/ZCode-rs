// rust-cli 与 Node CLI 的会话互操作检查的第二十六段：本地命令的记录（docs/specs/rust-cli/tui-manage.md §11.3，
// 修复线 L-A）。
//
// rust-cli 写一个会话（node_interop `records-write <dir>`）：一轮问答、一批本地命令的记录（固定说明、命令输入、
// 输出与 meta，来源 `local-command-*` 的 meta user 消息）、再一轮问答。检查：
// 1. Node 读出这四条记录：user 角色的合成消息（`synthetic`），只给模型看的 `semantics`；Node 的 history hydrator 把
//    原文作为 user 文本放进模型上下文，与 rust-cli 的冷读取相同；rust-cli 读回的仍是四条 meta 消息；
// 2. App：每条消息过 App 的 strict schema；旧协议的可见消息（`getZCodeUserVisibleMessages`）不含它们，v4 合成的事件
//    里没有命令标签。
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode } from "./node-root.mjs";

const sharedModule = (nodeRoot, file) =>
  import(pathToFileURL(join(nodeRoot, "packages/shared/src", file)).href);

export async function checkRecords(ctx) {
  const { nodeRoot, store, artifactStore, dir, rust, nodeHistory, rustText, expectEqual } = ctx;
  const written = rust("records-write", dir);
  const sessionID = written.session;
  const messages = await store.messages({ sessionID });
  const isRecord = (message) =>
    written.records.includes(message.info.id ?? message.info.messageId);
  const records = messages.filter(isRecord);

  // 1. Node 读出记录，模型上下文与 rust-cli 相同。
  expectEqual(
    "Node reads the local command records as synthetic user messages",
    records.map((message) => [
      message.info.role,
      message.parts.every((part) => part.type === "text" && part.synthetic === true),
    ]),
    written.records.map(() => ["user", true]),
  );
  expectEqual(
    "the local command records are for the model only",
    records.map((message) => [
      message.info.semantics?.uiVisibility,
      message.info.semantics?.providerVisibility,
    ]),
    written.records.map(() => ["hidden", "visible"]),
  );
  const session = await store.getSession(sessionID);
  const loaded = rust("read", dir, sessionID);
  const history = await nodeHistory(store, artifactStore, session, sessionID);
  expectEqual(
    "Node's model context keeps the local command records as rust-cli does",
    history,
    rustText(loaded.messages),
  );
  expectEqual(
    "the model context carries the command input",
    history.some(
      (item) => item.role === "user" && item.text.includes("<command-name>/rename</command-name>"),
    ),
    true,
  );
  expectEqual(
    "rust-cli reads the records back as meta messages",
    loaded.messages
      .filter((message) => written.records.includes(message.id))
      .map((message) => message.origin?.source),
    ["local-command-caveat", "local-command-input", "local-command-output", "local-command-meta"],
  );

  // 2. App：schema、可见消息与 v4 的事件合成。
  expectEqual(
    "the App schema accepts the session with local command records",
    await appSchemaRejections(nodeRoot, store, [sessionID]),
    [],
  );
  const { mapMessageWithParts } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol/message-mapper.ts",
  );
  const { synthesizeEventsFromMessages } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol-v4/transcript-hydration.ts",
  );
  const { getZCodeUserVisibleMessages } = await sharedModule(
    nodeRoot,
    "zcode-session-visible-content.ts",
  );
  const mapped = messages.map(mapMessageWithParts);
  const visible = getZCodeUserVisibleMessages(mapped).map((message) => message.info.messageId);
  expectEqual(
    "the App's visible messages leave the local command records out",
    visible.filter((id) => written.records.includes(id)),
    [],
  );
  const events = JSON.stringify(synthesizeEventsFromMessages(messages, { sessionId: sessionID }));
  expectEqual(
    "the App's v4 events do not show the command tags",
    [events.includes("<command-name>"), events.includes("local-command-caveat")],
    [false, false],
  );
}
