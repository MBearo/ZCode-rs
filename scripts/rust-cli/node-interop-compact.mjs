// rust-cli 与 Node CLI 的会话互操作检查的第二十段：压缩前已加载的工具（docs/specs/rust-cli/compact.md §3.5，
// 工作线 F82）。
//
// rust-cli 写的压缩会话（第四段的那个，node_interop `write` 的第 4 个会话）的边界带着压缩前已加载的工具：
// 1. 存在 `compaction` part 的 `metadata.rustCli.preCompactDiscoveredTools`，Node 读得出；`compactBoundary` 照样过
//    Node 的 strict schema（`parseCompactBoundaryPayload`），里面没有这一项；
// 2. App：每条消息过 App 的 strict schema；`mapMessageWithParts` 给 App 的 `compaction` part 的 metadata 不带它；
// 3. rust-cli 冷读取把它还原到边界上（`discovered_tools`）。
import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode } from "./node-root.mjs";

const CARRIED = ["WebFetch", "mcp__fixture__lookup"];

export async function checkCompactCarried(ctx) {
  const { nodeRoot, store, dir, rust, sessionID, expectEqual } = ctx;
  const { parseCompactBoundaryPayload } = await importNode(
    nodeRoot,
    "contracts/src/compact/index.ts",
  );
  const messages = await store.messages({ sessionID });
  const compaction = messages
    .flatMap((message) => message.parts)
    .find((part) => part.type === "compaction");
  expectEqual(
    "Node reads the tools loaded before compaction",
    compaction?.metadata?.rustCli?.preCompactDiscoveredTools,
    CARRIED,
  );
  const payload = parseCompactBoundaryPayload(compaction?.compactBoundary);
  expectEqual(
    "the strict compactBoundary does not carry them",
    "preCompactDiscoveredTools" in payload,
    false,
  );
  expectEqual(
    "the App schema accepts the compacted session",
    await appSchemaRejections(nodeRoot, store, [sessionID]),
    [],
  );
  const { mapMessageWithParts } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol/message-mapper.ts",
  );
  const mapped = messages
    .map(mapMessageWithParts)
    .flatMap((message) => message.parts)
    .find((part) => part.type === "compaction");
  expectEqual("the App's compaction part leaves them out", "rustCli" in (mapped?.metadata ?? {}), false);
  const loaded = rust("read", dir, sessionID);
  expectEqual(
    "rust-cli reads them back onto the boundary",
    loaded.messages[0]?.discovered_tools,
    CARRIED,
  );
}
