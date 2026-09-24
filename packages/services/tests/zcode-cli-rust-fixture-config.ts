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

/**
 * Node 的标题 sidecar 请求（首条输入后异步发出）：单独应答、单独记录，不计入用例的
 * 对话请求序列。Chat Completions 返回 `{"title": ...}`，其他协议返回 500（标题失败不影响会话）。
 */
export function answerTitleRequest(
  body: string,
  path: string,
  res: import("node:http").ServerResponse,
  title: string,
): boolean {
  if (!body.includes("Generate a concise title for this coding session.")) return false;
  if (!path.endsWith("/chat/completions")) {
    res.writeHead(500);
    res.end();
    return true;
  }
  res.writeHead(200, { "Content-Type": "text/event-stream" });
  const delta = { choices: [{ delta: { content: JSON.stringify({ title }) } }] };
  res.write(`data: ${JSON.stringify(delta)}\n\n`);
  res.write(`data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "stop" }] })}\n\n`);
  res.end("data: [DONE]\n\n");
  return true;
}
