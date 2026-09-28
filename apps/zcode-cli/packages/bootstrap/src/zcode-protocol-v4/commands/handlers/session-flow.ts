// 会话流命令组：sendText / stop（模式样板）。
// 每个命令组一个文件：handler 纯函数 (host, envelope) → CommandResult|undefined，
// 决策逻辑直驱 core，环境能力走 host 钩子（见 ../types.ts 的过渡标注）。
import type {
  CommandEnvelope,
  CommandPayloadMap,
  CommandResult,
  SubmissionMode,
} from "@zcode/shared/zcode-protocol-v4";
import type { ModelSelection } from "@zcode/shared";
import { createModelExecutionContext } from "../../../zcode-protocol/model-execution.js";
import type { SteerTurnOptions } from "../../../app/types.js";
import { parseProviderQualifiedModelSelection } from "../../../app/provider-registry-selection.js";
import type { TurnAttachment } from "@zcode/core";
import { mapAttachmentRefsToTurnAttachments } from "../attachment-refs.js";
import { inputIntentMetadata } from "../input-intent.js";
import { startPromptTurn, turnBackgroundAttributionOf } from "../prompt-turn.js";
import { requireRecord } from "../record-access.js";
import type { V4CommandCoreHost, V4SessionRecordView } from "../types.js";
import { V4CommandNoopError } from "../../v4-gateway.js";

/** 等 idle 轮询参数：25ms 间隔、5s 超时。 */
const IDLE_POLL_INTERVAL_MS = 25;
const IDLE_POLL_TIMEOUT_MS = 5_000;

export class V4InputAdmissionRejectedError extends Error {
  constructor(
    readonly reasonCode: string,
    message: string,
  ) {
    super(message);
    this.name = "V4InputAdmissionRejectedError";
  }
}

/** V4 用户输入统一准入：正文或附件至少存在一个。 */
export function hasPromptInput(text: string, attachments: readonly unknown[] | undefined): boolean {
  return text.trim().length > 0 || Boolean(attachments && attachments.length > 0);
}

/** held（inputRouting.mode=choice）下 sendText/sendGoalCommand 缺 disposition → 拒绝。 */
class V4HeldQueueDispositionRequiredError extends Error {
  readonly reasonCode = "heldQueueDispositionRequired";
  constructor() {
    super("held queue requires heldQueueDisposition (clearQueueAndSend | keepQueueAndSend)");
    this.name = "V4HeldQueueDispositionRequiredError";
  }
}

/** 确认框打开后队列被另一端增删：旧确认不能继续清空/保留并发送。 */
class V4HeldQueueConfirmationStaleError extends Error {
  readonly reasonCode = "guard.heldQueueConfirmationStale";
  constructor() {
    super("paused queue changed after send confirmation opened");
    this.name = "V4HeldQueueConfirmationStaleError";
  }
}

export async function enqueueDeferredInputForBusyWork(
  record: V4SessionRecordView,
  text: string,
  options: {
    commandKind?: SteerTurnOptions["commandKind"];
    inputId: string;
    queryId: SteerTurnOptions["queryId"];
    intent?: SteerTurnOptions["intent"];
    attachments?: TurnAttachment[];
    toolDisallowlist?: SteerTurnOptions["toolDisallowlist"];
  },
): Promise<boolean> {
  if (!record.app.enqueueDeferredInput) return false;
  const result = await record.app.enqueueDeferredInput(text, {
    ...(options.commandKind ? { commandKind: options.commandKind } : {}),
    delivery: "queue",
    inputId: options.inputId,
    ...(options.intent ? { intent: options.intent } : {}),
    ...(options.attachments ? { attachments: options.attachments } : {}),
    ...(options.toolDisallowlist ? { toolDisallowlist: options.toolDisallowlist } : {}),
    queryId: options.queryId,
  });
  if (result.kind === "queued") return true;
  throw new V4InputAdmissionRejectedError(
    result.reason === "input_too_large"
      ? "proto.payloadTooLarge"
      : result.reason === "empty_input"
        ? "proto.invalidPayload"
        : "fault.command.inputRejected",
    `deferred input rejected: ${result.reason}`,
  );
}

/** 等 idle 超时（active turn 的 finally 5s 内未释放锁）→ 放弃重发并报错。 */
export class V4SessionIdleTimeoutError extends Error {
  constructor(sessionId: string) {
    super(`v4 timed out waiting for session idle: ${sessionId}`);
    this.name = "V4SessionIdleTimeoutError";
  }
}

/**
 * completed + queue>0 + autoDrain=false（投影 inputRouting.mode=choice）时，
 * 输入不静默入队：
 * clear → 先清空 queue 再 startNow；keep → 保留 queue 直接 startNow；缺省 → reject。
 */
export async function applyHeldQueueDisposition(
  host: V4CommandCoreHost,
  record: V4SessionRecordView,
  disposition: "clearQueueAndSend" | "keepQueueAndSend" | undefined,
  expectedQueueItemIds?: readonly string[],
): Promise<void> {
  const routing = host.getInputRoutingMode?.(record.app.sessionId) ?? null;
  if (routing !== "choice") return;
  if (!disposition) {
    throw new V4HeldQueueDispositionRequiredError();
  }
  if (expectedQueueItemIds) {
    const expected = new Set(expectedQueueItemIds);
    const sameItems =
      expected.size === expectedQueueItemIds.length &&
      host.getQueueLength?.(record.app.sessionId) === expected.size &&
      expectedQueueItemIds.every(
        (queueItemId) => host.getQueueItem?.(record.app.sessionId, queueItemId) !== null,
      );
    if (!sameItems) {
      throw new V4HeldQueueConfirmationStaleError();
    }
  }
  if (disposition === "clearQueueAndSend") {
    await record.app.clearQueueItems();
  }
}

/**
 * 兼容 admission：新发送端显式提交 Selection/Mode；旧发送端在 CLI 接收边界把
 * 当前 Session 值固定进 canonical intent。固定完成后 Queue/Guide 不再读取可变 Session。
 */
export function resolveSubmittedExecutionState(
  record: V4SessionRecordView,
  payload: {
    modelSelection?: ModelSelection;
    mode?: SubmissionMode;
    planEnabled?: boolean;
  },
): { modelSelection: ModelSelection; mode: SubmissionMode; planEnabled: boolean } {
  let modelSelection = payload.modelSelection;
  if (!modelSelection) {
    const runtimeSelection = record.app.runtime?.getSessionModelSelection?.();
    const entrySelection = runtimeSelection
      ? undefined
      : parseProviderQualifiedModelSelection(record.app.getModel());
    if (!runtimeSelection && !entrySelection) {
      throw new Error(`Session model must be provider-qualified: ${record.app.getModel()}`);
    }
    // getThoughtLevel() 是 Active Model 的 effective 展示事实。把它补回
    // canonical intent 会把 Config 默认值伪装成显式 pin；旧发送端只能固定 Session
    // 已经持有的稀疏 Selection，不能在 admission 时重新解释它。
    modelSelection = runtimeSelection
      ? {
          providerId: runtimeSelection.providerId,
          modelId: runtimeSelection.modelId,
          ...(runtimeSelection.options ? { options: { ...runtimeSelection.options } } : {}),
        }
      : {
          providerId: entrySelection!.providerId,
          modelId: entrySelection!.modelId,
          ...(entrySelection!.options ? { options: { ...entrySelection!.options } } : {}),
        };
  }
  const current = resolveExecutionState({
    mode: record.app.getMode?.(),
    planEnabled: record.app.runtime?.getPlanEnabled?.(),
  });
  const state = resolveExecutionState(payload, current);
  return {
    modelSelection,
    mode: state.mode === "auto" ? "build" : state.mode,
    planEnabled: state.planEnabled,
  };
}
/**
 * sendText：只做协议/held/model/附件校验，start/queue 交给同一 session 的 Core admission。
 * held（choice）时仍按 heldQueueDisposition 裁决。
 */
async function sendText(
  host: V4CommandCoreHost,
  envelope: CommandEnvelope,
): Promise<CommandResult | undefined> {
  const payload = envelope.payload as CommandPayloadMap["sendText"];
  const record = requireRecord(host, envelope.sessionId);
  // 旧校验只看正文，UI 已允许的 attachment-only query 会在 CLI 被误判为空。
  if (!hasPromptInput(payload.text, payload.attachments)) {
    throw new V4InputAdmissionRejectedError("proto.invalidPayload", "input must not be empty");
  }
  const attachments = await mapAttachmentRefsToTurnAttachments(record.app, payload.attachments);
  const submittedExecutionState = resolveSubmittedExecutionState(record, payload);
  const submissionIntent = (options: Parameters<typeof inputIntentMetadata>[1]) =>
    inputIntentMetadata(envelope, { ...options, ...submittedExecutionState });
  const routingMode = host.getInputRoutingMode?.(envelope.sessionId ?? "") ?? null;
  const forceStartNow = payload.requestedDelivery === "startNow";
  const foregroundPromotionLeaseId = forceStartNow ? `send-now:${envelope.commandId}` : undefined;
  let foregroundPromotionLeaseAcquired = false;
  let preempted = false;
  const releaseForegroundPromotionLease = () => {
    if (!foregroundPromotionLeaseAcquired || !foregroundPromotionLeaseId) return;
    record.app.runtime.releaseForegroundPromotionLease(foregroundPromotionLeaseId);
    foregroundPromotionLeaseAcquired = false;
  };
  if (forceStartNow) {
    // 修饰键的“立即发送”若先进入 Core busy admission 会短暂创建 queue item。
    // 先取得唯一前台租约并抢占当前轮，再交给 Core 以 idle start_turn 原子启动。
    const leaseResult = record.app.runtime.acquireForegroundPromotionLease({
      leaseId: foregroundPromotionLeaseId!,
      mode: "after-current",
      promotedInputId: envelope.commandId,
    });
    if (leaseResult.kind !== "acquired") {
      throw new V4InputAdmissionRejectedError(
        "fault.command.inputRejected",
        "send now foreground promotion is busy",
      );
    }
    foregroundPromotionLeaseAcquired = true;
    try {
      // startNow 旧分支把 held queue 裁决误当成默认路由的一部分整体跳过，
      // 导致用户确认“清空队列并发送”后旧输入仍可能被 drain。单条消息的
      // delivery 只决定新输入何时消费，不能绕过已有队列的用户裁决和过期校验。
      await applyHeldQueueDisposition(
        host,
        record,
        payload.heldQueueDisposition,
        payload.expectedHeldQueueItemIds,
      );
      preempted = await preemptActiveTurnAndWait(host, record, {
        abortMessage: "v4 sendText startNow preempts active turn",
        goalPausedMutationReason: "send_now_goal_paused",
        preserveQueueAutoDrainOnCancel: true,
      });
    } catch (error) {
      releaseForegroundPromotionLease();
      throw error;
    }
  }
  if (!forceStartNow) {
    await applyHeldQueueDisposition(
      host,
      record,
      payload.heldQueueDisposition,
      payload.expectedHeldQueueItemIds,
    );
  }
  let started;
  // 附件命令面：AttachmentRef → TurnAttachment 在闸门后映射（active turn 已排除）。
  try {
    const intent = submissionIntent({
      text: payload.text,
      requestedDelivery:
        payload.requestedDelivery ??
        (routingMode === "guide" ? "guide" : routingMode === "enqueue" ? "queue" : "startNow"),
      ...(routingMode === "guide" && attachments?.length
        ? { fallbackReasonCode: "guide.attachmentsUnsupported" }
        : {}),
      attachmentRefs: payload.attachments,
      sharedContextRefs: payload.context_refs,
    });
    started = await startPromptTurn(host, record, {
      content: payload.text,
      ...(payload.browserAmbientContext
        ? { browserAmbientContext: payload.browserAmbientContext }
        : {}),
      inputId: envelope.commandId,
      // 立即发送切换了 runtime turn，导致运行中用户输入遗漏 human 提示。
      // 只按 Core 的实际抢占回执标记纯文本；空闲及附件输入保留原路径。
      ...(preempted && !attachments?.length ? { inputPresentation: "user_steer" as const } : {}),
      intent,
      ...(payload.context_refs ? { sharedContextRefs: payload.context_refs } : {}),
      ...turnBackgroundAttributionOf(payload),
      ...(payload.botDeliveryTarget ? { botDeliveryTarget: payload.botDeliveryTarget } : {}),
      toolDisallowlist: payload.toolDisallowlist,
      ...(payload.modelExecution
        ? { modelExecution: createModelExecutionContext(payload.modelExecution) }
        : {}),
      ...(attachments ? { attachments } : {}),
      // promotion lease 本身属于 Core busy authority；若不声明 requireIdle，
      // 抢占完成后的 startNow 会先落 deferred queue，待 lease 释放后再被自动 drain。
      ...(forceStartNow ? { requireIdle: true } : {}),
    });
  } finally {
    releaseForegroundPromotionLease();
  }
  if (started.admission.kind === "queued") {
    return {
      type: "inputAccepted",
      delivery: "queue",
      inputId: envelope.commandId,
    };
  }
  return {
    type: "inputAccepted",
    delivery: "startNow",
    inputId: envelope.commandId,
  };
}

/** stop：精确取消投影中看到的 runtime 前台执行，并把 active goal 收口为 paused。 */
async function stop(
  host: V4CommandCoreHost,
  envelope: CommandEnvelope,
): Promise<CommandResult | undefined> {
  const record = requireRecord(host, envelope.sessionId);
  const payload = envelope.payload as CommandPayloadMap["stop"];
  const runtimeStop = record.app.runtime?.stopActiveForegroundExecution?.({
    expectedForegroundExecutionId: payload.expectedForegroundExecutionId,
    reason: "v4 session stopped",
  });
  host.logger?.info?.("v4 stop foreground execution inspected", {
    activeForegroundExecutionId:
      runtimeStop?.kind === "mismatch"
        ? runtimeStop.activeForegroundExecutionId
        : runtimeStop?.kind === "stopped"
          ? runtimeStop.foregroundExecutionId
          : undefined,
    event: "v4.stop.foreground_execution_inspected",
    expectedForegroundExecutionId: payload.expectedForegroundExecutionId,
    module: "bootstrap.zcode_protocol_v4.commands",
    runtimeStopKind: runtimeStop?.kind ?? "unsupported",
    sessionId: record.app.sessionId,
  });
  if (
    payload.expectedForegroundExecutionId !== undefined &&
    (runtimeStop?.kind === "idle" || runtimeStop?.kind === "mismatch")
  ) {
    // Stop 从 renderer 到 host 有异步窗口；若 verifier 已结束且下一轮已启动，
    // 继续 abort 外层 controller 会误杀用户没看到的新执行。execution id 不匹配只能 noop。
    throw new V4CommandNoopError("guard.stopTargetChanged");
  }
  if (runtimeStop?.kind === "stopped") {
    // 先打断 runtime-owned verifier/continuation，再等待 goal pause；否则 verifier 可能在
    // pause RPC 完成前通过并接入下一次 continuation。
    record.activeAbortController?.abort(new Error("v4 session stopped"));
    if ((host.getQueueLength?.(record.app.sessionId) ?? 0) > 0) {
      try {
        // verifier 已越过普通 turn catch，不能依赖 TurnComplete(cancelled)
        // 翻转 runtime queue gate；显式写入 false，确保 future queue 原位 held。
        await record.app.setQueueAutoDrain(false);
      } catch (error) {
        host.logger?.warn?.("v4 stop failed to hold following queue", {
          error: error instanceof Error ? error.message : String(error),
          sessionId: record.app.sessionId,
        });
      }
    }
    const pausedGoal = await pauseActiveGoal(host, record);
    if (pausedGoal) {
      await host.afterLegacyStateMutation?.(record, "session_stop_goal_paused");
    }
    return undefined;
  }

  // 兼容 compact 与旧客户端：它们没有 runtime foreground execution token，仍由
  // bootstrap 外层 controller 提供取消窗口。
  const hadActivePrompt = Boolean(record.activeAbortController);
  let pausedGoal = false;
  if (hadActivePrompt) {
    pausedGoal = await pauseActiveGoal(host, record);
  }
  record.activeAbortController?.abort(new Error("v4 session stopped"));
  if (pausedGoal) {
    await host.afterLegacyStateMutation?.(record, "session_stop_goal_paused");
  }
  return undefined;
}

/** goal-pause barrier 共用件：stop 与 sendQueuedNow（抢占重发）复用，不复制。 */
async function pauseActiveGoal(
  host: V4CommandCoreHost,
  record: V4SessionRecordView,
): Promise<boolean> {
  // 注意：方法必须经 app 调用（不可解构，实现可能依赖 this 绑定）。
  const target = await record.app.readTarget();
  if (!target || target.status !== "active") {
    return false;
  }
  try {
    const paused = await record.app.updateTargetStatus("paused");
    return Boolean(paused);
  } catch (error) {
    host.logger?.warn?.("v4 stop failed to pause active goal", {
      error: error instanceof Error ? error.message : String(error),
      sessionId: record.app.sessionId,
    });
    return false;
  }
}

/** 轮询等 Bootstrap turn 与 Core foreground command 的 finally 都释放 authority。 */
async function waitForSessionIdle(record: V4SessionRecordView): Promise<void> {
  const deadline = Date.now() + IDLE_POLL_TIMEOUT_MS;
  while (
    record.activeAbortController !== undefined ||
    record.app.runtime?.getActiveForegroundExecutionId?.() !== undefined
  ) {
    if (Date.now() >= deadline) {
      throw new V4SessionIdleTimeoutError(record.app.sessionId);
    }
    await new Promise((resolve) => setTimeout(resolve, IDLE_POLL_INTERVAL_MS));
  }
}

/** 等待旧执行释放，返回 Core 是否实际取消了前台执行。 */
export async function preemptActiveTurnAndWait(
  host: V4CommandCoreHost,
  record: V4SessionRecordView,
  options: {
    abortMessage: string;
    goalPausedMutationReason: string;
    preserveQueueAutoDrainOnCancel?: boolean;
  },
): Promise<boolean> {
  const bootstrapAbortController = record.activeAbortController;
  // background notification 的 model-only turn 由 Core runtime command
  // 独立持有 foreground authority，不会创建 Bootstrap activeAbortController。
  // 忽略这点会误判 idle，随后把被提升的 queue item steer 回旧 notification turn。
  const runtimeStop = record.app.runtime?.stopActiveForegroundExecution?.({
    preserveQueueAutoDrainOnCancel: options.preserveQueueAutoDrainOnCancel === true,
    reason: options.abortMessage,
  });
  if (bootstrapAbortController || runtimeStop?.kind === "stopped") {
    const pausedGoal = await pauseActiveGoal(host, record);
    if (runtimeStop?.kind !== "stopped") {
      bootstrapAbortController?.abort(new Error(options.abortMessage));
    }
    if (pausedGoal) {
      await host.afterLegacyStateMutation?.(record, options.goalPausedMutationReason);
    }
  }
  await waitForSessionIdle(record);
  return runtimeStop?.kind === "stopped";
}

export const sessionFlowHandlers = { sendText, stop };
import { resolveExecutionState } from "@zcode/shared";
