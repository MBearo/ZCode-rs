// ============================================================
// CreateWorkflow Tool Handler
// ============================================================
// Typechecks a dynamic-workflow script against the facade, and — once the user has
// confirmed at the gate — starts the run in the background through
// `DynamicWorkflowRunPort`.
//
// 两条路径，由端口是否注入决定：
//   1. 端口在场（生产接线）：clean compile + Allow → port.submit → 输出
//      {status:"backgrounded", backgroundTaskId: runId}，结果经后台通知管线回到主 agent。
//   2. 端口缺席（未接线的宿主、单测）：保持接线前的占位行为——只回诊断与因果图，什么都不执行。
//
// 坏脚本路径与端口无关且完全不变：`prepareApproval` 裁掉弹窗，handler 直接回诊断，
// 不启动、不建 run（让用户批准一段编不过的代码，只会用一个无效果的决策打断 agent 自己的
// 改错重试回路）。

import {
  CreateWorkflowInputJsonSchema,
  CreateWorkflowInputSchema,
  CreateWorkflowOutputJsonSchema,
  CreateWorkflowOutputSchema,
  type CreateWorkflowInput,
  type CreateWorkflowOutput,
  type ModelMessageContent,
  type TraceContext,
  createWorkflowPhaseAlongside,
  createWorkflowPhaseNames,
} from "@zcode/contracts";
import type { ToolApprovalGate, ToolEntry, ToolExecutionContext, ToolHandler } from "../types.js";
import { CREATE_WORKFLOW_TOOL_DESCRIPTION } from "./create-workflow-description.js";
import {
  resolveCreateWorkflowInput,
  validateCreateWorkflowSource,
} from "./create-workflow-source.js";
import { describeWorkflowSubagentModel, parseWorkflowSubagentModel } from "./model-reference.js";
import { boundGraphOfAnalysis, displayOfAnalysis } from "./workflow-analysis-display.js";
import { recordAuthoredWorkflowDraft } from "./workflow-draft-read-state.js";
import { resolveWorkflowDraftName, writeWorkflowDraft } from "./workflow-drafts.js";
import {
  formatWorkflowDiagnosticLines,
  workflowLaunchedScriptSentence,
  workflowSavedDraftNote,
  workflowScriptFileNote,
  type WorkflowScriptLocation,
} from "./workflow-script-notes.js";
import { analyzeScript } from "./workflow-script-analysis.js";
import { describeWorkflowScriptPath } from "./workflow-script-path.js";
import { createWorkflowNeedsSkill, requireDynamicWorkflowSkill } from "./workflow-skill-gate.js";

const CREATE_WORKFLOW_TOOL_NAME = "CreateWorkflow";
const CREATE_WORKFLOW_TIMEOUT_MS = 15_000;
const CREATE_WORKFLOW_MODEL_BYTES = 24_000;

// 「没执行」的 NOTE 按路径分成两句，各自只说该路径为真的事。合并成一句常量的旧写法
// （PLACEHOLDER_HINT）在引擎接线后两个断言都成了谎言：它说执行模型仍在开发中、说本工具只做
// 类型检查，而干净脚本 + 端口在场早已真启动一个后台 run。文案是模型唯一的读者，说谎的代价是
// 它据此放弃提交或重复提交。
export const DIAGNOSTICS_NOT_EXECUTED_NOTE =
  "NOTE: The workflow was NOT executed — fix the errors above and resubmit.";
export const EXECUTION_UNAVAILABLE_NOTE =
  "NOTE: The workflow was NOT executed — workflow execution is not available in this session, so the script was only typechecked.";

const createWorkflowHandler: ToolHandler = async (input, context) => {
  // 走到这里输入已经过 resolveInput 归一化：`script` 一定在场，两条来源在此完全同形。
  // `saved` 只是来龙去脉（run 标签兜底与实参持久化读它），执行一个字节都不读它——因此
  // 一个 hook 若在归一化之后改写 `saved`，是**刻意无效**的，改不了将要跑的东西。
  const parsed = CreateWorkflowInputSchema.parse(input) as CreateWorkflowInput;
  const script = parsed.script;
  if (script === undefined) {
    // 到不了：validateInput 已挡掉「两个都不给」，resolveInput 会把 saved 填成 script。
    // 真发生了说明有人绕过了 executor 的生命周期，说出来好过静默跑一段空脚本。
    throw new Error("CreateWorkflow handler received input without a resolved script");
  }
  const saved = parsed.saved;
  const cwd = context.workingDirectory;

  const analysis = analyzeScript(script);
  const { diagnostics, ok } = analysis;
  // 确认窗与持久化输出读的是同一份静态分析：中间没有任何模型调用改写名字。
  const causalityGraph = boundGraphOfAnalysis(analysis);

  // 内联脚本的工作副本**无论编译结果如何**都落盘：
  // 编不过的脚本走不到确认窗，handler 是它唯一必经的地方，不写就等于「唯一需要被编辑的那份
  // 脚本反而没有文件」。分析排在前面只为取名：没有 `name` 时文件名取第一个阶段名，而阶段名
  // 在图上。saved 来源的拷贝已在 resolveInput 里写过，`path` 来源不写。
  const inlineDraft =
    saved === undefined && parsed.path === undefined
      ? await writeWorkflowDraft({
          cwd,
          name: resolveWorkflowDraftName(parsed.name, causalityGraph),
          source: script,
        })
      : undefined;
  // 这份草稿的字节就是模型本次的 `script`：记作它写过的文件，下一次 Edit 不必先 Read。
  if (inlineDraft !== undefined) {
    await recordAuthoredWorkflowDraft(context, {
      path: inlineDraft.path,
      source: script,
      toolName: "CreateWorkflow",
    });
  }
  const location = describeScriptLocation(parsed, inlineDraft?.path, cwd);

  if (!ok) {
    return {
      diagnostics,
      ok,
      response: [
        // 保存的定义编不过是**它的**问题，不是这次调用的输入问题；不点名文件的话，模型
        // 会以为是自己刚才写错了什么，然后原样重试。
        saved === undefined
          ? "The workflow script has errors:"
          : `The saved workflow '${saved.name}'${saved.path === undefined ? "" : ` (${saved.path})`} has errors:`,
        // 有文件就按**文件行**报（行号要能直接粘进一次 `Edit`）；输出里的 `diagnostics` 数组
        // 保持正文行不变——转录面画的是正文。
        ...formatWorkflowDiagnosticLines(diagnostics, location),
        "",
        diagnosticsNote(parsed, location, cwd),
      ].join("\n"),
      ...(causalityGraph === undefined ? {} : { causalityGraph }),
    } satisfies CreateWorkflowOutput;
  }

  const port = context.dynamicWorkflowRunPort;
  if (port === undefined) {
    // 未接线的宿主：保持占位语义。刻意不降级成"假装启动了"——模型据此会去等一个永不到来的通知。
    return {
      diagnostics,
      ok,
      response: `The workflow script compiled cleanly.\n\n${EXECUTION_UNAVAILABLE_NOTE}`,
      ...(causalityGraph === undefined ? {} : { causalityGraph }),
    } satisfies CreateWorkflowOutput;
  }

  // run 记下的永远是**绝对路径**：它是 run 身份的一部分，而会话的工作目录会变。模型面的写法
  // 由 `location.described` 负责（同一个绝对路径的另一种写法，不是另一个来源）。
  const scriptPath = resolveSubmittedScriptPath(parsed, inlineDraft?.path);

  // 提交失败（编译产物损坏、journal 不可用）向上冒泡成工具调用失败：绝不吞成一个带
  // backgroundTaskId 的成功输出，那会让后台追踪器去轮询一个不存在的 run。
  const submitted = await port.submit(
    {
      scriptText: script,
      cwd: context.workingDirectory,
      // 可选的展示名一路落到 dwf_run.name（submit → EngineConfig → createRun），不能只留在
      // 工具行与任务标题的兜底链上——否则跨会话枚举出来的 run 只能是一串裸 runId。
      ...(parsed.name === undefined ? {} : { name: parsed.name }),
      // 实参走与 name / scriptText / cwd 完全相同的元数据路：submit → EngineConfig →
      // createRun 写 dwf_run.args_json → 沙箱注入。内联 run 没有实参，字段整个缺席，
      // 沙箱侧把缺席解读为 `{}`（不变式 7：`args` 恒有定义）。
      // 两个来源各有各的落点：`saved` 的在 `saved.args`，`path` 的在顶层 `args`（模型面就是
      // 这么写的），两者不可能同时在场（`validateInput` 把 `args` 与 `path` 绑死）。
      ...(() => {
        const args = parsed.args ?? saved?.args;
        return args === undefined ? {} : { args };
      })(),
      parentSessionId: context.sessionId,
      toolCallId: context.toolCallId,
      // 声明阶段表随提交走进 run-launched，侧栏迷你轨道据此画站点。「同时在跑」表与它
      // 同源同行：下标指向同一张表，分开算会错位。
      ...(() => {
        const phaseNames = createWorkflowPhaseNames(causalityGraph);
        if (phaseNames === undefined) return {};
        const phaseAlongside = createWorkflowPhaseAlongside(causalityGraph);
        return { phaseNames, ...(phaseAlongside === undefined ? {} : { phaseAlongside }) };
      })(),
      // 并发上界已在 resolveInput 里钳进 `[1, 天花板]`（确认窗显示的就是将要生效的值）；
      // 缺席即天花板，所以不造空壳键。
      ...(parsed.max_concurrency === undefined ? {} : { maxConcurrency: parsed.max_concurrency }),
      // 子代理模型同样已在 resolveInput 里解析成规范形（解不出来的调用根本走不到这里），
      // 所以这里只是把那个字符串拆回结构化选型。缺席即继承会话模型，不造空壳键——端口按
      // 「字段在场 = 这次 run 显式选过模型」读它。
      ...(() => {
        const subagentModel = parseWorkflowSubagentModel(parsed.subagent_model);
        return subagentModel === undefined ? {} : { subagentModel };
      })(),
      // 脚本的家随提交走进 `run-launched`，终态通知与 `GetWorkflowRun` 再从那里读回来。草稿写不下去时字段整个缺席：
      // 端口按「字段在场 = 这个 run 有个可编辑的文件」读它，一个 undefined 会让那句话变成谎话。
      ...(scriptPath === undefined ? {} : { scriptPath }),
      trace: resolveTraceContext(context),
    },
    { signal: context.abortSignal },
  );

  const { runId } = submitted;

  return {
    diagnostics,
    ok,
    // 文案照 backgrounded Bash（bash-model-content.ts）：给出 id、说明仍在跑、
    // 明确结果以通知形式回来。占位提示在这条路径上必须消失。
    // 模型拿到 backgrounded 输出后立刻用 TaskOutput 阻塞等待，
    // 把异步 run 变成了同步等待——文案必须显式劝阻默认轮询（用户显式要求等待时
    // TaskOutput 仍然可用，这里只改默认引导，不改工具语义）。
    response: `The workflow script compiled cleanly and the run started in the background with ID: ${runId}. It is still running — you will be notified with the final output when it completes. Do not wait for it or poll it with TaskOutput; continue with other work unless the user asked you to wait.${describeWorkflowConcurrencyLimit(parsed.max_concurrency, port.concurrencyCeiling?.())}${describeWorkflowSubagentModel(parsed.subagent_model)}${location === undefined ? "" : workflowLaunchedScriptSentence(location)}`,
    status: "backgrounded",
    backgroundTaskId: runId,
    ...(causalityGraph === undefined ? {} : { causalityGraph }),
  } satisfies CreateWorkflowOutput;
};

/**
 * 这次 run 的脚本文件是哪一个（绝对路径，随提交进 journal）：`path` 来源就是那个文件，
 * `saved` 来源是刚写下的拷贝，内联是刚写下的草稿。三者都可能缺席（草稿写不下去）。
 */
function resolveSubmittedScriptPath(
  parsed: CreateWorkflowInput,
  inlineDraft: string | undefined,
): string | undefined {
  if (parsed.path !== undefined) return parsed.path;
  if (parsed.saved !== undefined) return parsed.saved.draft;
  return inlineDraft;
}

/** 脚本文件在模型面的身份；没有文件（草稿写不下去）时缺席，文案随之退回旧的那一套。 */
function describeScriptLocation(
  parsed: CreateWorkflowInput,
  inlineDraft: string | undefined,
  cwd: string,
): WorkflowScriptLocation | undefined {
  const absolute = resolveSubmittedScriptPath(parsed, inlineDraft);
  if (absolute === undefined) return undefined;
  return {
    // `path` 来源的文件是模型自己给的，不是工具刚写下的——动词因此不同。
    kind: parsed.path === undefined ? "draft" : "path",
    described: describeWorkflowScriptPath(absolute, cwd),
    lineOffset: parsed.script_line_offset ?? 0,
  };
}

/**
 * 编不过时的 NOTE。三种情形：saved 来源点名它抄自哪个定义，其余有文件的点名那个文件，
 * 没有文件的保留改动之前的老话（那条路径上模型确实只能再内联提交一次）。
 */
function diagnosticsNote(
  parsed: CreateWorkflowInput,
  location: WorkflowScriptLocation | undefined,
  cwd: string,
): string {
  const saved = parsed.saved;
  if (location === undefined) {
    return saved === undefined
      ? DIAGNOSTICS_NOT_EXECUTED_NOTE
      : "NOTE: The workflow was NOT executed — the saved file needs fixing (edit it, or save a corrected version).";
  }
  if (saved !== undefined) {
    return workflowSavedDraftNote({
      savedName: saved.name,
      // 定义本身也按模型面的写法给：它接下来若要改定义，走的是 `SaveWorkflow`，但读到一个
      // 绝对路径而其余路径都是工作区相对的，会让人以为那是另一台机器上的东西。
      savedPath:
        saved.path === undefined ? location.described : describeWorkflowScriptPath(saved.path, cwd),
      draft: location.described,
    });
  }
  return workflowScriptFileNote(location);
}

/**
 * 生效的并发上界在结果文案里的一句话（`AmendWorkflow` 共用）。**只在设了上界时出现**：跑在
 * 天花板上的 run 没有可说的，多一句「至多 N 个」只会让模型以为自己设过什么。
 *
 * 正好等于天花板时点明是本机上限——那说明模型要的数被压低了，不说破的话它会把「至多 32」
 * 当成已生效，并在用户追问时复述一个假数。
 */
export function describeWorkflowConcurrencyLimit(
  limit: number | undefined,
  ceiling: number | undefined,
): string {
  if (limit === undefined) return "";
  const subject = limit === 1 ? "1 subagent runs" : `${limit} subagents run`;
  return ` At most ${subject} at once${limit === ceiling ? " (this machine's maximum)" : ""}.`;
}

/**
 * 端口契约要求 `trace` 非可选，而 `context.traceContext` 是可选的，所以按离散字段合成。
 */
export function resolveTraceContext(context: ToolExecutionContext): TraceContext {
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
 * 判断"运行这段脚本"值不值得打断用户，并构造确认窗要渲染的预览。
 *
 * **对两条来源是同一段代码**：走到这里输入已被 `resolveInput` 归一化，`script` 一定在场，
 * 所以这里既不知道也不需要知道脚本是内联写的还是从磁盘读的。saved run 的确认窗因此与内联
 * 逐字节同形——这正是「同一段脚本经两条路径产出的 display 完全相同」那条测试钉住的东西。
 *
 * 两种失败形态直接放行给 handler 而不弹窗：输入不合 schema、编译不过。让用户去批准一段
 * 编不过的代码，只会用一个不产生任何效果的决策打断 agent 自己的改错重试回路。（解析失败
 * 更早就在 `resolveInput` 里收口了，根本到不了这里。）
 */
function prepareCreateWorkflowApproval(input: unknown): ToolApprovalGate {
  const parsed = CreateWorkflowInputSchema.safeParse(input);
  if (!parsed.success || parsed.data.script === undefined) return { gate: "proceed" };

  const analysis = analyzeScript(parsed.data.script);
  if (!analysis.ok) return { gate: "proceed" };

  // 弹窗自带标题并以图为主体；display 与直接启动的启动轮元数据同一构造函数。
  const display = displayOfAnalysis(analysis);
  return { gate: "ask", ...(display ? { display } : {}) };
}

export const createWorkflowToolEntry: ToolEntry = {
  capability:
    "Typecheck a dynamic-workflow TypeScript script against the facade and, once confirmed, start the run in the background",
  metadata: {
    name: CREATE_WORKFLOW_TOOL_NAME,
    description: CREATE_WORKFLOW_TOOL_DESCRIPTION,
    // 引擎接线后这个调用会启动一个执行子进程与多个 actor 会话；只读声明随执行语义翻面
    readOnly: false,
    destructive: false,
    concurrentSafe: true,
    timeoutMs: CREATE_WORKFLOW_TIMEOUT_MS,
    maxOutputBytes: CREATE_WORKFLOW_MODEL_BYTES,
    sideEffectScope: "none",
    riskLevel: "low",
    needsApproval: true,
  },
  handler: createWorkflowHandler,
  // 来源二选一只对模型入参成立（归一化后两者同时在场是合法执行态），所以它住在这里而
  // 不是 schema 上——见 CreateWorkflowInputSchema 的注释。
  validateInput: (input) => validateCreateWorkflowSource(input),
  // 全流程唯一一次读盘。此后 hook、权限规则、确认窗与 handler 看到的都是同一份字节。
  // 天花板同在这里读：钳制必须发生在确认窗之前，否则用户批准的是一个不会生效的数。
  // 模型目录同在这里读：`subagent_model` 必须在确认窗之前解析成规范形，否则用户批准的是一个
  // 还没被认出来的名字，而解不出来的调用会在批准之后才失败。
  resolveInput: (input, context) => {
    // 技能门先于一切解析：没读过 dynamic-workflows 就拒绝提交脚本（saved 来源例外，见 gate 模块）。
    if (createWorkflowNeedsSkill(input)) {
      const refused = requireDynamicWorkflowSkill(context, CREATE_WORKFLOW_TOOL_NAME);
      if (refused) return refused;
    }
    return resolveCreateWorkflowInput(
      input,
      context.workingDirectory ?? ".",
      context.dynamicWorkflowRunPort?.concurrencyCeiling?.(),
      context.modelCatalogPort,
    );
  },
  prepareApproval: prepareCreateWorkflowApproval,
  inputSchema: CreateWorkflowInputJsonSchema,
  outputSchema: CreateWorkflowOutputJsonSchema,
  runtimeInputSchema: CreateWorkflowInputSchema,
  runtimeOutputSchema: CreateWorkflowOutputSchema,
  formatModelContent: formatCreateWorkflowModelContent,
  permission: {
    permission: "createWorkflow",
    // 诊断用，不面向用户：确认窗自己渲染本地化标题，UI 也会过滤读起来像内部信息的 reason。
    reason: "createWorkflow.runConfirmation: user must confirm running the analyzed script",
    riskLevel: "low",
    sideEffectScope: "none",
    needsApproval: true,
    patternSources: ["toolName"],
    alwaysAllowPatternSources: ["toolName"],
    denyPriority: "beforeAsk",
    // workflow 是一整块执行 + 模型调用，任何权限模式（含 yolo / plan）都要先问。
    alwaysAsk: true,
    // 「每次调用都是不同的脚本」对保存的 workflow 不再成立：按名字
    // 调用时，同一个名字每次都是"同一个"工作流。但「同一个」工作流也不足以支撑免确认——
    // 每次仍要确认，理由有两条更硬的：运行一次
    // 要花钱且有真实副作用；而保存的文件在批准之后随时可能被改动（手改、git pull、别人提交），
    // 所以"上次批准过这个名字"根本不能推出"这次要跑的还是那段代码"。**持久**确认永不减免。
    //
    // 放开的是**会话作用域**：用户看过并批准了本会话第一个脚本之后，
    // 可以选「Always allow in this session」让本会话后续的 CreateWorkflow 免确认。授权只活在
    // PermissionService 实例的内存里，重启 / 冷恢复 / `/new` 都从零开始。
    askOptions: { allowAlways: "session" },
  },
  resultBudget: {
    maxInlineBytes: CREATE_WORKFLOW_MODEL_BYTES,
    maxModelBytes: CREATE_WORKFLOW_MODEL_BYTES,
    strategy: "truncate",
    preview: {
      maxBytes: CREATE_WORKFLOW_MODEL_BYTES,
      direction: "head",
    },
  },
  timeout: {
    defaultMs: CREATE_WORKFLOW_TIMEOUT_MS,
    maxMs: CREATE_WORKFLOW_TIMEOUT_MS,
    allowCallOverride: false,
  },
  cancellation: {
    supported: false,
    cleanup: "none",
    userVisibleMessage: "CreateWorkflow typechecks synchronously and cannot be cancelled",
  },
  trace: {
    required: true,
    propagateToAdapters: true,
    recordInput: "summary",
    recordOutput: "summary",
  },
};

function formatCreateWorkflowModelContent(output: unknown): ModelMessageContent {
  const parsed = CreateWorkflowOutputSchema.safeParse(output);
  if (!parsed.success) return "CreateWorkflow returned an invalid result.";
  return parsed.data.response;
}
