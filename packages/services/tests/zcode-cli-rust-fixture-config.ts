import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

/**
 * 用户配置文件 `~/.zcode/cli/config.json`：`permission.mode` 默认 yolo，让既有用例
 * 不经审批直接执行工具；`null` 且没有其他段时不写文件。
 */
export async function writeUserConfig(
  root: string,
  permissionMode: string | null | undefined,
  userConfig: Record<string, unknown> | undefined,
) {
  if (permissionMode === null && !userConfig) return;
  await mkdir(join(root, ".zcode", "cli"), { recursive: true });
  const permission =
    permissionMode === null ? {} : { permission: { mode: permissionMode ?? "yolo" } };
  await writeFile(
    join(root, ".zcode", "cli", "config.json"),
    JSON.stringify({ ...permission, ...userConfig }),
  );
}

/** 轮询等待进程写出非空文件（最多 3 秒）。 */
export async function waitForFile(path: string): Promise<string> {
  const started = Date.now();
  while (true) {
    try {
      const content = await readFile(path, "utf8");
      if (content.trim()) return content;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    assert.ok(Date.now() - started < 3000, `File was not produced: ${path}`);
    await delay(10);
  }
}
