import { ReadOutputSchema } from "@zcode/contracts";
import { createReadFileStateKey } from "./read-file-state.js";
import type { ReadFileStateEntry, ReadFileStateMap } from "./types.js";

export const READ_FILE_STATE_METADATA_SCHEMA_VERSION = 1;
// `CreateWorkflow` / `AmendWorkflow`：内联草稿的字节就是模型那次调用的 `script` 入参，与 Write
// 同理记作完整视图（handlers/workflow-draft-read-state.ts）。
export type PersistedReadFileStateTool =
  | "Read"
  | "Write"
  | "Edit"
  | "CreateWorkflow"
  | "AmendWorkflow";

export interface PersistedReadFileStateMetadata {
  schemaVersion: typeof READ_FILE_STATE_METADATA_SCHEMA_VERSION;
  tool: PersistedReadFileStateTool;
  path: string;
  content: string;
  offset?: number;
  limit?: number;
  isPartialView: boolean;
  readAtMs: number;
  revisionId: string;
  mtimeMs: number;
  sizeBytes: number;
}

export function createReadFileStateMetadata(input: {
  completedAt: Date;
  output: unknown;
  readFileState?: ReadFileStateMap;
  toolInput: unknown;
  toolName: string;
}): PersistedReadFileStateMetadata | undefined {
  if (input.toolName !== "Read") return undefined;

  const parsedOutput = ReadOutputSchema.safeParse(input.output);
  if (!parsedOutput.success) return undefined;
  const output = parsedOutput.data;
  if (output.type !== "text" && output.type !== "file_unchanged") return undefined;

  const toolInput = asRecord(input.toolInput);
  const offset = toolInput ? numberField(toolInput, "offset") : undefined;
  const limit = toolInput ? numberField(toolInput, "limit") : undefined;
  const entry = input.readFileState?.get(
    createReadFileStateKey(output.filePath, offset === undefined ? 1 : offset, limit),
  );
  return createReadFileStateMetadataFromEntry({
    completedAt: input.completedAt,
    entry,
    toolName: "Read",
  });
}

export function createReadFileStateMetadataFromEntry(input: {
  completedAt: Date;
  entry?: ReadFileStateEntry;
  toolName: PersistedReadFileStateTool;
}): PersistedReadFileStateMetadata | undefined {
  const entry = input.entry;
  if (!entry) return undefined;
  if (entry.mtimeMs === undefined || entry.sizeBytes === undefined || !entry.revisionId) {
    // resume stale guard 必须恢复文件时间戳和 adapter revision；
    // 缺 freshness metadata 的历史文件状态不能冒充已读，否则会放过 stale 写入。
    return undefined;
  }

  return {
    schemaVersion: READ_FILE_STATE_METADATA_SCHEMA_VERSION,
    tool: input.toolName,
    path: entry.path,
    content: entry.content,
    ...(entry.offset === undefined ? {} : { offset: entry.offset }),
    ...(entry.limit === undefined ? {} : { limit: entry.limit }),
    isPartialView: entry.isPartialView,
    readAtMs: input.completedAt.getTime(),
    revisionId: entry.revisionId,
    mtimeMs: entry.mtimeMs,
    sizeBytes: entry.sizeBytes,
  };
}

export function parseReadFileStateMetadata(
  metadata: unknown,
): PersistedReadFileStateMetadata | undefined {
  const record = asRecord(metadata);
  if (!record) return undefined;
  const readState = asRecord(record.readFileState);
  if (!readState) return undefined;

  if (readState.schemaVersion !== READ_FILE_STATE_METADATA_SCHEMA_VERSION) return undefined;
  const tool = stringField(readState, "tool");
  if (!isPersistedReadFileStateTool(tool)) return undefined;

  const path = stringField(readState, "path");
  const content = stringField(readState, "content");
  const readAtMs = numberField(readState, "readAtMs");
  const isPartialView = booleanField(readState, "isPartialView");
  const revisionId = stringField(readState, "revisionId");
  const mtimeMs = numberField(readState, "mtimeMs");
  const sizeBytes = numberField(readState, "sizeBytes");
  if (
    !path ||
    content === undefined ||
    readAtMs === undefined ||
    isPartialView === undefined ||
    !revisionId ||
    mtimeMs === undefined ||
    sizeBytes === undefined
  ) {
    return undefined;
  }
  const offset = numberField(readState, "offset");
  const limit = numberField(readState, "limit");

  return {
    schemaVersion: READ_FILE_STATE_METADATA_SCHEMA_VERSION,
    tool,
    path,
    content,
    ...(offset === undefined ? {} : { offset }),
    ...(limit === undefined ? {} : { limit }),
    isPartialView,
    readAtMs,
    revisionId,
    mtimeMs,
    sizeBytes,
  };
}

function isPersistedReadFileStateTool(value: unknown): value is PersistedReadFileStateTool {
  return (
    value === "Read" ||
    value === "Write" ||
    value === "Edit" ||
    value === "CreateWorkflow" ||
    value === "AmendWorkflow"
  );
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  return value as Record<string, unknown>;
}

function stringField(record: Record<string, unknown>, key: string): string | undefined {
  const value = record[key];
  return typeof value === "string" ? value : undefined;
}

function numberField(record: Record<string, unknown>, key: string): number | undefined {
  const value = record[key];
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function booleanField(record: Record<string, unknown>, key: string): boolean | undefined {
  const value = record[key];
  return typeof value === "boolean" ? value : undefined;
}
