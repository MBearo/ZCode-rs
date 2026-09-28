import { randomUUID } from "node:crypto";
import {
  AMEND_WORKFLOW_TOOL_NAME,
  boundWorkflowLaunchMeta,
  createWorkflowPhaseAlongside,
  createWorkflowPhaseNames,
  type DynamicWorkflowRunRetuneResult,
  type TraceContext,
  type WorkflowSettingsAmendMeta,
} from "@zcode/contracts";
import {
  resolveAmendMaxConcurrency,
  resolveAmendSubagentModelChoice,
} from "../../tool/handlers/amend-workflow-resolve.js";
import { resolveKeptScriptFile } from "../../tool/handlers/amend-workflow-source.js";
import { parseWorkflowSubagentModel } from "../../tool/handlers/model-reference.js";
import {
  boundGraphOfAnalysis,
  displayOfAnalysis,
} from "../../tool/handlers/workflow-analysis-display.js";
import {
  resolveWorkflowDraftName,
  writeWorkflowDraft,
} from "../../tool/handlers/workflow-drafts.js";
import { analyzeScript } from "../../tool/handlers/workflow-script-analysis.js";
import type { ExecutableToolCall } from "../../tool/types.js";
import { traceContextToLogContext } from "../deps.js";
import type { AgentRuntimeInternal } from "../internal.js";
import { boundedCompileDiagnostics } from "./dynamic-workflow-run-start.js";
import {
  buildSettingsMessageText,
  enqueueSettingsTurn,
  fromTo,
  runSettingsOfSnapshot,
  type RunSettings,
} from "./dynamic-workflow-run-settings-turn.js";

export { buildSettingsMessageText } from "./dynamic-workflow-run-settings-turn.js";

/**
 * GUI「配置」的请求。两项设置守工具的三态：
 * 省略 = 沿用，`null` = 回到默认（会话模型 / 本机上限），值 = 设定。
 */
export interface AmendWorkflowRunSettingsInput {
  runId: string;
  subagentModel?: string | null;
  maxConcurrency?: number | null;
  traceContext?: TraceContext;
}

/** 拒绝词表与 shared 的 `workflowRunSettingsRejectionReasonSchema` 逐字相同。 */
export type AmendWorkflowRunSettingsRejection =
  | "not_found"
  | "not_configurable"
  | "unchanged"
  | "script_missing"
  | "model_unavailable"
  | "compile_failed"
  | "missing_boundaries"
  | "start_failed";

export type AmendWorkflowRunSettingsResult =
  | { ok: true; runId: string; toolCallId: string; supersededRunId?: string }
  | { ok: false; reason: AmendWorkflowRunSettingsRejection; message?: string };

/**
 * GUI「配置」：以同一份脚本、新的设置修订一个 run。
 *
 * 它是 `port.amend` 与 `port.retuneConcurrency` 的**第二个调用方**，与 `AmendWorkflow` 工具同构：
 * 同一段三态归一（`resolveAmendSubagentModelChoice` / `resolveAmendMaxConcurrency`，不复制）、同一条
 * 路由、同一个编译、同一个后台追踪器（合成一个 AmendWorkflow 描述子）。区别只在：不经模型轮、不开
 * 确认窗（用户在弹层里点「应用」就是同意，脚本也是他批准过的那一份），实参沿用前驱
 * （`inheritArgs`），并用一条排队的 controlOnly「设置轮」把这件事记进会话。
 *
 * 只改并发、run 又还在飞时在下面那个分叉处**就地生效**：同一个 runId、不停、不铸后继。
 *
 * 顺序固定：直到 `port.amend` / `port.retuneConcurrency` 之前的每一步失败都是零副作用——旧 run
 * 照旧在跑、没有新行、没有消息。
 */
export async function amendWorkflowRunSettings(
  this: AgentRuntimeInternal,
  input: AmendWorkflowRunSettingsInput,
): Promise<AmendWorkflowRunSettingsResult> {
  const traceContext = input.traceContext ?? this.rootTraceContext;
  const port = this.dynamicWorkflowRunPort;
  if (port === undefined || typeof port.amend !== "function") {
    // 能力只在端口带 amend 与 getScript 时注册；到这里还缺，是接线故障而不是用户输入。
    return { ok: false, reason: "start_failed", message: "dynamic workflow amend unavailable" };
  }

  // (1) 这个 run 必须存在、且属于本会话——命令发给哪个会话，就只能改那个会话自己的 run。
  const snapshot = await port.getTask(input.runId);
  if (snapshot === undefined || snapshot.parentSessionId !== this.sessionId) {
    return { ok: false, reason: "not_found" };
  }
  // (2) 已完成的 run 每个 ask 都会从缓存重放，新设置无处生效；被替代的 run 活的是它的后继。
  if (snapshot.runStatus === "completed" || snapshot.supersededBy !== undefined) {
    return { ok: false, reason: "not_configurable" };
  }
  // (3)(4) 两项设置的三态归一，与工具同一段代码。
  const current = runSettingsOfSnapshot(snapshot);
  const model = resolveAmendSubagentModelChoice(
    input.subagentModel,
    current.subagentModel,
    this.modelCatalogPort,
  );
  if (!model.ok) {
    // 目录缺席时的那句话是写给模型的（「omit subagent_model」），GUI 只要原因码；有目录时的
    // 解析诊断（候选名等）对人同样有用，随 message 走。
    return {
      ok: false,
      reason: "model_unavailable",
      ...(this.modelCatalogPort === undefined ? {} : { message: model.message }),
    };
  }
  const ceiling = port.concurrencyCeiling?.();
  const bound = resolveAmendMaxConcurrency(input.maxConcurrency, current.maxConcurrency, ceiling);
  // 等于天花板的界就是「没有自己的界」：快照只在低于天花板时带 maxConcurrency，两边同一个读法，
  // 「未改」的比较才成立（弹层把步进器推到顶也发 null，这里兜住直接给数的调用方）。
  const nextBound =
    bound.max_concurrency === undefined || bound.max_concurrency === ceiling
      ? undefined
      : bound.max_concurrency;
  const next: RunSettings = {
    ...(model.canonical === undefined ? {} : { subagentModel: model.canonical }),
    ...(nextBound === undefined ? {} : { maxConcurrency: nextBound }),
  };
  // (5) 什么都没变就不起新 run：一次修订会停下在飞的 run，没有理由为零改动付这个代价。
  const modelChanged = next.subagentModel !== current.subagentModel;
  const boundChanged = next.maxConcurrency !== current.maxConcurrency;
  if (!modelChanged && !boundChanged) return { ok: false, reason: "unchanged" };

  // (6) 分叉：只有并发变了、run 又在飞，就地
  // 生效——同一个 runId，不停、不铸后继、不导入缓存。端口答 not_live（已结算，或 pending 但引擎
  // 还没建）就顺着这张表往下走，落成今天那次修订。
  if (boundChanged && !modelChanged && typeof port.retuneConcurrency === "function") {
    // 弹层的 `null` 原样递到端口：天花板那个数只有端口知道，这里不猜第二遍。缺席只可能来自
    // 「沿用的界高过本机天花板」（run 是在更大的机器上起的），那时要的也正是回到天花板。
    const answer = await port.retuneConcurrency({
      runId: input.runId,
      maxConcurrency: input.maxConcurrency ?? null,
    });
    if (answer.ok) {
      return retunedSettings.call(this, {
        answer,
        name: displayNameOfSnapshot(snapshot),
        runId: input.runId,
        traceContext,
      });
    }
    if (answer.reason === "unchanged") return { ok: false, reason: "unchanged" };
  }

  // (7) 沿用的脚本（「Keeping the predecessor's script」同一条读路）。读在分叉**之后**：就地调
  // 并发跑的还是同一段脚本，一个没有存档脚本、或脚本已经编不过的 run 因此照样调得动上界。
  const script =
    typeof port.getScript === "function" ? await port.getScript(input.runId) : undefined;
  if (script === undefined || script.length === 0) {
    return { ok: false, reason: "script_missing" };
  }

  // (8) 编译。存下的脚本可能是在更早的 facade 上写的；编不过就停在这里，旧 run 不动。
  const analysis = analyzeScript(script);
  if (!analysis.ok || analysis.diagnostics.length > 0) {
    return {
      ok: false,
      reason: "compile_failed",
      message: boundedCompileDiagnostics(
        `The stored script of run ${input.runId} has errors:`,
        analysis.diagnostics,
      ),
    };
  }

  // —— 到此为止零副作用。——

  // 新 run 的脚本文件，与工具沿用脚本时同一条规则、
  // 同一段代码：前驱的脚本文件此刻仍是这份字节就继续记它，否则照「不来自文件的脚本」写一份新草稿。
  // 不记的话，模型之后要修订这个 run 就只剩把整份脚本内联再抄一遍这一条路。草稿尽力而为：写不成
  // 即缺席，run 照常起。它是 amend 之前唯一的落盘，留下的至多是一个没人引用的草稿文件。
  const graph = boundGraphOfAnalysis(analysis);
  const keptFile = await resolveKeptScriptFile({
    cwd: this.workingDirectory,
    scriptPath: snapshot.scriptPath,
    script,
  });
  const scriptPath =
    keptFile?.path ??
    (
      await writeWorkflowDraft({
        cwd: this.workingDirectory,
        name: resolveWorkflowDraftName(snapshot.name, graph),
        source: script,
      })
    )?.path;

  // (8) 修订。`settings-` 前缀让日志与卡片分得出它与模型工具调用（`tool_*`）、中枢启动（`launch-`）。
  const toolCallId = `settings-${randomUUID()}`;
  const phaseNames = createWorkflowPhaseNames(graph);
  const phaseAlongside = phaseNames === undefined ? undefined : createWorkflowPhaseAlongside(graph);
  const subagentModel = parseWorkflowSubagentModel(next.subagentModel);
  let amended: Awaited<ReturnType<NonNullable<typeof port.amend>>>;
  try {
    amended = await port.amend({
      scriptText: script,
      cwd: this.workingDirectory,
      predecessorRunId: input.runId,
      parentSessionId: this.sessionId,
      toolCallId,
      ...(phaseNames === undefined ? {} : { phaseNames }),
      ...(phaseAlongside === undefined ? {} : { phaseAlongside }),
      ...(next.maxConcurrency === undefined ? {} : { maxConcurrency: next.maxConcurrency }),
      ...(subagentModel === undefined ? {} : { subagentModel }),
      ...(scriptPath === undefined ? {} : { scriptPath }),
      // 重跑的是前驱自己的脚本，它读的正是前驱启动时的实参。
      inheritArgs: true,
      trace: traceContext,
    });
  } catch (error) {
    return {
      ok: false,
      reason: "start_failed",
      message: error instanceof Error ? error.message : String(error),
    };
  }
  if (!amended.ok) {
    return {
      ok: false,
      reason: amended.reason === "run_not_found" ? "not_found" : "missing_boundaries",
    };
  }

  // 名字只取 run 自己的：没起过名的 run 就不带名字，卡片、侧板与通知照任何无名 run 的规矩换用兜底词。
  // 这里曾兜底成 `run <id>`，设置轮的卡片与侧板标题于是成了一串 run id，
  // 而且这个假名会沿修订链一路传下去。
  const name = displayNameOfSnapshot(snapshot);
  const amend: WorkflowSettingsAmendMeta = {
    predecessorRunId: input.runId,
    ...(modelChanged ? { subagentModel: fromTo(current.subagentModel, next.subagentModel) } : {}),
    ...(boundChanged
      ? { maxConcurrency: fromTo(current.maxConcurrency, next.maxConcurrency) }
      : {}),
    ...(ceiling === undefined ? {} : { ceiling }),
  };

  // (9)(10) 之后的失败只记日志不回滚：新 run 已在飞，可在侧板停下；撤回它反而制造孤儿。
  try {
    // 后台追踪：合成一个 AmendWorkflow 描述子走 executor 的同一条登记（registry、终态 waiter、
    // 结算通知）。`input.name` 喂通知主题，工具名让分派归 "workflow"。
    const toolCall: ExecutableToolCall = {
      id: toolCallId,
      name: AMEND_WORKFLOW_TOOL_NAME,
      input: { run_id: input.runId, ...(name === undefined ? {} : { name }) },
    };
    await this.executor.trackExternalBackgroundTask(
      toolCall,
      { backgroundTaskId: amended.runId, status: "backgrounded" },
      traceContext,
      undefined,
    );
    enqueueSettingsTurn.call(this, {
      text: buildSettingsMessageText({
        ...(name === undefined ? {} : { name }),
        previous: input.runId,
        runId: amended.runId,
        superseded: amended.supersededRunId !== undefined,
        amend,
      }),
      meta: boundWorkflowLaunchMeta({
        runId: amended.runId,
        toolCallId,
        ...(name === undefined ? {} : { name }),
        ...(() => {
          const display = displayOfAnalysis(analysis);
          return display?.kind === "create_workflow" ? { display } : {};
        })(),
        amend,
      }),
      // 会话此时必已落库（它名下有 run），标题种子不会被用上；给一个诚实的值即可。
      titleInput: name ?? input.runId,
      traceContext,
    });
  } catch (error) {
    this.logger?.error(
      "Workflow settings amended but post-amend bookkeeping failed",
      error instanceof Error ? error : new Error(String(error)),
      {
        ...traceContextToLogContext(traceContext),
        event: "dynamic_workflow.settings.post_amend_failed",
        module: "core.runtime",
        runId: amended.runId,
        toolCallId,
      },
    );
  }

  return {
    ok: true,
    runId: amended.runId,
    toolCallId,
    ...(amended.supersededRunId === undefined ? {} : { supersededRunId: amended.supersededRunId }),
  };
}

/**
 * 就地生效的收尾：同一个 runId、没有
 * `supersededRunId`，**不登记第二个后台任务**——这个 run 本来就在追踪器里，再登记一次会按
 * AmendWorkflow 的 rearm 规则把一个从没停过的 run 的结算面清空。
 *
 * 设置轮照记，但 `amend` 块不带 `predecessorRunId`：缺席即「就地生效」，渲染端据此只画一行、
 * 不再画一张 run 卡（同一个 run 两张卡会读成两次运行）。也没有 `display`——这条路不编译。
 */
function retunedSettings(
  this: AgentRuntimeInternal,
  options: {
    answer: Extract<DynamicWorkflowRunRetuneResult, { ok: true }>;
    name: string | undefined;
    runId: string;
    traceContext: TraceContext;
  },
): AmendWorkflowRunSettingsResult {
  const { answer, name, runId, traceContext } = options;
  const toolCallId = `settings-${randomUUID()}`;
  // 等于天花板的那一端就是「默认」，于是整端缺席——与修订那条路同一个读法（弹层把步进器推到顶
  // 发的是 `null`，端口答回来的却永远是绝对值，折算只能在这里做）。
  const amend: WorkflowSettingsAmendMeta = {
    maxConcurrency: fromTo(
      answer.previous === answer.ceiling ? undefined : answer.previous,
      answer.maxConcurrency === answer.ceiling ? undefined : answer.maxConcurrency,
    ),
    ceiling: answer.ceiling,
  };
  try {
    enqueueSettingsTurn.call(this, {
      text: buildSettingsMessageText({
        ...(name === undefined ? {} : { name }),
        previous: runId,
        runId,
        superseded: false,
        amend,
      }),
      meta: boundWorkflowLaunchMeta({
        runId,
        toolCallId,
        ...(name === undefined ? {} : { name }),
        amend,
      }),
      titleInput: name ?? runId,
      traceContext,
    });
  } catch (error) {
    // 记完日志就走：上界**已经**生效了，回滚一条记录换不回它，撤销这次调整更不是用户要的。
    this.logger?.error(
      "Workflow run retuned but the settings turn could not be queued",
      error instanceof Error ? error : new Error(String(error)),
      {
        ...traceContextToLogContext(traceContext),
        event: "dynamic_workflow.settings.post_retune_failed",
        module: "core.runtime",
        runId,
        toolCallId,
      },
    );
  }
  return { ok: true, runId, toolCallId };
}

/** run 自己的名字（无名即缺席）：设置轮与合成追踪都按这一条，绝不拿 run id 当标题。 */
function displayNameOfSnapshot(snapshot: { name?: string }): string | undefined {
  const trimmed = snapshot.name?.trim();
  return trimmed === undefined || trimmed.length === 0 ? undefined : trimmed;
}
