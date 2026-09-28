// 工作流创作工具的技能加载检查。
// CreateWorkflow、AmendWorkflow、SaveWorkflow 和 EvalWorkflowSnippet 的工具描述保持简短，
// facade 与写作规则由 `dynamic-workflows` 技能提供。提交脚本前必须加载技能：会话历史里没有
// 成功的 `Skill(dynamic-workflows)` 调用时，resolveInput 直接拒绝，避免进入 hook 或显示无效确认窗。
//
// 判据来自模型当前可见的 messageHistory。compaction 移除技能正文及对应调用后，需要重新加载；
// resume/rewind 则随历史一起恢复该判据，不维护第二份会话状态。
// 探针缺席表示当前装配未提供技能加载检查，此时不设置无法满足的前提。

import { DYNAMIC_WORKFLOW_SKILL_NAME } from "@zcode/contracts";
import type { ToolHandlerFailure, ToolInputResolutionContext } from "../types.js";

/**
 * 「技能未加载」的稳定错误码。与四个工具的入参级 400 分开：调用方要能不靠文本区分「参数给错了」
 * 与「先去读技能」——前者改参数，后者多一次 Skill 调用。
 */
export const WORKFLOW_SKILL_NOT_LOADED_CODE = 428;

/** 门在场时的判据；单独导出供探针实现复用。 */
export function isDynamicWorkflowSkillLoaded(context: ToolInputResolutionContext): boolean {
  return context.hasLoadedSkill?.(DYNAMIC_WORKFLOW_SKILL_NAME) ?? true;
}

/**
 * 没读过技能就拒绝。返回 `undefined` 表示放行：技能已加载，或本会话没有探针（见文件头）。
 *
 * @param toolName 拒绝文案里点名的工具，让模型知道重试哪一个。
 */
export function requireDynamicWorkflowSkill(
  context: ToolInputResolutionContext,
  toolName: string,
): ToolHandlerFailure | undefined {
  if (isDynamicWorkflowSkillLoaded(context)) return undefined;
  return {
    result: false,
    errorCode: WORKFLOW_SKILL_NOT_LOADED_CODE,
    message: `${toolName} needs the \`${DYNAMIC_WORKFLOW_SKILL_NAME}\` skill loaded in this session before it accepts a script. Call the Skill tool with skill "${DYNAMIC_WORKFLOW_SKILL_NAME}" first — it carries the facade declarations the script is checked against, the authoring rules and this tool's full contract — then call ${toolName} again. Nothing was started.`,
  };
}

/** CreateWorkflow 的例外：按名字跑一个保存的工作流不是写脚本，不需要技能。 */
export function createWorkflowNeedsSkill(input: unknown): boolean {
  const fields = asRecord(input);
  if (fields === undefined) return true;
  const runsSavedOnly =
    fields.saved !== undefined && fields.script === undefined && fields.path === undefined;
  return !runsSavedOnly;
}

/** AmendWorkflow 的例外：只改设定（`path` 与 `script` 都不带）沿用前驱的脚本，不是写脚本。 */
export function amendWorkflowNeedsSkill(input: unknown): boolean {
  const fields = asRecord(input);
  if (fields === undefined) return true;
  return fields.script !== undefined || fields.path !== undefined;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}
