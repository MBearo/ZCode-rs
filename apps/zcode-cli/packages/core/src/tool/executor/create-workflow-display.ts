/**
 * CreateWorkflow 的结果卡 display 构造。从 result-display.ts 拆出（400 行纪律）：
 * 与 workflow-observation-display.ts 同族——按工具名分派、safeParse 输出 schema、
 * display 侧独立限长。
 */

import {
  AMEND_WORKFLOW_TOOL_NAME,
  CREATE_WORKFLOW_DISPLAY_MAX_DIAGNOSTICS,
  CREATE_WORKFLOW_DISPLAY_MAX_MESSAGE_CHARS,
  CREATE_WORKFLOW_TOOL_NAME,
  CreateWorkflowOutputSchema,
  type ToolResultDisplayPayload,
} from "@zcode/contracts";

/**
 * ⚠ 这个投影的字段集合是**冻结**的（contracts 的 schema 注释说明了为什么）。gate 专属的
 * 事实——比如 saved run 的来源与解析出的脚本——一律走**工具入参**通道，不上 display。
 */
export function createCreateWorkflowDisplay(
  toolName: string,
  output: unknown,
): ToolResultDisplayPayload | undefined {
  // 两个启动工具共用同一个 display kind：图、草稿笔与诊断卡在 UI 侧只有一份实现
  if (toolName !== CREATE_WORKFLOW_TOOL_NAME && toolName !== AMEND_WORKFLOW_TOOL_NAME) {
    return undefined;
  }
  const parsed = CreateWorkflowOutputSchema.safeParse(output);
  if (!parsed.success) return undefined;
  // 就地调并发没有 display：这条路一行脚本都没编译，而 `create_workflow` 这块载荷的 `ok` 在 UI 上读作
  // 「已编译」。判据是**显式的 `retuned` 块**而不是形状——「ok 且没有 status」在本工具上还有
  // 「没有 run 端口、只 typecheck」这条来路。工具卡因此退回响应正文那一段。
  if (parsed.data.retuned !== undefined) return undefined;

  const { causalityGraph, diagnostics, ok } = parsed.data;
  // display 不经过 tool result budget：诊断条数与单条 message 长度都必须在进入实时事件和
  // 持久化 metadata 前独立限长，避免类型错误把 continuous/replayable 消息扩成无界载荷。
  // causalityGraph 已在工具输出边界限长（handler 的 boundCausalityGraph + 输出 schema），
  // 直接透传。
  const errorCount = diagnostics.length;
  const bounded = diagnostics
    .slice(0, CREATE_WORKFLOW_DISPLAY_MAX_DIAGNOSTICS)
    .map((diagnostic) => ({
      line: diagnostic.line,
      column: diagnostic.column,
      code: diagnostic.code,
      message: diagnostic.message.slice(0, CREATE_WORKFLOW_DISPLAY_MAX_MESSAGE_CHARS),
    }));
  const truncated = errorCount > bounded.length;

  return {
    kind: "create_workflow",
    ok,
    errorCount,
    diagnostics: bounded,
    ...(causalityGraph === undefined ? {} : { causalityGraph }),
    ...(truncated ? { truncated: true } : {}),
  };
}
