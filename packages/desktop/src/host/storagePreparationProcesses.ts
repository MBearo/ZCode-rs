import { realpath } from "node:fs/promises";
import { resolve as resolvePath } from "node:path";
import { Worker } from "node:worker_threads";
import { createInterface } from "node:readline";
import { z } from "zod";
import {
  databaseStartupErrorCodeSchema,
  databaseMigrationFactsSchema,
  type DatabaseMigrationFacts,
  databaseStartupErrorDetailsSchema,
  zcodeStoragePreparationFrameSchema,
  type DatabaseStartupState,
} from "@zcode/shared";
import {
  resolveDefaultZCodeAgentCommand,
  createNativeAgentStorageProcess,
} from "@zcode/services/storage-startup";

type Phase = NonNullable<DatabaseStartupState["databasePhase"]>;
const workerMessageSchema = z.discriminatedUnion("type", [
  z
    .object({
      type: z.literal("progress"),
      migration: databaseMigrationFactsSchema.optional(),
      phase: z.enum([
        "checking",
        "waiting_for_lock",
        "migrating",
        "committing",
        "maintaining",
        "ready",
      ]),
    })
    .strict(),
  z.object({ type: z.literal("done") }).strict(),
  z
    .object({
      type: z.literal("failed"),
      errorCode: databaseStartupErrorCodeSchema,
      migration: databaseMigrationFactsSchema.optional(),
      ...databaseStartupErrorDetailsSchema.shape,
    })
    .strict(),
]);
const statusError = (
  kind: string,
  details?: {
    sqliteCode?: number;
    systemCode?: string;
    migrationId?: string;
    migration?: DatabaseMigrationFacts;
  },
  databaseId?: string,
) =>
  Object.assign(new Error(`Storage preparation failed: ${kind}`), {
    kind,
    errcode: details?.sqliteCode,
    code: details?.systemCode,
    migrationId: details?.migrationId,
    migrationUpdate:
      databaseId && details?.migration ? { databaseId, migration: details.migration } : undefined,
  });

export function prepareHostStorage(
  path: string,
  report: (phase: Phase, migration?: DatabaseMigrationFacts) => void,
  signal: AbortSignal,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("./tasksStorageWorker.js", import.meta.url), {
      workerData: { path },
    });
    let done = false;
    let failure: unknown;
    let firstState = false;
    const firstStateTimer = setTimeout(() => {
      failure = statusError("startup_status_timeout");
      void worker.terminate();
    }, 30_000);
    const abort = () => {
      failure = statusError("transport_closed");
      void worker.terminate();
    };
    signal.addEventListener("abort", abort, { once: true });
    worker.on("message", (raw: unknown) => {
      const result = workerMessageSchema.safeParse(raw);
      if (!result.success) {
        failure = statusError("transport_closed");
        void worker.terminate();
        return;
      }
      firstState = true;
      clearTimeout(firstStateTimer);
      const message = result.data;
      if (message.type === "progress") report(message.phase, message.migration);
      else if (message.type === "done") done = true;
      else failure = statusError(message.errorCode, message, "tasks-index");
    });
    worker.once("error", (error) => {
      failure = error;
    });
    worker.once("exit", (code) => {
      clearTimeout(firstStateTimer);
      signal.removeEventListener("abort", abort);
      if (done && code === 0 && !failure && firstState) resolve();
      else reject(failure ?? statusError("transport_closed"));
    });
    if (signal.aborted) abort();
  });
}

/** Host 拥有存储准备的 Worker/原生子进程，两种入口共享协议和结束判定。 */
export async function prepareSessionStorage(options: {
  cwd: string;
  env?: Record<string, string>;
  signal: AbortSignal;
  report: (
    phase: Phase,
    details?: { databaseId: string; migration?: DatabaseMigrationFacts },
  ) => void;
  preparedPaths?: Set<string>;
  resolveCommand?: typeof resolveDefaultZCodeAgentCommand;
  observePath: (path: string) => Promise<void>;
}): Promise<void> {
  const command = (options.resolveCommand ?? resolveDefaultZCodeAgentCommand)({
    workspacePath: options.cwd,
    workspaceKey: options.cwd,
    presentationSurface: "desktop",
  });
  if (
    !command?.supportsStorageStartup ||
    (command.storagePreparationMode !== "process" && !command.storagePreparationEntry)
  )
    throw statusError("unsupported_runtime");
  const entry = command.storagePreparationEntry;
  await new Promise<void>((resolve, reject) => {
    // Rust binary 不能交给 Node Worker。仅显式声明 native 的 runtime 使用进程适配器，
    // 两种入口继续共享下面同一个握手、观测、去重和终态判断。
    const native =
      command.storagePreparationMode === "process"
        ? createNativeAgentStorageProcess(command, { ...process.env, ...options.env })
        : undefined;
    const child = native
      ? undefined
      : new Worker(entry!, {
          argv: ["app-server", "--stdio", "--prepare-storage", "--cwd", command.cwd ?? options.cwd],
          env: { ...process.env, ...options.env, ...command.env },
          stdin: true,
          stdout: true,
          stderr: true,
        });
    const input = native?.input ?? child!.stdin!;
    const lines = createInterface({ input: native?.output ?? child!.stdout });
    let settled = false;
    let prepared = false;
    let pathReceived = false;
    let preparedPath: string | undefined;
    let failure: unknown;
    const terminate = () => {
      if (native) native.terminate();
      else void child!.terminate();
    };
    const abort = () => {
      failure ??= statusError("transport_closed");
      terminate();
    };
    const firstStateTimer = setTimeout(() => {
      failure = statusError("startup_status_timeout");
      terminate();
    }, 30_000);
    options.signal.addEventListener("abort", abort, { once: true });
    // stdout 只有有界控制帧，stderr 排空但不把可能含本地路径的原始文本上报。
    (native?.stderr ?? child!.stderr).resume();
    input.on("error", (error) => {
      failure ??= error;
      terminate();
    });
    lines.on("line", (line) => {
      try {
        if (line.length > 65536) throw statusError("transport_closed");
        const frame = zcodeStoragePreparationFrameSchema.parse(JSON.parse(line));
        clearTimeout(firstStateTimer);
        if (frame.method === "startup/storagePath") {
          if (pathReceived) throw statusError("transport_closed");
          pathReceived = true;
          void (async () => {
            // 仅复用同一次准备中已成功关闭的真实库，不能按不同 cwd 误判为不同数据库。
            preparedPath = await realpath(frame.params.path).catch(
              (error: NodeJS.ErrnoException) => {
                if (error.code === "ENOENT") return resolvePath(frame.params.path);
                throw error;
              },
            );
            if (settled || failure || options.signal.aborted) return;
            const reuse = options.preparedPaths?.has(preparedPath) ?? false;
            if (!reuse) await options.observePath(frame.params.path);
            if (!settled && !failure && !options.signal.aborted)
              input.write(`${JSON.stringify({ method: "startup/storagePathReady", reuse })}\n`);
          })().catch((error) => {
            failure ??= error;
            terminate();
          });
        } else if (frame.method === "startup/storagePrepared") {
          prepared = pathReceived;
          input.end();
        } else if (frame.params.phase === "failed")
          failure ??= statusError(
            frame.params.errorCode ?? "sql_failed",
            frame.params,
            frame.params.databaseId,
          );
        else
          options.report(frame.params.phase, {
            databaseId: frame.params.databaseId,
            migration: frame.params.migration,
          });
      } catch (error) {
        failure ??= error;
        terminate();
      }
    });
    const onError = (error: Error) => {
      failure ??= error;
    };
    const onExit = (code: number | null) => {
      settled = true;
      clearTimeout(firstStateTimer);
      options.signal.removeEventListener("abort", abort);
      lines.close();
      if (code === 0 && prepared && !failure) {
        if (preparedPath) options.preparedPaths?.add(preparedPath);
        resolve();
      } else reject(failure ?? statusError("transport_closed"));
    };
    if (native) {
      native.onError(onError);
      native.onExit(onExit);
    } else {
      child!.once("error", onError);
      child!.once("exit", onExit);
    }
    if (options.signal.aborted) abort();
  });
}
