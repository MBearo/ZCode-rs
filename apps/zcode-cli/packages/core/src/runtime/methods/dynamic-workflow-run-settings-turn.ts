// GUI 配置变更的会话记录。
// 将设置轮排入运行时队列，记录两项设置的 from/to，并区分修订产生新 run 与并发调整就地生效。
// 变更决策和副作用顺序由 dynamic-workflow-run-settings.ts 负责。

import type { TraceContext, WorkflowSettingsAmendMeta } from "@zcode/contracts";
import type { DynamicWorkflowRunSnapshot } from "@zcode/contracts";
import { uuidv7 } from "@zcode/shared";
import { createRuntimeCommandId } from "../command-queue.js";
import type { AgentRuntimeInternal } from "../internal.js";
import type { boundWorkflowLaunchMeta } from "@zcode/contracts";

/** 本 run 的两项设置的归一形：缺席即默认（会话模型 / 本机上限）。 */
export interface RunSettings {
  subagentModel?: string;
  maxConcurrency?: number;
}

/**
 * 设置轮不在命令处理里直接落：主代理可能正在一轮里，user 消息插不进去。排进运行时队列，空闲时
 * 立即跑、忙时等当前轮结束；与通知同优先级，因而先于新 run 的任何通知。
 */
export function enqueueSettingsTurn(
  this: AgentRuntimeInternal,
  turn: {
    text: string;
    meta: ReturnType<typeof boundWorkflowLaunchMeta>;
    titleInput: string;
    traceContext: TraceContext;
  },
): void {
  this.enqueueRuntimeCommand({
    branchGeneration: this.branchGeneration,
    createdAt: new Date(),
    id: createRuntimeCommandId(),
    inputId: uuidv7(),
    mode: "control-only-turn",
    priority: "next",
    text: turn.text,
    titleInput: turn.titleInput,
    traceContext: turn.traceContext,
    workflowLaunch: turn.meta,
  });
}

export function fromTo<T>(from: T | undefined, to: T | undefined): { from?: T; to?: T } {
  return { ...(from === undefined ? {} : { from }), ...(to === undefined ? {} : { to }) };
}

/**
 * 设置轮的模型面规范句（英文，不本地化——它进 provider transcript）。只说改过的设置；两个 `null`
 * 各有一句话。末句劝阻模型再动这个 run：它已经在跑，进展以通知回来。
 *
 * 两种结局在这里分岔，判据就是 `amend.predecessorRunId` 在不在场（缺席即就地生效，与元数据同
 * 一条读法）：修订出了一个新 run，就地调并发则还是同一个 run——照修订那句话写下去，模型会去找
 * 一个根本不存在的 run B。
 */
export function buildSettingsMessageText(input: {
  name?: string;
  previous: string;
  runId: string;
  superseded: boolean;
  amend: WorkflowSettingsAmendMeta;
}): string {
  const inPlace = input.amend.predecessorRunId === undefined;
  const lead = `Changed the settings of workflow run ${input.previous}${input.name === undefined ? "" : ` ("${input.name}")`} from the GUI: ${settingsChangeClauses(input.amend, inPlace).join("; ")}.`;
  const closing =
    "Progress and results arrive as background notifications; do not amend, resume or restart it.";
  if (inPlace) {
    const kept =
      input.amend.maxConcurrency?.to === undefined
        ? `Run ${input.previous} keeps running with no limit`
        : `Run ${input.previous} keeps running under the new limit`;
    return [lead, `${kept}; nothing was stopped and no new run was started.`, closing].join(" ");
  }
  const relation = input.superseded ? "supersedes" : "takes over from";
  return [
    lead,
    `The same script continues as run ${input.runId}, which ${relation} run ${input.previous} and imports everything run ${input.previous} finished as cache.`,
    closing,
  ].join(" ");
}

/**
 * 「改了什么」的分句。就地生效那一条要把主语说全（「at most n of **its subagents**」）：那条路上
 * 没有模型分句在前，一句「at most n of them」就没有了指代对象。
 */
function settingsChangeClauses(amend: WorkflowSettingsAmendMeta, inPlace: boolean): string[] {
  const changes: string[] = [];
  const model = amend.subagentModel;
  if (model !== undefined) {
    changes.push(
      model.to === undefined
        ? "its subagents are back on the session model"
        : `its subagents now run on ${model.to}`,
    );
  }
  const bound = amend.maxConcurrency;
  if (bound !== undefined) {
    changes.push(
      bound.to === undefined
        ? "the limit on subagents at once is removed"
        : inPlace
          ? `at most ${bound.to} of its subagents run at once`
          : `at most ${bound.to} of them run at once`,
    );
  }
  return changes;
}

/**
 * 快照上的两项设置。两者都「无则缺席」：没指定过模型 = 会话模型，界不低于天花板 = 没有自己的界，
 * 所以缺席就是默认，与 {@link RunSettings} 同一个读法。
 */
export function runSettingsOfSnapshot(snapshot: DynamicWorkflowRunSnapshot): RunSettings {
  return {
    ...(snapshot.subagentModel === undefined ? {} : { subagentModel: snapshot.subagentModel }),
    ...(snapshot.maxConcurrency === undefined ? {} : { maxConcurrency: snapshot.maxConcurrency }),
  };
}
