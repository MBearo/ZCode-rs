import { join } from "node:path";
import type { SkillRoot } from "@zcode/contracts";
import { DYNAMIC_WORKFLOW_SKILL_NAME } from "./bundled-skills.js";

/**
 * App 装配层按动态工作流开关过滤配套技能。
 * 工具面的减法在 core 的 registerBuiltInTools，`/` 目录与 `/workflow` 展开的减法分别在
 * zcode-protocol/slash-commands.ts 与 builtin-prompt-command.ts；这里只放需要 bootstrap 侧路径推导的技能剔除。
 */

const SKILL_MANIFEST_FILE_NAME = "SKILL.md";

/**
 * 动态工作流关闭时要从技能发现中剔除的 SKILL.md 绝对路径。
 *
 * 为什么按路径而不是按 root 过滤：NodeSkillAdapter 只提供 `disabledPaths` 这一个剔除机制
 * （config.json 的 `skill.<path>.enable=false` 走的也是它）。传入的是内置技能包的根
 * （bundled-skills.ts），路径不存在时只是一个永不命中的 Set 成员，没有副作用；技能包日后再放
 * 与动态工作流开关无关的技能时，它们也不会被连坐。
 */
export function collectDynamicWorkflowDisabledSkillPaths(
  bundledSkillRoots: readonly SkillRoot[],
): string[] {
  return bundledSkillRoots.map((root) =>
    join(root.path, DYNAMIC_WORKFLOW_SKILL_NAME, SKILL_MANIFEST_FILE_NAME),
  );
}
