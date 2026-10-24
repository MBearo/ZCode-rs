// 互操作检查共用的历史归一与 App 协议检查（docs/specs/rust-cli/node-store.md §6）。
//
// 从 node-interop-check.mjs 拆出：第十一段（node-interop-runtime.mjs）在另一个进程里也要用同样的比较。
// - nodeText / rustText：Node history hydrator 的结果与 rust-cli 冷读取（`LoadedSession`）都归一成「角色 + 文本 +
//   推理块 + 工具调用」与工具结果条目（node-store.md §4.5、§7 登记的差异先去掉）；
// - nodeShape / rustShape：每条 Node 行的角色、`semantics.origin`、`synthetic`、text part 数与 rust-cli 冷读取的
//   消息逐条对应；
// - appSchemaRejections：库里的消息过 App 的 strict schema 与 v4 的事件合成。
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { importNode } from "./node-root.mjs";

// rust-cli 只有数据的空 attachment 不比；相邻的工具结果按调用 id 排序（Node 按声明顺序，rust-cli 按提交顺序）。
// Node 给 Node 写的未结调用补的中断结果，rust-cli 冷读取补同样的（Node 的文字与位置，node-store.md §4.6，工作线 F86、
// F88），直接比。
const joined = (texts) => texts.filter((text) => text.length > 0).join("\n\n");

function sortToolRuns(items) {
  const out = [];
  let run = [];
  const flush = () => {
    out.push(...run.sort((a, b) => a.id.localeCompare(b.id)));
    run = [];
  };
  for (const item of items) {
    if (item.role === "tool") {
      run.push(item);
      continue;
    }
    flush();
    out.push(item);
  }
  flush();
  return out;
}

function nodeContent(content) {
  const blocks = typeof content === "string" ? [{ type: "text", text: content }] : content;
  return blocks.map((b) =>
    b.type === "image"
      ? { type: "image", mediaType: b.mediaType, dataUrl: b.dataUrl }
      : // Read 整读 PDF 的结果（tools-files.md §1.5，修复线 F24）：Node 读成 PDF 的 file 块。
        b.type === "file"
        ? { type: "document", mediaType: b.mediaType, dataUrl: b.dataUrl }
        : { type: "text", text: b.text },
  );
}

/** Node history hydrator 的 runtime entries → 归一的条目。 */
export function nodeText(entries) {
  const items = entries
    .filter((entry) => entry.message && ["user", "assistant", "tool"].includes(entry.message.role))
    .map(({ message }) => {
      if (message.role === "tool") {
        const content = nodeContent(message.content);
        return { role: "tool", id: message.toolCallId, error: message.isError, content };
      }
      const blocks =
        typeof message.content === "string"
          ? [{ type: "text", text: message.content }]
          : message.content;
      const text = joined(blocks.filter((b) => b.type === "text").map((b) => b.text));
      // 人类输入里粘贴的图片（docs/specs/rust-cli/tui-input.md §3.2）。
      const images = nodeContent(blocks.filter((b) => b.type === "image"));
      if (message.role === "user")
        return { role: "user", text, ...(images.length ? { images } : {}) };
      const reasoning = blocks
        .filter((b) => b.type === "reasoning")
        .map((b) => ({ text: b.text, options: b.providerOptions ?? null }));
      const tools = (message.toolCalls ?? []).map(({ id, name, input }) => ({ id, name, input }));
      return { role: "assistant", text, reasoning, tools };
    });
  return sortToolRuns(items);
}

function rustContent(content) {
  return content.map((b) =>
    b.type === "image" || b.type === "document"
      ? {
          type: b.type,
          mediaType: b.source.media_type,
          dataUrl: `data:${b.source.media_type};base64,${b.source.data}`,
        }
      : // ToolSearch 的结果（tools-more.md §3.6）：Node 读的是落库的 `output`，同一句降级文字（第二十二段，F84）。
        b.type === "tool_reference"
        ? { type: "text", text: `Tool loaded: ${b.tool_name}` }
        : { type: "text", text: b.text },
  );
}

/** rust-cli 冷读取的消息 → 归一的条目。 */
export function rustText(messages) {
  const items = messages.flatMap((message) => {
    // 压缩边界不发给模型（compact.md §6）。
    if (message.type === "system") return [];
    if (message.type === "attachment") {
      if (!message.text) return [];
      // 带内容块的 attachment（tui-input.md §1.3）：Node 读出的是一条 user 消息，文字在前、图片在后。
      const blocks = message.data?.blocks ?? [];
      const text = joined([
        message.text,
        ...blocks.filter((b) => b.type === "text").map((b) => b.text),
      ]);
      const images = rustContent(blocks.filter((b) => b.type === "image"));
      return [{ role: "user", text, ...(images.length ? { images } : {}) }];
    }
    if (message.type === "user" && message.origin.kind === "tool_result") {
      return message.content.map((b) => ({
        role: "tool",
        id: b.tool_use_id,
        error: b.is_error,
        content: rustContent(b.content),
      }));
    }
    const text = joined(message.content.filter((b) => b.type === "text").map((b) => b.text));
    const images = rustContent(message.content.filter((b) => b.type === "image"));
    if (message.type === "user")
      return [{ role: "user", text, ...(images.length ? { images } : {}) }];
    const reasoning = message.content.flatMap((b) => {
      if (b.type === "thinking")
        return [
          {
            text: b.thinking,
            options: b.signature ? { anthropic: { signature: b.signature } } : null,
          },
        ];
      if (b.type === "redacted_thinking")
        return [{ text: "", options: { anthropic: { redactedData: b.data } } }];
      return [];
    });
    const tools = message.content
      .filter((b) => b.type === "tool_use")
      .map(({ id, name, input }) => ({ id, name, input }));
    return [{ role: "assistant", text, reasoning, tools }];
  });
  return sortToolRuns(items);
}

/** Node 冷读取一个会话（`session.revert` 的切点按 Node 的读法）的工厂：依赖从调用方的导入里给。 */
export function nodeHistoryReader({ hydrateMessageHistoryFromSession, createMessageHistory }) {
  return async function nodeHistory(store, artifactStore, session, sessionID) {
    const revert = session.revert;
    const history = createMessageHistory();
    await hydrateMessageHistoryFromSession({
      artifactStore,
      history,
      messages: await store.messages({ sessionID }),
      branchCutAfterMessageId: revert?.branchCutAfterMessageID,
      rewindCreatedMessageId: revert?.createdMessageID,
      rewindKeptMessageIds: revert?.keptMessageIDs,
      rewindTargetMessageId: revert?.targetMessageID,
    });
    return nodeText(history.toRuntimeEntries());
  };
}

const INTERRUPTS = ["[Request interrupted by user]", "[Request interrupted by user for tool use]"];

/** Node 行的结构：角色、`semantics.origin`、`synthetic`、text part 数。 */
export function nodeShape(messages) {
  return messages.map(({ info, parts }) => [
    info.role,
    info.semantics?.origin ?? null,
    info.synthetic === true,
    parts.filter((part) => part.type === "text").length,
  ]);
}

/** rust-cli 冷读取的消息在 Node 里应有的行：工具结果没有行；中断与 bash 输出是合成消息。 */
export function rustShape(messages) {
  return messages.flatMap((message) => {
    if (message.type === "system") return [];
    if (message.type === "attachment") {
      // 带内容块的 attachment（L36）：文字块是同一条消息里另外的 text part。
      const blocks = message.data?.blocks ?? [];
      const texts = 1 + blocks.filter((block) => block.type === "text").length;
      return [["user", "agent_runtime", true, texts]];
    }
    if (message.type === "assistant") {
      const texts = message.content.some((block) => block.type === "text") ? 1 : 0;
      return [["assistant", "agent_runtime", false, texts]];
    }
    if (message.origin.kind === "tool_result") return [];
    const texts = message.content.filter((block) => block.type === "text");
    const first = texts[0]?.text ?? "";
    const synthetic =
      message.origin.kind !== "human" ||
      INTERRUPTS.includes(first) ||
      first.startsWith("<bash-stdout>") ||
      first.startsWith("<bash-stderr>");
    return [["user", synthetic ? "agent_runtime" : "real_user", synthetic, texts.length]];
  });
}

/** 这些会话的每条消息过 App 的 strict schema（`mapMessageWithParts` + `zcodeMessageWithPartsSchema`），v4 的事件合成不报错。 */
export async function appSchemaRejections(nodeRoot, store, sessionIDs) {
  const { mapMessageWithParts } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol/message-mapper.ts",
  );
  const { synthesizeEventsFromMessages } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol-v4/transcript-hydration.ts",
  );
  const { zcodeMessageWithPartsSchema } = await import(
    pathToFileURL(join(nodeRoot, "packages/shared/src/zcode-protocol-legacy-types.ts")).href
  );
  const rejected = [];
  for (const sessionID of sessionIDs) {
    const messages = await store.messages({ sessionID });
    for (const message of messages) {
      const parsed = zcodeMessageWithPartsSchema.safeParse(mapMessageWithParts(message));
      if (!parsed.success)
        rejected.push(`${sessionID}/${message.info.id}: ${parsed.error.issues[0]?.message}`);
    }
    try {
      synthesizeEventsFromMessages(messages, { sessionId: sessionID });
    } catch (error) {
      rejected.push(`${sessionID}: ${error}`);
    }
  }
  return rejected;
}
