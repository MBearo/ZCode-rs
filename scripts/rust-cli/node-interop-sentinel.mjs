// rust-cli 与 Node CLI 的会话互操作检查的第十八段：续写占位（docs/specs/rust-cli/node-store.md §4.2.1、
// session-lifecycle.md §6.3.1，工作线 F77、F79）。
//
// rust-cli 按两种写入时机各写一个会话（node_interop `sentinel-write <dir> <timing>`）：
// - `continue`：被中断的输入、占位与新输入同一批、新输入的回答（启动时打开与 `-p` 续写）；
// - `open`：被中断的输入、占位单独一批，之后不再写（会话里 `/resume` 切过去就退出，工作线 F79）。
// 每个会话：
// 1. Node 读出占位：只给模型看的 assistant（`semantics`）；Node 的 history hydrator 把它放进模型上下文，与 rust-cli 的
//    冷读取相同，rust-cli 读回的是占位（模型名 `<synthetic>`）；
// 2. App：每条消息过 App 的 strict schema，v4 的事件合成不报错；投影 policy 是 `providerContextOnly`，旧协议的可见消息
//    （`getZCodeUserVisibleMessages`）不含它，v4 合成的事件里没有它的文字；
// 3. 记下 Node 独立 CLI 的 `/resume` 回放（`projectSessionTranscript`）显示不显示它：它不看 assistant 的 `semantics`
//    （Node 自己的隐藏 assistant 同样），只报告，不判失败（用户 2026-10-24 接受）。
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode } from "./node-root.mjs";

const SENTINEL = "No response requested.";

/** 两种写入时机与各自的模型上下文。 */
const TIMINGS = {
  continue: [
    ["user", "First question"],
    ["assistant", SENTINEL],
    ["user", "Second question"],
    ["assistant", "Second answer"],
  ],
  open: [
    ["user", "First question"],
    ["assistant", SENTINEL],
  ],
};

const sharedModule = (nodeRoot, file) =>
  import(pathToFileURL(join(nodeRoot, "packages/shared/src", file)).href);

async function checkTiming(ctx, timing, context) {
  const { nodeRoot, store, artifactStore, dir, rust, nodeHistory, rustText, expectEqual } = ctx;
  const written = rust("sentinel-write", dir, timing);
  const sessionID = written.session;
  const messages = await store.messages({ sessionID });
  const isPlaceholder = (message) =>
    (message.info.id ?? message.info.messageId) === written.placeholder;
  const label = (text) => `${text} (${timing})`;

  // 1. Node 读出占位，模型上下文与 rust-cli 相同。
  const placeholder = messages.find(isPlaceholder);
  expectEqual(
    label("Node reads the resume placeholder as an assistant"),
    [
      placeholder?.info.role,
      placeholder?.parts.filter((part) => part.type === "text").map((part) => part.text),
    ],
    ["assistant", [SENTINEL]],
  );
  expectEqual(label("the placeholder is for the model only"), placeholder?.info.semantics, {
    origin: "agent_runtime",
    kind: "assistant_response",
    uiVisibility: "hidden",
    providerVisibility: "visible",
    transcriptVisibility: "hidden",
  });
  const session = await store.getSession(sessionID);
  const loaded = rust("read", dir, sessionID);
  const history = await nodeHistory(store, artifactStore, session, sessionID);
  expectEqual(
    label("Node's model context keeps the placeholder as rust-cli does"),
    history,
    rustText(loaded.messages),
  );
  expectEqual(
    label("the model context of the resumed session"),
    history.map((item) => [item.role, item.text]),
    context,
  );
  expectEqual(
    label("rust-cli reads the placeholder back"),
    loaded.messages
      .filter((message) => message.id === written.placeholder)
      .map((message) => message.model),
    ["<synthetic>"],
  );

  // 2. App：schema、投影与 v4 的事件合成。
  expectEqual(
    label("the App schema accepts the resumed session"),
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
  const { getConversationMessageProjectionPolicy } = await sharedModule(
    nodeRoot,
    "conversation-message-projection-policy.ts",
  );
  const { getZCodeUserVisibleMessages } = await sharedModule(
    nodeRoot,
    "zcode-session-visible-content.ts",
  );
  const mapped = messages.map(mapMessageWithParts);
  expectEqual(
    label("the App projects the placeholder as model-only context"),
    mapped.filter(isPlaceholder).map(getConversationMessageProjectionPolicy),
    ["providerContextOnly"],
  );
  expectEqual(
    label("the App's visible messages leave the placeholder out"),
    getZCodeUserVisibleMessages(mapped).map((message) => message.info.messageId),
    mapped.filter((message) => !isPlaceholder(message)).map((message) => message.info.messageId),
  );
  const events = synthesizeEventsFromMessages(messages, { sessionId: sessionID });
  expectEqual(
    label("the App's v4 events do not show the placeholder"),
    JSON.stringify(events).includes(SENTINEL),
    false,
  );

  // 3. Node 独立 CLI 的 `/resume` 回放（只报告）。
  const { projectSessionTranscript } = await importNode(
    nodeRoot,
    "bootstrap/src/session-transcript.ts",
  );
  return projectSessionTranscript(messages).some((message) => message.content === SENTINEL);
}

export async function checkSentinel(ctx) {
  const shows = [];
  for (const [timing, context] of Object.entries(TIMINGS))
    shows.push(await checkTiming(ctx, timing, context));
  return { nodeCliReplayShowsIt: shows.every(Boolean) };
}
