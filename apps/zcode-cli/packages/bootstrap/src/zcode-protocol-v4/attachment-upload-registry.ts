import { createHash } from "node:crypto";
import {
  PROTOCOL_V4_LIMITS,
  type V4AttachmentBeginParams,
  type V4AttachmentBeginResult,
  type V4AttachmentChunkParams,
  type V4AttachmentChunkResult,
  type V4AttachmentCommitParams,
  type V4AttachmentCommitResult,
} from "@zcode/shared/zcode-protocol-v4";

interface UploadMetadata {
  fileName: string;
  mime: string;
  totalBytes: number;
  totalChunks: number;
  checksum: string;
}

interface StagedUpload {
  key: string;
  connectionId: string;
  sessionId: string;
  uploadId: string;
  metadata: UploadMetadata;
  chunks: Uint8Array[];
  receivedBytes: number;
  expiresAt: number;
  commitPromise: Promise<V4AttachmentCommitResult> | null;
}

interface CommittedUpload {
  connectionId: string;
  sessionId: string;
  uploadId: string;
  metadata: UploadMetadata;
  nextChunkIndex: number;
  ref: string;
  expiresAt: number;
}

interface AttachmentUploadRegistryOptions {
  now: () => number;
  putSessionAttachment: (
    sessionId: string,
    input: { fileName: string; mime: string; bytes: Uint8Array },
  ) => Promise<{ ref: string }>;
}

function uploadKey(connectionId: string, sessionId: string, uploadId: string): string {
  return `${connectionId}\0${sessionId}\0${uploadId}`;
}

function metadataOf(params: V4AttachmentBeginParams): UploadMetadata {
  return {
    fileName: params.fileName,
    mime: params.mime,
    totalBytes: params.totalBytes,
    totalChunks: params.totalChunks,
    checksum: params.checksum,
  };
}

function sameMetadata(left: UploadMetadata, right: UploadMetadata): boolean {
  return (
    left.fileName === right.fileName &&
    left.mime === right.mime &&
    left.totalBytes === right.totalBytes &&
    left.totalChunks === right.totalChunks &&
    left.checksum === right.checksum
  );
}

function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

/**
 * CLI 进程内 attachment staging 权威表。
 *
 * 旧 attachment/put 会把 20MiB bytes 扩成约 26.7MiB NDJSON 单行，mobile
 * 外层再膨胀一次。这里逐片解码后计账，只有 commit 校验完整事实才落 artifact。
 */
export class AttachmentUploadRegistry {
  private readonly staged = new Map<string, StagedUpload>();
  private readonly committed = new Map<string, CommittedUpload>();
  private stagedBytes = 0;

  constructor(private readonly options: AttachmentUploadRegistryOptions) {}

  begin(params: V4AttachmentBeginParams): V4AttachmentBeginResult {
    this.pruneExpired();
    const key = uploadKey(params.connectionId, params.sessionId, params.uploadId);
    const metadata = metadataOf(params);
    const committed = this.committed.get(key);
    if (committed) {
      if (!sameMetadata(committed.metadata, metadata)) {
        throw new Error("fault.attachment.beginConflict");
      }
      return {
        uploadId: params.uploadId,
        state: "committed",
        nextChunkIndex: committed.nextChunkIndex,
        ref: committed.ref,
      };
    }
    const existing = this.staged.get(key);
    if (existing) {
      if (!sameMetadata(existing.metadata, metadata)) {
        throw new Error("fault.attachment.beginConflict");
      }
      existing.expiresAt = this.options.now() + PROTOCOL_V4_LIMITS.attachmentUploadTtlMs;
      return {
        uploadId: params.uploadId,
        state: "staging",
        nextChunkIndex: existing.chunks.length,
      };
    }
    if (this.staged.size >= PROTOCOL_V4_LIMITS.attachmentUploadMaxConcurrent) {
      throw new Error("fault.attachment.tooManyUploads");
    }
    if (
      params.totalChunks > 0 &&
      params.totalBytes > params.totalChunks * PROTOCOL_V4_LIMITS.attachmentChunkMaxBytes
    ) {
      throw new Error("fault.attachment.chunkCountInsufficient");
    }
    this.staged.set(key, {
      key,
      connectionId: params.connectionId,
      sessionId: params.sessionId,
      uploadId: params.uploadId,
      metadata,
      chunks: [],
      receivedBytes: 0,
      expiresAt: this.options.now() + PROTOCOL_V4_LIMITS.attachmentUploadTtlMs,
      commitPromise: null,
    });
    return { uploadId: params.uploadId, state: "staging", nextChunkIndex: 0 };
  }

  chunk(params: V4AttachmentChunkParams): V4AttachmentChunkResult {
    this.pruneExpired();
    const key = uploadKey(params.connectionId, params.sessionId, params.uploadId);
    const upload = this.staged.get(key);
    if (!upload) throw new Error("fault.attachment.uploadNotFound");
    if (upload.commitPromise) throw new Error("fault.attachment.commitInProgress");
    const bytes = new Uint8Array(Buffer.from(params.dataBase64, "base64"));
    if (params.chunkIndex < upload.chunks.length) {
      // chunks 只通过 push 增长；前置范围检查已证明元素存在，供严格索引类型检查使用。
      if (!sameBytes(upload.chunks[params.chunkIndex]!, bytes)) {
        throw new Error("fault.attachment.chunkConflict");
      }
      return { uploadId: params.uploadId, nextChunkIndex: upload.chunks.length };
    }
    if (params.chunkIndex > upload.chunks.length) {
      throw new Error("fault.attachment.chunkGap");
    }
    if (params.chunkIndex >= upload.metadata.totalChunks) {
      throw new Error("fault.attachment.tooManyChunks");
    }
    if (bytes.byteLength === 0) throw new Error("fault.attachment.emptyChunk");
    if (upload.receivedBytes + bytes.byteLength > upload.metadata.totalBytes) {
      throw new Error("fault.attachment.totalBytesExceeded");
    }
    if (this.stagedBytes + bytes.byteLength > PROTOCOL_V4_LIMITS.attachmentUploadMaxStagedBytes) {
      throw new Error("fault.attachment.stagingCapacityExceeded");
    }
    upload.chunks.push(bytes);
    upload.receivedBytes += bytes.byteLength;
    upload.expiresAt = this.options.now() + PROTOCOL_V4_LIMITS.attachmentUploadTtlMs;
    this.stagedBytes += bytes.byteLength;
    return { uploadId: params.uploadId, nextChunkIndex: upload.chunks.length };
  }

  async commit(params: V4AttachmentCommitParams): Promise<V4AttachmentCommitResult> {
    this.pruneExpired();
    const key = uploadKey(params.connectionId, params.sessionId, params.uploadId);
    const committed = this.committed.get(key);
    if (committed) return { ref: committed.ref };
    const upload = this.staged.get(key);
    if (!upload) throw new Error("fault.attachment.uploadNotFound");
    if (upload.commitPromise) return upload.commitPromise;
    const promise = this.commitStaged(upload);
    upload.commitPromise = promise;
    try {
      return await promise;
    } catch (error) {
      if (this.staged.get(key) === upload) upload.commitPromise = null;
      throw error;
    }
  }

  async abort(params: V4AttachmentCommitParams): Promise<void> {
    this.pruneExpired();
    const key = uploadKey(params.connectionId, params.sessionId, params.uploadId);
    const upload = this.staged.get(key);
    if (upload?.commitPromise) {
      await upload.commitPromise;
      return;
    }
    this.deleteStaged(key);
  }

  clearConnection(connectionId: string): void {
    this.clearWhere((upload) => upload.connectionId === connectionId);
    for (const [key, upload] of this.committed) {
      if (upload.connectionId === connectionId) this.committed.delete(key);
    }
  }

  clearSession(sessionId: string): void {
    this.clearWhere((upload) => upload.sessionId === sessionId);
    for (const [key, upload] of this.committed) {
      if (upload.sessionId === sessionId) this.committed.delete(key);
    }
  }

  clear(): void {
    this.staged.clear();
    this.committed.clear();
    this.stagedBytes = 0;
  }

  pruneExpired(): void {
    const now = this.options.now();
    this.clearWhere((upload) => upload.expiresAt <= now && upload.commitPromise === null);
    for (const [key, upload] of this.committed) {
      if (upload.expiresAt <= now) this.committed.delete(key);
    }
  }

  private async commitStaged(upload: StagedUpload): Promise<V4AttachmentCommitResult> {
    if (
      upload.chunks.length !== upload.metadata.totalChunks ||
      upload.receivedBytes !== upload.metadata.totalBytes
    ) {
      throw new Error("fault.attachment.uploadIncomplete");
    }
    const hash = createHash("sha256");
    for (const chunk of upload.chunks) hash.update(chunk);
    if (`sha256:${hash.digest("hex")}` !== upload.metadata.checksum) {
      throw new Error("fault.attachment.checksumMismatch");
    }
    const bytes = new Uint8Array(upload.metadata.totalBytes);
    let offset = 0;
    for (const chunk of upload.chunks) {
      bytes.set(chunk, offset);
      offset += chunk.byteLength;
    }
    const { ref } = await this.options.putSessionAttachment(upload.sessionId, {
      fileName: upload.metadata.fileName,
      mime: upload.metadata.mime,
      bytes,
    });
    if (this.staged.get(upload.key) === upload) this.deleteStaged(upload.key);
    this.committed.set(upload.key, {
      connectionId: upload.connectionId,
      sessionId: upload.sessionId,
      uploadId: upload.uploadId,
      metadata: upload.metadata,
      nextChunkIndex: upload.chunks.length,
      ref,
      expiresAt: this.options.now() + PROTOCOL_V4_LIMITS.attachmentUploadTtlMs,
    });
    return { ref };
  }

  private deleteStaged(key: string): void {
    const upload = this.staged.get(key);
    if (!upload) return;
    this.staged.delete(key);
    this.stagedBytes -= upload.receivedBytes;
  }

  private clearWhere(predicate: (upload: StagedUpload) => boolean): void {
    for (const [key, upload] of this.staged) {
      if (predicate(upload)) this.deleteStaged(key);
    }
  }
}
