// 原生 prompt turn 运行器。
//
// Core admission 只负责接受输入并建立 session-scoped reservation；本文件不再拥有
// activeAbortController，也不等待 projection commit。这样 TurnStarted 之后的任意 Core
// starting/active 状态都会继续挡住同一 session 的第二次 start。
import { type TurnBackgroundAttribution, type TurnInputIntentMetadata } from "@zcode/contracts";
import type { TurnAttachment } from "@zcode/core";
import type { ZCodeAutomationBotDeliveryTarget } from "@zcode/shared";
import type { SendInputOptions, SendInputResult } from "../../app/types.js";
import { runWithSessionResidencyFinalization } from "../../zcode-protocol/session-residency.js";
import type { V4CommandCoreHost, V4SessionRecordView } from "./types.js";

interface StartPromptTurnParamsBase {
  content: string;
  /** v4 锚点：inputId=queryId=commandId（权威数据 sourceCommandId 对账）。 */
  inputId: string;
  inputPresentation?: SendInputOptions["inputPresentation"];
  /** 附件命令面：AttachmentRef 已在 handler 层映射为 core TurnAttachment。 */
  attachments?: TurnAttachment[];
  browserAmbientContext?: SendInputOptions["browserAmbientContext"];
  intent?: TurnInputIntentMetadata;
  /** 标准 Selection 的单次执行约束；不会改写 Session Selection。 */
  modelExecution?: SendInputOptions["modelExecution"];
  sharedContextRefs?: SendInputOptions["sharedContextRefs"];
  toolDisallowlist?: readonly string[];
  /** sendQueuedNow 已持有 Core promotion lease，要求这次 admission 只能占用空闲位。 */
  requireIdle?: boolean;
  /** Bot 入站 turn 的稳定回推地址；仅在本 turn 内暴露给 CronCreate。 */
  botDeliveryTarget?: ZCodeAutomationBotDeliveryTarget;
}

type StartPromptTurnParams = StartPromptTurnParamsBase & TurnBackgroundAttribution;

interface PromptTurnStartResult {
  /** Core admission 已完成；不等待 TurnStarted 或 projection commit。 */
  turnStarted: Promise<void>;
  /** Core 真实完成 promise，仅供生命周期清理使用，不属于 ACK 边界。 */
  completion?: Promise<unknown>;
  admission: SendInputResult;
  /** 兼容旧调用方；初始 ACK 不再依赖 messageId。 */
  messageId?: string;
}

export class V4PromptRejectedError extends Error {
  readonly turnStartUncertain = false;

  constructor(
    readonly reasonCode: "restoreWarning" | "activePrompt",
    message: string,
  ) {
    super(message);
    this.name = "V4PromptRejectedError";
  }
}

/**
 * 入口只做模型/持久化前置校验，然后调用 app -> Core admission。Core 返回 started 时，
 * 后台生命周期等待 completion 清理 turn 归因和广播状态；RPC 本身立即返回 admission receipt。
 */
export async function startPromptTurn(
  host: V4CommandCoreHost,
  record: V4SessionRecordView,
  params: StartPromptTurnParams,
): Promise<PromptTurnStartResult> {
  const usesExecutionSelection = params.modelExecution?.selectionScope === "execution";
  if (!usesExecutionSelection && record.restoreWarning) {
    // app 重启后冷恢复可能跑在 provider registry 推送
    // 之前，record 创建时判「模型不可解析」挂上告警；registry 随后到达时 runtime
    // 早已可服务，但这个一次性标志没人回来清——发送被永久拒（用户只能手动切模型
    // 解锁）。闸前经宿主能力重评：已有可用目标则清除过期告警，随后仍由
    // ensureModelReady 校验当前选择；不能在这里兜底换模型或清空持久选择。
    // 仍无可用模型/宿主不支持 → 维持拒绝。
    if (host.hasUsableRuntimeModelTarget?.(record) === true) {
      host.logger?.info?.("v4 restoreWarning cleared by model catalog recovery", {
        sessionId: record.app.sessionId,
        warningType: record.restoreWarning.type,
      });
      record.restoreWarning = undefined;
    }
  }
  if (!usesExecutionSelection && record.restoreWarning) {
    throw new V4PromptRejectedError("restoreWarning", record.restoreWarning.message);
  }
  if (!usesExecutionSelection) {
    await host.ensureModelReady?.(record);
  }
  if (record.persistence === "deferred") record.persistence = "immediate";

  const previousAutomationId = record.activeAutomationId;
  const previousOffPeakTaskId = record.activeOffPeakTaskId;
  const previousBotDeliveryTarget = record.activeBotDeliveryTarget;
  const activeAutomationId = resolveTurnAutomationId(params);
  const activeOffPeakTaskId = resolveTurnOffPeakTaskId(params);
  const turnToolDisallowlist = buildTurnToolDisallowlist(
    params,
    activeAutomationId,
    activeOffPeakTaskId,
  );
  if (activeAutomationId) record.activeAutomationId = activeAutomationId;
  if (activeOffPeakTaskId) {
    // 闲时派发轮同型标记，供 offpeak-port 在工具执行前拒绝递归 OffPeakCreate。
    record.activeOffPeakTaskId = activeOffPeakTaskId;
  }
  record.activeBotDeliveryTarget = params.botDeliveryTarget;

  let admission: SendInputResult;
  try {
    admission = await record.app.sendInput(
      {
        text: params.content,
        ...(params.attachments ? { attachments: params.attachments } : {}),
      },
      {
        // Bootstrap controller 曾经被当成 Core busy 真相，并由 projection watchdog
        // 清理它；现在 Core admission 自己持有 reservation，Stop 也直接调用 Core execution。
        delivery: "start_turn",
        ...(params.intent?.requestedDelivery === "guide"
          ? { queueDelivery: "guide" as const }
          : {}),
        ...(params.browserAmbientContext
          ? { browserAmbientContext: params.browserAmbientContext }
          : {}),
        inputId: params.inputId,
        ...(params.inputPresentation ? { inputPresentation: params.inputPresentation } : {}),
        ...turnBackgroundAttributionOf({
          automationId: activeAutomationId,
          // 归因用解析后的 id：resume 段仅靠 inputId 前缀兜底时也要进 core loop state。
          offPeakTaskId: activeOffPeakTaskId,
          offPeakRunType: params.offPeakRunType,
        }),
        intent: params.intent,
        ...(params.modelExecution ? { modelExecution: params.modelExecution } : {}),
        ...(params.sharedContextRefs ? { sharedContextRefs: params.sharedContextRefs } : {}),
        ...(turnToolDisallowlist ? { toolDisallowlist: turnToolDisallowlist } : {}),
        ...(params.requireIdle ? { requireIdle: true } : {}),
        queryId: params.inputId as SendInputOptions["queryId"],
      },
    );
  } catch (error) {
    clearPromptRecordState(
      record,
      previousAutomationId,
      previousOffPeakTaskId,
      previousBotDeliveryTarget,
    );
    await host.afterLegacyStateMutation?.(record, "prompt_failed");
    throw error;
  }

  if (admission.kind === "rejected") {
    clearPromptRecordState(
      record,
      previousAutomationId,
      previousOffPeakTaskId,
      previousBotDeliveryTarget,
    );
    throw new V4PromptRejectedError(
      "activePrompt",
      `Core prompt admission rejected: ${admission.reason}`,
    );
  }

  if (admission.kind === "queued") {
    clearPromptRecordState(
      record,
      previousAutomationId,
      previousOffPeakTaskId,
      previousBotDeliveryTarget,
    );
    return { admission, turnStarted: Promise.resolve() };
  }

  const completion = runWithSessionResidencyFinalization(record, async () => {
    let mutationReason = "prompt_completed";
    try {
      await admission.completion;
    } catch (error) {
      mutationReason = "prompt_failed";
      host.logger?.warn?.("v4 background turn failed", {
        error: error instanceof Error ? error.message : String(error),
        inputId: params.inputId,
        sessionId: record.app.sessionId,
      });
    } finally {
      clearPromptRecordState(
        record,
        previousAutomationId,
        previousOffPeakTaskId,
        previousBotDeliveryTarget,
      );
      await host.afterLegacyStateMutation?.(record, mutationReason);
    }
  });
  void completion.catch(() => undefined);
  host.logger?.info?.("v4 prompt admitted", {
    attachmentCount: params.attachments?.length ?? 0,
    inputId: params.inputId,
    sessionId: record.app.sessionId,
    textLength: params.content.length,
  });
  return { admission, completion, turnStarted: Promise.resolve() };
}

function clearPromptRecordState(
  record: V4SessionRecordView,
  previousAutomationId: string | undefined,
  previousOffPeakTaskId: string | undefined,
  previousBotDeliveryTarget: V4SessionRecordView["activeBotDeliveryTarget"],
): void {
  record.activeAutomationId = previousAutomationId;
  // 闲时轮身份与 automation 同规则随 turn 还原，防止跨轮残留误拒 OffPeakCreate。
  record.activeOffPeakTaskId = previousOffPeakTaskId;
  record.activeBotDeliveryTarget = previousBotDeliveryTarget;
}

function buildTurnToolDisallowlist(
  params: Pick<StartPromptTurnParams, "automationId" | "offPeakTaskId" | "toolDisallowlist">,
  activeAutomationId = params.automationId,
  activeOffPeakTaskId = params.offPeakTaskId,
): readonly string[] | undefined {
  const tools = new Set(params.toolDisallowlist ?? []);
  if (activeAutomationId) {
    // automation 派发漏传身份时，后续 model step 会重新暴露 Cron 写工具。
    for (const toolName of AUTOMATION_MUTATION_TOOL_NAMES) tools.add(toolName);
  }
  if (activeOffPeakTaskId) {
    // 闲时派发轮隐藏 OffPeakCreate（防递归自我派生）；OffPeakList 只读保留。
    // automation 轮不加此项——cron 轮放行 OffPeakCreate（定时派生闲时任务）。
    for (const toolName of OFF_PEAK_MUTATION_TOOL_NAMES) tools.add(toolName);
  }
  return tools.size > 0 ? [...tools] : undefined;
}

export function resolveTurnAutomationId(
  params: Pick<StartPromptTurnParams, "automationId" | "inputId">,
): string | undefined {
  const explicit = params.automationId?.trim();
  if (explicit) return explicit;
  const inputId = params.inputId.trim();
  if (!inputId.startsWith(AUTOMATION_INPUT_ID_PREFIX)) return undefined;
  const separatorIndex = inputId.indexOf(":");
  const automationId = separatorIndex >= 0 ? inputId.slice(0, separatorIndex) : inputId;
  return automationId.length > AUTOMATION_INPUT_ID_PREFIX.length ? automationId : undefined;
}

function resolveTurnOffPeakTaskId(
  params: Pick<StartPromptTurnParams, "offPeakTaskId" | "inputId">,
): string | undefined {
  const explicit = params.offPeakTaskId?.trim();
  if (explicit) return explicit;
  // 兜底：续跑派发的 inputId 形如 `offpeak-<uuid>:resume:<uuid>`；首段派发无固定前缀，
  // 主信号必须是显式 offPeakTaskId（host 派发一律显式传）。
  const inputId = params.inputId.trim();
  if (!inputId.startsWith(OFF_PEAK_INPUT_ID_PREFIX)) return undefined;
  const separatorIndex = inputId.indexOf(":");
  const offPeakTaskId = separatorIndex >= 0 ? inputId.slice(0, separatorIndex) : inputId;
  return offPeakTaskId.length > OFF_PEAK_INPUT_ID_PREFIX.length ? offPeakTaskId : undefined;
}

export function turnBackgroundAttributionOf(params: {
  automationId?: string;
  offPeakTaskId?: string;
  offPeakRunType?: "init" | "resume";
}): TurnBackgroundAttribution {
  if (params.automationId) return { automationId: params.automationId };
  if (params.offPeakTaskId) {
    return {
      offPeakTaskId: params.offPeakTaskId,
      ...(params.offPeakRunType ? { offPeakRunType: params.offPeakRunType } : {}),
    };
  }
  return {};
}

const AUTOMATION_INPUT_ID_PREFIX = "automation-";
const AUTOMATION_MUTATION_TOOL_NAMES = ["CronCreate", "CronUpdate", "CronDelete"] as const;
// 独立常量，绝不并入 AUTOMATION_MUTATION_TOOL_NAMES（cron 轮放行 OffPeakCreate）。
// 与 core turn-loop-state 同值——闲时轮同时隐藏 SendMessage / Workflow（两者会在本轮
// modelExecution 之外重启子 Agent）。
const OFF_PEAK_INPUT_ID_PREFIX = "offpeak-";
const OFF_PEAK_MUTATION_TOOL_NAMES = ["OffPeakCreate", "SendMessage", "Workflow"] as const;
