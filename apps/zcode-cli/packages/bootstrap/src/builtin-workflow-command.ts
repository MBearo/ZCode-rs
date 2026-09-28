import { expandCustomCommandPrompt, type CustomCommandContent } from "@zcode/contracts";
import { BUILTIN_ZCODE_SLASH_COMMAND_HELP_ENTRIES } from "@zcode/shared";
import { DYNAMIC_WORKFLOW_SKILL_NAME } from "./app/bundled-skills.js";

/**
 * 内置 `/workflow` 命令。
 * 命令正文随 CLI 编译，与 `/init` 同为代码定义的 prompt 命令，不依赖可卸载插件。
 * 命令名进入保留字表，用户或插件的同名命令不会被展开。
 *
 * 正文复用 contracts 的 custom command 展开规则：替换 $ARGUMENTS，并补充 `skills:` 前言。
 */
export const BUILTIN_WORKFLOW_COMMAND_NAME = "workflow";

const helpEntry = BUILTIN_ZCODE_SLASH_COMMAND_HELP_ENTRIES.find(
  (entry) => entry.name === BUILTIN_WORKFLOW_COMMAND_NAME,
);
if (!helpEntry) {
  // 共享 help 表是保留字、TUI 候选与 App 目录的唯一来源；条目缺席时命令根本不可寻址。
  throw new Error(`Missing builtin slash command help entry: ${BUILTIN_WORKFLOW_COMMAND_NAME}`);
}

const USAGE_PREFIX = `/${BUILTIN_WORKFLOW_COMMAND_NAME} `;
export const BUILTIN_WORKFLOW_COMMAND_DESCRIPTION = helpEntry.summary;
export const BUILTIN_WORKFLOW_COMMAND_ARGUMENT_HINT = helpEntry.usage.startsWith(USAGE_PREFIX)
  ? helpEntry.usage.slice(USAGE_PREFIX.length)
  : "";

/** `$ARGUMENTS` 必须在场：否则展开会把参数追加成位置不受控的 "User arguments:" 尾块。 */
const BUILTIN_WORKFLOW_COMMAND_BODY = [
  `Use the \`${DYNAMIC_WORKFLOW_SKILL_NAME}\` skill to design and launch a dynamic workflow for this request:`,
  "",
  "$ARGUMENTS",
  "",
  "Decide the subagent topology before writing any code: how many subagents, which of them",
  "share a context, what result each one returns. Then write the script and call the",
  "`CreateWorkflow` tool. (`CreateWorkflow` is the dynamic-workflow tool. Do not use the",
  "legacy `Workflow` tool, and do not substitute the `Agent` tool.)",
  "",
].join("\n");

export const BUILTIN_WORKFLOW_COMMAND: CustomCommandContent = {
  bytesRead: Buffer.byteLength(BUILTIN_WORKFLOW_COMMAND_BODY),
  content: BUILTIN_WORKFLOW_COMMAND_BODY,
  metadata: {
    allowedTools: [],
    argumentHint: BUILTIN_WORKFLOW_COMMAND_ARGUMENT_HINT,
    description: BUILTIN_WORKFLOW_COMMAND_DESCRIPTION,
    disableNonInteractive: false,
    frontmatterKeys: ["description", "argument-hint", "skills"],
    name: BUILTIN_WORKFLOW_COMMAND_NAME,
    path: `builtin:${BUILTIN_WORKFLOW_COMMAND_NAME}`,
    rootPath: "builtin:",
    scope: "system",
    skills: [DYNAMIC_WORKFLOW_SKILL_NAME],
    source: "zcode",
  },
  sizeBytes: Buffer.byteLength(BUILTIN_WORKFLOW_COMMAND_BODY),
  truncated: false,
};

export function expandBuiltinWorkflowCommandPrompt(args: string): string {
  return expandCustomCommandPrompt({ args, command: BUILTIN_WORKFLOW_COMMAND }).prompt;
}
