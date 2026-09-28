// ============================================================
// 「这段历史里加载过某个技能吗」
// ============================================================
// 工作流创作工具的技能门（tool/handlers/workflow-skill-gate.ts）问的是这一句。判据刻意取自
// runtime 的 provider 可见历史，而不是另立一个会话级 Set：历史就是模型此刻记得的东西——
// compaction 把那次 Skill 调用挤出去，技能正文也一起不在了，门理应重新关上；resume / rewind
// 重建历史时，答案随之重建，不需要第二套 hydration。

import type { RuntimeMessageEntry } from "./message-history.js";

const SKILL_TOOL_NAME = "Skill";

/**
 * 历史里是否有一次**成功完成**的 `Skill` 调用加载了 `skillName`。
 *
 * 成功 = assistant 发出的调用有对应的 tool 结果条目且不是错误。只发出没结果（还在跑、被拒）
 * 或结果 `isError` 都不算。技能名同时认当前形 `{ skill }` 与旧形 `{ name }`
 * （contracts 的 SkillInputSchema 两种都收）。
 */
export function sessionHasLoadedSkill(
  entries: readonly RuntimeMessageEntry[],
  skillName: string,
): boolean {
  const pendingCallIds = new Set<string>();
  for (const entry of entries) {
    if (entry.kind === "attachment") continue;
    const message = entry.message;
    if (message.role === "assistant") {
      for (const call of message.toolCalls ?? []) {
        if (call.name === SKILL_TOOL_NAME && skillInputNames(call.input) === skillName) {
          pendingCallIds.add(call.id);
        }
      }
      continue;
    }
    if (
      message.role === "tool" &&
      message.toolCallId !== undefined &&
      pendingCallIds.has(message.toolCallId) &&
      message.isError !== true
    ) {
      return true;
    }
  }
  return false;
}

function skillInputNames(input: unknown): string | undefined {
  if (input === null || typeof input !== "object") return undefined;
  const fields = input as { skill?: unknown; name?: unknown };
  if (typeof fields.skill === "string") return fields.skill;
  if (typeof fields.name === "string") return fields.name;
  return undefined;
}
