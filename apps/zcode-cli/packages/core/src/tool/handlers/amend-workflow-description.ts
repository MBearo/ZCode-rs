// AmendWorkflow 的常驻描述。
//
// 缓存如何命中、省略即沿用的三个字段、`path` 与 `script` 两条来路、确认窗
// 何时出现，都在 `dynamic-workflows` 技能的「Tool reference」里，由技能门保证读过。这里只留
// 路由——什么情况该来修订而不是重建、不要先停、不要等——因为它决定的是「要不要调这个工具」，
// 必须在技能加载之前就被读到。

import { DYNAMIC_WORKFLOW_SKILL_NAME } from "@zcode/contracts";

export const AMEND_WORKFLOW_TOOL_DESCRIPTION = [
  "Amend an existing dynamic-workflow run with a revised script or revised settings. Starts a NEW run that supersedes the old one and imports its finished work as a cache, so only what you changed is paid for again. Works on ANY run of this project: completed, errored, stopped — or still running.",
  "",
  "When to use:",
  "- The run errored, or completed but needs one more stage: fix or extend the script and amend. Never rewrite the workflow from scratch with CreateWorkflow.",
  "- The run is STILL RUNNING and is visibly going wrong: amend it NOW, in one call. Do not TaskStop it first and do not wait for it to finish — this tool stops the running predecessor and starts the revision; the earlier you amend, the less is re-paid.",
  "- The user wants the same workflow with fewer subagents at once, its subagents on another model, or another name: amend with only that field and neither `path` nor `script`.",
  "- To continue a stopped run unchanged, use ResumeWorkflowRun instead.",
  "",
  `Load the \`${DYNAMIC_WORKFLOW_SKILL_NAME}\` skill with the Skill tool before revising a script: it carries the cache rules, what each omitted field keeps, and the confirmation rule. A call that passes \`path\` or \`script\` is refused until that skill has been loaded in this session; a settings-only call is not. Pass \`path\` (the run's script file, edited in place — the usual form) or \`script\` (the whole revised script inline), never both.`,
].join("\n");
