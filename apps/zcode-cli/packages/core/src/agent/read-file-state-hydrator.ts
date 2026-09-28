import type { MessageId, MessagePart, MessageWithParts, ToolPart } from "@zcode/contracts";
import {
  parseReadFileStateMetadata,
  type PersistedReadFileStateTool,
} from "../tool/read-file-state-metadata.js";
import { createReadFileStateKey, normalizeReadFileStateMtimeMs } from "../tool/read-file-state.js";
import type { ReadFileStateMap } from "../tool/types.js";
import { activeSessionMessages } from "./session-history-hydrator.js";

export interface ReadFileStateHydrationResult {
  restoredCount: number;
  skippedRangeReadCount: number;
  skippedUnreadableEditCount: number;
}

type CompletedToolPart = ToolPart & {
  state: ToolPart["state"] & {
    output: unknown;
    status: "completed";
  };
};

export async function hydrateReadFileStateFromSession(input: {
  branchCutAfterMessageId?: MessageId;
  messages: MessageWithParts[];
  readFileState: ReadFileStateMap;
  rewindCreatedMessageId?: MessageId;
  rewindKeptMessageIds?: readonly MessageId[];
  rewindTargetMessageId?: MessageId;
  workingDirectory: string;
  workspaceRoot: string;
}): Promise<ReadFileStateHydrationResult> {
  input.readFileState.clear();
  const activeMessages = activeSessionMessages(input.messages, {
    branchCutAfterMessageId: input.branchCutAfterMessageId,
    includeCompactPreservedSegment: false,
    rewindCreatedMessageId: input.rewindCreatedMessageId,
    rewindKeptMessageIds: input.rewindKeptMessageIds,
    rewindTargetMessageId: input.rewindTargetMessageId,
  });

  const result: ReadFileStateHydrationResult = {
    restoredCount: 0,
    skippedRangeReadCount: 0,
    skippedUnreadableEditCount: 0,
  };

  for (const message of activeMessages) {
    if (message.info.role !== "assistant") continue;

    for (const part of dedupeParts(message.parts)) {
      if (!isCompletedToolPart(part)) continue;

      if (part.tool === "Read") {
        const restored = restoreReadToolState(input, part, result);
        if (restored) result.restoredCount++;
        continue;
      }

      if (part.tool === "Write") {
        const restored = restoreMetadataToolState(input.readFileState, part, "Write");
        if (restored) result.restoredCount++;
        continue;
      }

      if (part.tool === "Edit") {
        const restored = restoreMetadataToolState(input.readFileState, part, "Edit");
        if (restored) result.restoredCount++;
        continue;
      }

      // 内联草稿：模型亲手写的字节，与 Write 同一条恢复路径。不带 metadata 的 part（saved 拷贝、
      // `path` 提交、沿用的脚本）在 restoreMetadataToolState 里自然落空。
      if (part.tool === "CreateWorkflow" || part.tool === "AmendWorkflow") {
        const restored = restoreMetadataToolState(input.readFileState, part, part.tool);
        if (restored) result.restoredCount++;
      }
    }
  }

  return result;
}

function restoreReadToolState(
  input: {
    readFileState: ReadFileStateMap;
  },
  part: CompletedToolPart,
  result: ReadFileStateHydrationResult,
): boolean {
  const toolInput = asRecord(part.state.input);
  if (!toolInput) return false;
  if (!isHistoricalFullReadWindow(toolInput as HistoricalReadWindow)) {
    // 真正的 range Read 只在同一 runtime 内作为最新水位，跨 resume 不恢复。
    result.skippedRangeReadCount++;
    return false;
  }

  const metadata = parseReadFileStateMetadata(part.state.metadata);
  if (!metadata) return false;
  if (metadata.tool !== "Read") return false;
  if (!isHistoricalFullReadWindow(metadata)) {
    return false;
  }
  setFullReadState(input.readFileState, metadata.path, metadata.content, {
    // resume 不再从 provider-visible cat-n 文本恢复 Read 状态；只有带
    // mtimeMs/revisionId/sizeBytes 的结构化 metadata 才能支撑后续 stale guard。
    isPartialView: metadata.isPartialView,
    mtimeMs: normalizeReadFileStateMtimeMs(metadata.mtimeMs),
    readAt: new Date(metadata.readAtMs),
    revisionId: metadata.revisionId,
    sizeBytes: metadata.sizeBytes,
    sourceTool: metadata.tool,
  });
  return true;
}

function restoreMetadataToolState(
  readFileState: ReadFileStateMap,
  part: CompletedToolPart,
  expectedTool: PersistedReadFileStateTool,
): boolean {
  const metadata = parseReadFileStateMetadata(part.state.metadata);
  if (!metadata || metadata.tool !== expectedTool) return false;
  if (!isHistoricalFullReadWindow(metadata)) return false;

  // Write/Edit 的历史 tool part 不能在 resume 时读取当前磁盘来“补全”状态；
  // 外部手动保存会被误认证为 agent 已读。这里只恢复成功时持久化的完整快照。
  setFullReadState(readFileState, metadata.path, metadata.content, {
    isPartialView: metadata.isPartialView,
    mtimeMs: normalizeReadFileStateMtimeMs(metadata.mtimeMs),
    readAt: new Date(metadata.readAtMs),
    revisionId: metadata.revisionId,
    sizeBytes: metadata.sizeBytes,
    sourceTool: metadata.tool,
  });
  return true;
}

function setFullReadState(
  readFileState: ReadFileStateMap,
  filePath: string,
  content: string,
  metadata: {
    isPartialView?: boolean;
    mtimeMs?: number;
    readAt: Date;
    revisionId?: string;
    sizeBytes?: number;
    sourceTool?: PersistedReadFileStateTool;
  },
): void {
  readFileState.set(createReadFileStateKey(filePath, 1, undefined), {
    path: filePath,
    content,
    offset: undefined,
    limit: undefined,
    isPartialView: metadata.isPartialView ?? false,
    readAt: metadata.readAt,
    sourceTool: metadata.sourceTool,
    revisionId: metadata.revisionId,
    mtimeMs: metadata.mtimeMs,
    sizeBytes: metadata.sizeBytes ?? Buffer.byteLength(content, "utf8"),
  });
}

interface HistoricalReadWindow {
  limit?: number;
  offset?: number;
}

function isHistoricalFullReadWindow({ offset, limit }: HistoricalReadWindow): boolean {
  return (offset ?? 1) <= 1 && limit === undefined;
}

function dedupeParts(parts: MessagePart[]): MessagePart[] {
  const byId = new Map<string, MessagePart>();
  for (const part of parts) {
    byId.set(part.id, part);
  }
  return [...byId.values()];
}

function isCompletedToolPart(part: MessagePart): part is CompletedToolPart {
  return part.type === "tool" && part.state.status === "completed" && "output" in part.state;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  return value as Record<string, unknown>;
}
