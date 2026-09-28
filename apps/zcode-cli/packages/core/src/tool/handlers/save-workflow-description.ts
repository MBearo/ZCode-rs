// SaveWorkflow 的常驻描述。
//
// 文件格式、实参声明与写作规则都在 `dynamic-workflows` 技能的「Tool
// reference」里，由技能门保证读过。留在这里的是唯一一条**在决定调不调之前**就必须看见的
// 规则——绝不主动保存。保存会在用户仓库里留下文件，而模型对「看起来挺通用」的判断远比用户
// 宽松；这条门槛必须常驻，不能等到技能加载之后。

import { DYNAMIC_WORKFLOW_SKILL_NAME } from "@zcode/contracts";

export const SAVE_WORKFLOW_TOOL_DESCRIPTION = [
  "Save a dynamic-workflow script with its metadata so it can be run again later by name (CreateWorkflow's `saved` source; ListSavedWorkflows lists them). The required `scope` decides whether it lives in this project or globally.",
  "",
  "NEVER call this tool unsolicited: saving writes a file into the user's repository, and that is their decision. When a workflow you just built looks reusable, suggest saving it in one sentence and wait; call SaveWorkflow only after the user agrees, or when the user asks directly.",
  "",
  `Load the \`${DYNAMIC_WORKFLOW_SKILL_NAME}\` skill with the Skill tool first: it carries the file format, the argument declarations and the authoring rules. The call is refused until that skill has been loaded in this session. Pass \`script\` (the body only) or \`script_path\` (a draft file, saved without re-emitting it), never both.`,
].join("\n");
