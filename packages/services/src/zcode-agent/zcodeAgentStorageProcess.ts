import { spawn } from "node:child_process";
import type { Readable, Writable } from "node:stream";
import { terminateProcessTree } from "#src/process/processTreeTerminator.js";
import type { ZCodeAgentCommand } from "./zcodeAgentProcessManager.js";

export interface NativeAgentStorageProcess {
  input: Writable;
  output: Readable;
  stderr: Readable;
  onError: (listener: (error: Error) => void) => void;
  onExit: (listener: (code: number | null) => void) => void;
  terminate: () => void;
}

/** 存储专用入口不启动模型/MCP；复用已有进程树清理，Host 仍是唯一生命周期 owner。 */
export function createNativeAgentStorageProcess(
  command: ZCodeAgentCommand,
  env: NodeJS.ProcessEnv,
): NativeAgentStorageProcess {
  const startedAt = Date.now();
  const child = spawn(command.command, [...(command.args ?? []), "--prepare-storage"], {
    cwd: command.cwd,
    env: { ...env, ...command.env },
    stdio: ["pipe", "pipe", "pipe"],
    detached: process.platform !== "win32",
    windowsHide: true,
  });
  return {
    input: child.stdin,
    output: child.stdout,
    stderr: child.stderr,
    onError: (listener) => {
      child.once("error", listener);
    },
    // 原生进程 exit 可能早于最后一帧 drain；close 才说明协议管道已经读完。
    onExit: (listener) => {
      child.once("close", listener);
    },
    terminate: () =>
      terminateProcessTree(child, {
        ownedProcessStartedAtMs: startedAt,
        ...(process.platform !== "win32" && child.pid ? { ownedProcessGroupId: child.pid } : {}),
      }),
  };
}
