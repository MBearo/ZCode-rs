import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

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
