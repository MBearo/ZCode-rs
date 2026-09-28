// AmendWorkflow：只改并发时就地生效。
// 当调用只带 `run_id` 与 `max_concurrency`，且前驱仍在运行时，保留 run ID、子代理、转录与未完成 ask。
// 本模块负责路由判定、端口调用和结果说明；若 run 在判定与调用之间结算，重新读取前驱事实，
// 再决定是否允许回落到修订流程。

import {
  AmendWorkflowInputSchema,
  isAmendWorkflowOwnedPredecessor,
  type AmendWorkflowInput,
  type AmendWorkflowPredecessor,
  type CreateWorkflowOutput,
  type DynamicWorkflowRunPort,
  type DynamicWorkflowRunRetuneResult,
} from "@zcode/contracts";
import type { ToolExecutionContext, ToolHandlerFailure } from "../types.js";
import {
  AMEND_WORKFLOW_ERROR_CODE,
  describePredecessor,
  resolveAmendMaxConcurrency,
  resolveAmendScript,
} from "./amend-workflow-source.js";
import { clampWorkflowMaxConcurrency } from "./create-workflow-source.js";
import { workflowRunNotFoundFailure } from "./workflow-run-introspection.js";

/**
 * 「除并发之外什么都没变」：判的是**入参的形状**，不是新字段。
 * `script`、`path`、`subagent_model` 与 `name` 无论带的是什么，都把这次调用送去修订那条路——
 * 它们各自都可能改变将要跑的东西，而这条路的前提是「跑的还是同一段脚本、同一批子代理」。
 *
 * 归一化之后的入参同样适用：修订那条路恒会落定一份脚本，所以「没有脚本、却有并发」只可能是
 * 就地调并发。
 */
export function isConcurrencyOnlyAmend(model: AmendWorkflowInput): boolean {
  return (
    model.max_concurrency !== undefined &&
    model.script === undefined &&
    model.path === undefined &&
    model.subagent_model === undefined &&
    model.name === undefined
  );
}

/**
 * resolveInput 里的路由判定。命中即返回**不带脚本**的归一化入参——此后 prepareApproval 放行、handler 调
 * `retuneConcurrency`；不命中回 `undefined`，调用方照常走修订。
 *
 * 三个前提缺一不可：入参形状只改并发、前驱还活着（`pending` 与 `running` 都试，活不活由端口说
 * 了算）、端口接得住这条控制面。端口不带 `retuneConcurrency`（老宿主）时整条路不存在，调用
 * 原样落成今天的修订——包括别人的 run 那一个确认窗。
 */
export function resolveConcurrencyRetuneRoute(options: {
  model: AmendWorkflowInput;
  port: DynamicWorkflowRunPort;
  predecessor: AmendWorkflowPredecessor;
  /** 前驱快照上的上界；缺席即它跑在天花板上。 */
  inherited: number | undefined;
}): { result: true; input: AmendWorkflowInput } | ToolHandlerFailure | undefined {
  const { model, port, predecessor } = options;
  if (!isConcurrencyOnlyAmend(model)) return undefined;
  if (predecessor.status !== "pending" && predecessor.status !== "running") return undefined;
  if (typeof port.retuneConcurrency !== "function") return undefined;

  // 三态在这里**不**归一：`null` 要原样递到端口（见 resolveAmendMaxConcurrency 的注释）。
  const requested = model.max_concurrency ?? null;
  const ceiling = port.concurrencyCeiling?.();
  const unchanged = refuseUnchangedBound(model.run_id, requested, options.inherited, ceiling);
  if (unchanged !== undefined) return unchanged;
  return {
    result: true,
    input: { run_id: model.run_id, max_concurrency: requested, predecessor },
  };
}

/**
 * 同值就在这里收口——早于 hook、早于确认窗，端口一次都不碰。天花板读不到（老宿主不带
 * `concurrencyCeiling`）时 `null` 无从折算成数，这道网就让开，由端口自己去答 `unchanged`。
 */
function refuseUnchangedBound(
  runId: string,
  requested: number | null,
  inherited: number | undefined,
  ceiling: number | undefined,
): ToolHandlerFailure | undefined {
  const current = inherited ?? ceiling;
  const next = requested === null ? ceiling : clampWorkflowMaxConcurrency(requested, ceiling);
  if (current === undefined || next === undefined || current !== next) return undefined;
  return retuneUnchangedFailure(runId, current, ceiling);
}

/**
 * handler 侧的本体：调端口、把三种答复翻成模型面的结果。
 *
 * 回 `undefined` 只有一个含义——端口答了 `not_live`（或宿主根本没有这条控制面），这次调用
 * 此刻描述的是一次修订，做不做由 {@link resolveRetuneFallbackAmend} 定夺。
 */
export async function runConcurrencyRetune(
  parsed: AmendWorkflowInput,
  context: ToolExecutionContext,
): Promise<CreateWorkflowOutput | ToolHandlerFailure | undefined> {
  const port = context.dynamicWorkflowRunPort;
  if (port === undefined || typeof port.retuneConcurrency !== "function") return undefined;
  const answer = await port.retuneConcurrency({
    runId: parsed.run_id,
    // 路由判定已保证这里是「一个数或 null」；`?? null` 只是把绕过归一化的缺席读作「回天花板」。
    maxConcurrency: parsed.max_concurrency ?? null,
  });
  if (answer.ok) {
    return {
      diagnostics: [],
      ok: true,
      // 不进后台追踪器：run 本来就在里面，而且它自始至终是同一个 run，没有 `backgrounded`
      // 契约可言，也没有编译产物可画。`retuned` 是**显式**
      // 的判别块：消费方不该按「ok 且没有 status」去猜，那个形状还有别的来路。
      response: retuneResponse(parsed.run_id, answer),
      retuned: {
        runId: parsed.run_id,
        maxConcurrency: answer.maxConcurrency,
        previous: answer.previous,
        ceiling: answer.ceiling,
      },
    } satisfies CreateWorkflowOutput;
  }
  if (answer.reason === "unchanged") {
    return retuneUnchangedFailure(parsed.run_id, answer.current, port.concurrencyCeiling?.());
  }
  return undefined;
}

/**
 * 结算竞态：同一份入参此刻
 * 描述的是一次修订。能不能做只看一件事——**那次修订本来要不要开窗**。
 *
 * 本会话自己的、不是用户亲手停下的 run：owner 规则本来也不开窗，于是照常修订，脚本与编译推迟
 * 到此刻才发生（缺脚本、编不过都按修订自己的拒绝回报）。别人的 run，或用户停过的 run：拒掉。
 * 这条路一个窗都没弹过，不能把「什么都没批」撑成「另起一次 run」。
 */
export async function resolveRetuneFallbackAmend(
  parsed: AmendWorkflowInput,
  context: ToolExecutionContext,
): Promise<{ result: true; input: AmendWorkflowInput } | ToolHandlerFailure> {
  const port = context.dynamicWorkflowRunPort;
  // 事实要重读一遍：run 刚刚在存活判定与端口调用之间结算，入参里的那一份说的还是「在跑」。
  const snapshot = port === undefined ? undefined : await port.getTask(parsed.run_id);
  if (snapshot === undefined) return predecessorNotFoundFailure(parsed.run_id);
  const predecessor = describePredecessor(snapshot, context.sessionId);
  if (!isAmendWorkflowOwnedPredecessor(predecessor)) {
    // 还没结算（pending、引擎没建起来）与已结算是两句不同的话，但下一步相同：再调一次。
    return predecessor.status === "pending" || predecessor.status === "running"
      ? notRetunableFailure(parsed.run_id)
      : runSettledFailure(parsed.run_id);
  }
  const script = await resolveAmendScript({
    model: parsed,
    cwd: context.workingDirectory,
    port,
    predecessorScriptPath: snapshot.scriptPath,
  });
  if (!script.result) return script;
  return {
    result: true,
    input: AmendWorkflowInputSchema.parse({
      run_id: parsed.run_id,
      ...script.fields,
      // 落回修订就回到「一个数或没有」：这次调用显式给了值，没有可沿用的。
      ...resolveAmendMaxConcurrency(
        parsed.max_concurrency,
        undefined,
        port?.concurrencyCeiling?.(),
      ),
      predecessor: {
        ...predecessor,
        ...(script.inherited ? { script_inherited: true as const } : {}),
      },
    }) as AmendWorkflowInput,
  };
}

/** 前驱不存在：与 resolveInput 那一条同源同文案（一个 run id 打错了只该有一种说法）。 */
function predecessorNotFoundFailure(runId: string): ToolHandlerFailure {
  const base = workflowRunNotFoundFailure(runId);
  return {
    ...base,
    message: `${base.message} Nothing was stopped or created: \`run_id\` pointed at a run that does not exist — pass an existing run's ID (see ListWorkflowRuns), or start a fresh run with CreateWorkflow.`,
  };
}

/** 现在生效的上界读成一句话；等于天花板即「没有自己的界」。 */
function describeBoundInForce(bound: number | undefined, ceiling: number | undefined): string {
  if (bound === undefined || bound === ceiling) {
    return "has no limit on how many subagents run at once (it runs at this machine's maximum)";
  }
  return bound === 1
    ? "already runs at most 1 subagent at once"
    : `already runs at most ${bound} subagents at once`;
}

/** 同值：什么都没写、什么都没停，拒绝里点名此刻生效的那个界。 */
export function retuneUnchangedFailure(
  runId: string,
  current: number | undefined,
  ceiling: number | undefined,
): ToolHandlerFailure {
  return {
    result: false,
    errorCode: AMEND_WORKFLOW_ERROR_CODE.RETUNE_UNCHANGED,
    message: `workflow_retune_unchanged: run ${runId} ${describeBoundInForce(current, ceiling)}, so there is nothing to change. Nothing was stopped, created or changed — pass a different \`max_concurrency\`, or a revised script if you meant to amend the run.`,
  };
}

/** 别人的 run，已经结算：再调一次，那一次从头走修订，连同它要的那个确认窗。 */
export function runSettledFailure(runId: string): ToolHandlerFailure {
  return {
    result: false,
    errorCode: AMEND_WORKFLOW_ERROR_CODE.RUN_SETTLED,
    message: `workflow_run_settled: run ${runId} settled before the new parallelism limit could take hold, so there is nothing running to retune. Nothing was stopped, created or changed — call AmendWorkflow again if you want a new run of it under that limit.`,
  };
}

/** 别人的 run，本 agent 从没握住过（`pending`，引擎还没建）：同样的下一步。 */
export function notRetunableFailure(runId: string): ToolHandlerFailure {
  return {
    result: false,
    errorCode: AMEND_WORKFLOW_ERROR_CODE.NOT_RETUNABLE,
    message: `workflow_run_not_retunable: run ${runId} is not being executed by this agent, so its parallelism cannot be changed in place. Nothing was stopped, created or changed — call AmendWorkflow again if you want a new run of it under that limit.`,
  };
}

/**
 * 模型面的回话：**点名一个 run、没有后继**——模型正是据此
 * 分辨自己这次调用走的是哪条路，不必被告知路由本身。上界等于天花板时说「限制已取消」而不是
 * 报一个数，与 `CreateWorkflow` 划的是同一条界。
 *
 * ⚠ 这段文本是**唯一**过得了 v4 的事实：这条路没有 display 载荷（`retuned` 这个块只到进程内为
 * 止，协议的 `toolOutputSchema` 只带 text / display / truncated），工具卡拿它当整行来画。所以它
 * 必须自足——点名 run、点名现在的上界、说清没有新 run——而且要稳：改词就等于改 UI。
 */
function retuneResponse(
  runId: string,
  answer: Extract<DynamicWorkflowRunRetuneResult, { ok: true }>,
): string {
  const bound =
    answer.maxConcurrency === answer.ceiling
      ? "the limit on how many subagents run at once is removed (this machine's maximum applies)"
      : answer.maxConcurrency === 1
        ? "at most 1 subagent runs at once"
        : `at most ${answer.maxConcurrency} subagents run at once`;
  return `Applied to the running run ${runId}; ${bound}. Run ${runId} keeps running under it: nothing was stopped and no new run was started.`;
}
