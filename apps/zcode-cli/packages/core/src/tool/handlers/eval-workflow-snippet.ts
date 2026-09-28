// ============================================================
// EvalWorkflowSnippet Tool Handler
// ============================================================
// 动态工作流创作的实验通道：同步编译并运行一段
// scratch-facade snippet，经 `DynamicWorkflowSnippetPort` 交给 bootstrap 的执行面。
// 完全瞬态：无后台任务、无持久化。确认门只落在携带 world.run 命令的 snippet 上
// （prepareApproval）。
//
// 两条路径，由端口是否注入决定：
//   1. 端口在场（生产接线）：编译 + 执行，返回 {ok, diagnostics, logs, response}。
//   2. 端口缺席（未接线的宿主、单测）：诚实的业务失败——绝不假装执行过。

import {
  EVAL_WORKFLOW_SNIPPET_DEFAULT_TIMEOUT_MS,
  EVAL_WORKFLOW_SNIPPET_SOURCE_ERROR,
  EVAL_WORKFLOW_SNIPPET_TOOL_NAME,
  EvalWorkflowSnippetInputJsonSchema,
  EvalWorkflowSnippetInputSchema,
  EvalWorkflowSnippetOutputJsonSchema,
  EvalWorkflowSnippetOutputSchema,
  serializeWorkflowArtifact,
  type EvalWorkflowSnippetInput,
  type EvalWorkflowSnippetOutput,
  type ModelMessageContent,
  type TraceContext,
} from "@zcode/contracts";
import {
  collectDiagnostics,
  collectSites,
  collectWorldRunCommands,
  createWorkflowProgram,
  SNIPPET_FACADE_DTS,
} from "@zcode/dynamic-workflow";
import type {
  ToolApprovalGate,
  ToolEntry,
  ToolExecutionContext,
  ToolHandler,
  ToolInputResolutionResult,
  ToolInputValidationResult,
} from "../types.js";
import { EVAL_WORKFLOW_SNIPPET_TOOL_DESCRIPTION } from "./eval-workflow-snippet-description.js";
import { readWorkflowScriptFile } from "./workflow-path-source.js";
import { formatWorkflowDiagnosticLines } from "./workflow-script-notes.js";
import { describeWorkflowScriptPath } from "./workflow-script-path.js";
import { requireDynamicWorkflowSkill } from "./workflow-skill-gate.js";

// 工具级超时是**外层兜底**，不是 snippet 的墙钟：入参 timeoutMs（≤600s）驱动 harness
// 到点 kill 子进程并正常返回失败；这里再留编译与收尾的余量。双时钟语义刻意不存在——
// 正常路径永远是内层先到。
const EVAL_WORKFLOW_SNIPPET_TOOL_TIMEOUT_MS = 660_000;
const EVAL_WORKFLOW_SNIPPET_MODEL_BYTES = 24_000;

const NOT_EXECUTED_NOTE =
  "NOTE: The snippet was NOT executed — fix the errors above and call the tool again.";
const UNAVAILABLE_NOTE =
  "NOTE: The snippet was NOT executed — snippet evaluation is not available in this session.";

const EVAL_WORKFLOW_SNIPPET_FAILURE_CODE = 400;

/**
 * 来源二选一，只对**模型发出的**入参成立：归一化之后 `code` 与 `path` 同时在场是合法执行态
 * （理由与 `CreateWorkflow` 的同一条，见 create-workflow-source.ts）。
 */
function validateEvalWorkflowSnippetInput(input: unknown): ToolInputValidationResult {
  const parsed = EvalWorkflowSnippetInputSchema.safeParse(input);
  if (!parsed.success) return { result: true };
  const hasCode = parsed.data.code !== undefined;
  const hasPath = parsed.data.path !== undefined;
  if (hasCode === hasPath) {
    return {
      result: false,
      errorCode: EVAL_WORKFLOW_SNIPPET_FAILURE_CODE,
      message: EVAL_WORKFLOW_SNIPPET_SOURCE_ERROR,
    };
  }
  return { result: true };
}

/**
 * `path` 来源归一化成 `code`。整个文件就是片段（不解析元数据块），路径写成绝对形——此后确认门
 * （`prepareApproval` 读 `code` 找 `world.run` 站点）与 handler 看到的都是同一串字节。
 */
async function resolveEvalWorkflowSnippetInput(
  input: unknown,
  cwd: string,
): Promise<ToolInputResolutionResult> {
  const parsed = EvalWorkflowSnippetInputSchema.safeParse(input);
  if (!parsed.success || parsed.data.path === undefined) return { result: true, input };
  const read = await readWorkflowScriptFile({
    cwd,
    inputPath: parsed.data.path,
    parseFrontmatter: false,
  });
  if (!read.ok) {
    return { result: false, errorCode: EVAL_WORKFLOW_SNIPPET_FAILURE_CODE, message: read.message };
  }
  return {
    result: true,
    input: {
      ...parsed.data,
      code: read.file.source,
      path: read.file.path,
    } satisfies EvalWorkflowSnippetInput,
  };
}

const evalWorkflowSnippetHandler: ToolHandler = async (input, context) => {
  const parsed = EvalWorkflowSnippetInputSchema.parse(input) as EvalWorkflowSnippetInput;
  const code = parsed.code;
  if (code === undefined) {
    // 到不了：validateInput 挡掉「两个都不给」，resolveInput 会把 `path` 读成 `code`。
    throw new Error("EvalWorkflowSnippet handler received input without resolved code");
  }
  // 有文件就按**文件行**报诊断（片段没有元数据块，所以偏移恒为 0）。
  const location =
    parsed.path === undefined
      ? undefined
      : {
          kind: "path" as const,
          described: describeWorkflowScriptPath(parsed.path, context.workingDirectory),
          lineOffset: 0,
        };
  const startedAt = Date.now();

  const port = context.dynamicWorkflowSnippetPort;
  if (port === undefined) {
    // 未接线的宿主：诚实降级（CreateWorkflow 的 EXECUTION_UNAVAILABLE 同款语义）。
    return {
      ok: false,
      diagnostics: [],
      logs: [],
      response: UNAVAILABLE_NOTE,
      durationMs: 0,
    } satisfies EvalWorkflowSnippetOutput;
  }

  const result = await port.evalSnippet(
    {
      code,
      cwd: context.workingDirectory,
      timeoutMs: parsed.timeoutMs ?? EVAL_WORKFLOW_SNIPPET_DEFAULT_TIMEOUT_MS,
      trace: resolveTraceContext(context),
    },
    { signal: context.abortSignal },
  );
  const durationMs = Math.max(0, Date.now() - startedAt);

  if (result.kind === "diagnostics") {
    return {
      ok: false,
      diagnostics: result.diagnostics,
      logs: [],
      response: [
        "The snippet has errors:",
        ...formatWorkflowDiagnosticLines(result.diagnostics, location),
        "",
        NOT_EXECUTED_NOTE,
      ].join("\n"),
      durationMs,
    } satisfies EvalWorkflowSnippetOutput;
  }

  const logsSection = renderLogs(result.logs, result.logsTruncated);

  if (result.kind === "failed") {
    return {
      ok: false,
      diagnostics: [],
      logs: result.logs,
      response: [
        `The snippet failed (${result.error.code}): ${result.error.message}`,
        ...logsSection,
      ].join("\n"),
      durationMs,
    } satisfies EvalWorkflowSnippetOutput;
  }

  // completed。`undefined` 产物即「没有 return 值」——如实说，不去发明一个空对象。
  const serialized = serializeWorkflowArtifact(result.artifact);
  return {
    ok: true,
    diagnostics: [],
    logs: result.logs,
    response: [
      `The snippet completed in ${durationMs}ms.`,
      serialized === undefined ? "It returned no value." : `Return value:\n${serialized}`,
      ...logsSection,
    ].join("\n"),
    durationMs,
  } satisfies EvalWorkflowSnippetOutput;
};

/** logs 渲染进 response（formatModelContent 只交 response，logs 字段之外模型看不见它们）。 */
function renderLogs(logs: string[], truncated: boolean): string[] {
  if (logs.length === 0) return [];
  return [
    "",
    "Logs:",
    ...logs.map((message) => `- ${message}`),
    ...(truncated ? ["- … (logs truncated)"] : []),
  ];
}

/** 端口契约要求 trace 非可选；按离散字段合成，与 CreateWorkflow handler 同一处理。 */
function resolveTraceContext(context: ToolExecutionContext): TraceContext {
  return (
    context.traceContext ??
    ({
      traceId: context.traceId,
      spanId: context.spanId,
      parentSpanId: context.parentSpanId,
      sessionId: context.sessionId,
      turnId: context.turnId,
    } as TraceContext)
  );
}

/**
 * snippet 的确认门：只有携带
 * world.run 站点的 snippet 才值得打断用户——纯读 snippet（files/git + 纯计算）静默放行，
 * 编译不过的也放行（handler 经端口返回诊断，打断一段编不过的代码的确认没有意义，
 * 与 CreateWorkflow 的 gate 同一姿态）。
 *
 * v1 是**普通权限 ask**（无富预览）：display 的权限 schema 在已部署桌面上是 .strict()，
 * 新增载荷种类会让旧桌面丢弃整个 permission.requested 事件——确认门本身消失（同类
 * 回归曾实测复现）。命令集用户可直接读自
 * 入参代码（cmd 是编译期字面量，这正是字面量规则买到的可读性）。
 *
 * 这里的编译与端口侧的编译是两次（gate 在 core、执行在 bootstrap，ts.Program 不跨包共享）；
 * snippet 体量小，两次编译的代价换来 gate 不依赖端口在场。
 */
function prepareEvalWorkflowSnippetApproval(input: unknown): ToolApprovalGate {
  const parsed = EvalWorkflowSnippetInputSchema.safeParse(input);
  // 归一化之后 `code` 必在场（`path` 已被读成它）；缺席即有人绕过了生命周期，放行给 handler。
  if (!parsed.success || parsed.data.code === undefined) return { gate: "proceed" };
  const workflow = createWorkflowProgram(parsed.data.code, { facadeDts: SNIPPET_FACADE_DTS });
  if (collectDiagnostics(workflow.program).length > 0) return { gate: "proceed" };
  const { commands, diagnostics } = collectWorldRunCommands(workflow, collectSites(workflow));
  if (diagnostics.length > 0) return { gate: "proceed" };
  return commands.length > 0 ? { gate: "ask" } : { gate: "proceed" };
}

export const evalWorkflowSnippetToolEntry: ToolEntry = {
  capability:
    "Compile and synchronously run a small dynamic-workflow TypeScript snippet (world reads + pure logic) against the real workflow execution path, fully ephemerally",
  metadata: {
    name: EVAL_WORKFLOW_SNIPPET_TOOL_NAME,
    description: EVAL_WORKFLOW_SNIPPET_TOOL_DESCRIPTION,
    // world.run 落地后 snippet 可表达效应：readOnly 翻面，确认门在 prepareApproval
    // （携带 world.run 站点才 ask）。
    readOnly: false,
    destructive: false,
    concurrentSafe: true,
    timeoutMs: EVAL_WORKFLOW_SNIPPET_TOOL_TIMEOUT_MS,
    maxOutputBytes: EVAL_WORKFLOW_SNIPPET_MODEL_BYTES,
    sideEffectScope: "workspace",
    riskLevel: "low",
    needsApproval: true,
  },
  prepareApproval: prepareEvalWorkflowSnippetApproval,
  handler: evalWorkflowSnippetHandler,
  // 来源二选一只对模型入参成立；`path` 在归一化里读成 `code`，确认门与 handler 因此同形。
  validateInput: (input) => validateEvalWorkflowSnippetInput(input),
  resolveInput: (input, context) =>
    // 技能门先于 path 归一化（handlers/workflow-skill-gate.ts）：片段的语言规则也在技能里。
    requireDynamicWorkflowSkill(context, EVAL_WORKFLOW_SNIPPET_TOOL_NAME) ??
    resolveEvalWorkflowSnippetInput(input, context.workingDirectory ?? "."),
  inputSchema: EvalWorkflowSnippetInputJsonSchema,
  outputSchema: EvalWorkflowSnippetOutputJsonSchema,
  runtimeInputSchema: EvalWorkflowSnippetInputSchema,
  runtimeOutputSchema: EvalWorkflowSnippetOutputSchema,
  formatModelContent: formatEvalWorkflowSnippetModelContent,
  permission: {
    permission: "evalWorkflowSnippet",
    reason:
      "evalWorkflowSnippet.runConfirmation: snippets carrying world.run commands must be confirmed",
    riskLevel: "low",
    sideEffectScope: "workspace",
    needsApproval: true,
    // 输入是代码文本，没有路径主体：模式只按工具名匹配。
    patternSources: ["toolName"],
    alwaysAllowPatternSources: ["toolName"],
    denyPriority: "beforeAsk",
    // 普通权限姿态
    // alwaysAsk 只属于 CreateWorkflow——整块工作流 load-bearing 且昂贵，值得压过一切
    // 放行分支。snippet 廉价、瞬态，和 Bash 一样按标准模式 / 规则流程裁决（always-allow、
    // yolo 照常生效）；prepareApproval 仍在默认 ask 流程里把「纯读 snippet」与「编不过的
    // snippet」裁回 proceed，弹窗只落在真的携带命令的 snippet 上。
  },
  resultBudget: {
    maxInlineBytes: EVAL_WORKFLOW_SNIPPET_MODEL_BYTES,
    maxModelBytes: EVAL_WORKFLOW_SNIPPET_MODEL_BYTES,
    strategy: "truncate",
    preview: {
      maxBytes: EVAL_WORKFLOW_SNIPPET_MODEL_BYTES,
      direction: "head",
    },
  },
  timeout: {
    defaultMs: EVAL_WORKFLOW_SNIPPET_TOOL_TIMEOUT_MS,
    maxMs: EVAL_WORKFLOW_SNIPPET_TOOL_TIMEOUT_MS,
    allowCallOverride: false,
  },
  cancellation: {
    supported: true,
    // abort 信号一路到 harness，kill 沙箱子进程（bestEffort：收尾竞态里子进程可能已退出）。
    cleanup: "bestEffort",
    userVisibleMessage: "Snippet evaluation cancelled",
  },
  trace: {
    required: true,
    propagateToAdapters: true,
    recordInput: "summary",
    recordOutput: "summary",
  },
};

function formatEvalWorkflowSnippetModelContent(output: unknown): ModelMessageContent {
  const parsed = EvalWorkflowSnippetOutputSchema.safeParse(output);
  if (!parsed.success) return "EvalWorkflowSnippet returned an invalid result.";
  return parsed.data.response;
}
