// ============================================================
// 内联草稿记作「模型写过的文件」
// ============================================================
// Edit / Write 拒绝会话没读过的文件（FILE_NOT_READ）：模型不能改它没见过的字节。内联草稿恰好是
// 这条已经成立的文件——它的字节就是产生它的那次调用的 `script` 入参。不记这一笔，NOTE 要求的
// 第一次 `Edit` 必然失败，补救是把模型自己刚写的两万 token 脚本整个 `Read` 一遍，正是草稿文件
// 要省掉的那笔开销。
//
// **只由内联分支调用**：saved 拷贝（带模型没见过的元数据块）、沿用前驱脚本的新草稿（脚本可能
// 来自别的会话或压缩之前）、中枢直接启动与 GUI 设置修订写下的草稿都不是模型本次亲手写的字节，
// 记了就是替模型担保它没看过的内容。
//
// 与草稿本身一样**尽力而为**：stat 不到就不记，模型的第一次 Edit 退回「先 Read」的老路。

import { createReadFileStateMetadataFromEntry } from "../read-file-state-metadata.js";
import { createReadFileStateKey, normalizeReadFileStateMtimeMs } from "../read-file-state.js";
import type { ReadFileStateEntry, ToolExecutionContext } from "../types.js";

export type WorkflowDraftAuthoringTool = "CreateWorkflow" | "AmendWorkflow";

const CRLF_PATTERN = /\r\n/gu;
const LF = "\n";

export async function recordAuthoredWorkflowDraft(
  context: ToolExecutionContext,
  draft: { path: string; source: string; toolName: WorkflowDraftAuthoringTool },
): Promise<void> {
  const { fileSystemPort, readFileState } = context;
  if (readFileState === undefined || fileSystemPort === undefined) return;

  let revision: { id: string; mtimeMs?: number; sizeBytes?: number } | undefined;
  try {
    // 与 Edit / Write 之后的 stale 校验读同一个端口，revision 的口径因此一致。stat 发生在写之后：
    // 记下的 mtime 不早于草稿的真实 mtime，其后任何外部改动都会让 mtime 前进并判 stale。
    const info = await fileSystemPort.stat(
      { path: draft.path, trace: context.traceContext },
      { signal: context.abortSignal },
    );
    revision = info.revision;
  } catch {
    return;
  }
  if (revision === undefined) return;

  const entry: ReadFileStateEntry = {
    path: draft.path,
    // 端口读回的文本统一成 LF（FileSystemReadTextResult.content），快照跟随同一口径。
    content: draft.source.replace(CRLF_PATTERN, LF),
    offset: undefined,
    limit: undefined,
    isPartialView: false,
    readAt: new Date(),
    sourceTool: draft.toolName,
    revisionId: revision.id,
    mtimeMs: normalizeReadFileStateMtimeMs(revision.mtimeMs),
    sizeBytes: revision.sizeBytes ?? Buffer.byteLength(draft.source, "utf8"),
  };
  readFileState.set(createReadFileStateKey(draft.path, 1, undefined), entry);

  // resume 只从 tool part metadata 恢复 read-state（read-file-state-hydrator.ts），不落这一笔，
  // 会话恢复后同一个 bug 原样回来。
  const metadata = createReadFileStateMetadataFromEntry({
    completedAt: entry.readAt,
    entry,
    toolName: draft.toolName,
  });
  if (metadata !== undefined) context.recordReadFileStateMetadata?.(metadata);
}
