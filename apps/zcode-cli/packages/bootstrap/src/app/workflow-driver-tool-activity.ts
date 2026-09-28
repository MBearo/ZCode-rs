// ============================================================
// driver 侧的工具活动观察：子代理即将改写工作区 → 引擎关导入缓存
// ============================================================
// 与 workflow-driver-concurrency.ts 的模型活动面同一副姿态：只读 actor runtime 的会话事件流，不碰 turn
// 编排。这里只认一种事件——`ToolCallStarted`，它由执行器在权限判定之后、handler 动手之前发出，载荷上
// 带着按入参解析后的 `readOnly` / `sideEffectScope`（Bash 的只读命令判定已落定）。判定用 contracts 的
// `isWorkspaceMutatingToolCall`；同一副载荷还按 `isWorldTouchingToolCall` 数出「碰过外部世界」的调用数，
// 那个数决定一条缓存条目是不是纯的（见下）。
//
// 同一份订阅还负责**数**这个 ask 的工具调用（总数与其中的写入数），因为 AskStats 需要它，而
// `TurnResult.events` 里没有工具事件——照那里数，`toolCalls` 恒为 0（实测：生产 journal 的 2097 条
// ask 行无一例外，其中包括明明写过文件的子代理）。计数在 startAsk 归零、跨 repair / nudge 轮累加，
// 所以最后一次 askStats 上报的是这个 ask 的全量。
//
// 两个计数分工不同：`toolCalls` 是全部调用（审计面诚实），`worldToolCalls` 只数看或动了外部世界的那些
// ——协议工具（`submit_result`、`escalate`）不算，否则每条 typed ask 都会因为交结果而显得「碰过世界」，
// 纯 ask 的豁免就一条也用不上了。
//
// 每个 ask 只上报一次关门：引擎侧关门本就幂等，少报几次只是省事件流量；换 ask 时 reset。
//
// 同一份订阅还记下**最近一次**工具调用的名字与一条有界的目标线索（`node-progress` 的 `lastTool`）。名字在 `ToolCallStarted` 上就有，入参没有——
// 它只在更早的 `ToolCallScheduled` 上，所以这里同时订阅两者：scheduled 把入参按 toolCallId 存一手，
// started 取回来、压成线索、随即删掉那一手。只在 started 上落 `lastTool`，与两个计数同一时刻，
// 于是被权限拒掉、从未真正开跑的调用不会冒充「它正在做的事」。

import {
  isWorkspaceMutatingToolCall,
  isWorldTouchingToolCall,
  SessionEventType,
  type SessionEvent,
  type SessionId,
  type ToolCallScheduledPayload,
  type ToolCallStartedPayload,
} from "@zcode/contracts";
import type { AgentRuntime } from "@zcode/core";
import type { AskLastTool } from "@zcode/dynamic-workflow";
import { summarizeToolCall } from "./workflow-driver-tool-target.js";

/**
 * 待认领的 scheduled 入参最多存这么多手。正常情况下每一手都会被紧随的 started 取走，但被权限
 * 拒绝、被取消、或整批被跳过的调用不会——没有上限，一个长 ask 就能把它们攒成一条内存泄漏。
 * 溢出时丢最老的一手（Map 按插入序遍历）：新的调用才是「它正在做什么」的答案。
 */
const MAX_PENDING_SCHEDULED_CALLS = 64;

/** 一个 ask 内观察到的工具调用计数（`AskStats` 的两个字段就是它）。 */
export interface ActorToolCounts {
  /** 全部调用，含 `submit_result` / `escalate` 这类协议工具。 */
  toolCalls: number;
  /** 其中看或动了外部世界的那些（读文件、跑命令、访问网络）；为 0 即这条 ask 是纯的。 */
  worldToolCalls: number;
}

interface ActorToolActivity {
  /** 换 ask 时归零：计数重新数，下一个 ask 的第一笔写入要重新上报一次。 */
  reset(): void;
  /** 本 ask 至今观察到的工具调用计数。 */
  counts(): ActorToolCounts;
  /**
   * 此刻**还在跑**的工具调用数（started 减去 result / error，按 toolCallId 去重，所以一轮里并行
   * 发出的四次 WebSearch 数出来就是 4）。
   *
   * 唯一的读者是座位闸门（workflow-seat-gate.ts）：准入调用上只有 `{model}`，分不出一次模型请求
   * 是这个子代理的下一个 turn step 还是它某个工具内部发的，而**工具侧的请求永不停驻**。有工具在
   * 跑就是工具侧，一个都没有就是下一个 turn step。用 started/结束配对而不是「上一次事件是什么」：
   * 并行调用下后者会在第一个工具返回时就误判成空闲。
   */
  inFlight(): number;
  /** 本 ask 至今**最近**一次真正开跑的工具调用；一次都没有时缺席。 */
  lastTool(): AskLastTool | undefined;
  /** 订阅 runtime 的 `ToolCallStarted` 会话事件；最小 stub runtime 没有 subscribeEvents 时空操作。 */
  observe(runtime: AgentRuntime, sessionId: SessionId): void;
  unsubscribe(): void;
}

export function createActorToolActivity(handlers: {
  /** 当前 ask 的子代理即将执行一个会改写工作区的工具（每个 ask 至多一次）。 */
  onMutating(): void;
}): ActorToolActivity {
  let reported = false;
  let toolCalls = 0;
  let worldToolCalls = 0;
  let lastTool: AskLastTool | undefined;
  /** toolCallId → scheduled 时的名字与入参，等 started 来认领。 */
  const scheduled = new Map<string, ToolCallSummaryHold>();
  /** 已 started 还没等到 result / error 的那些 toolCallId（见 {@link ActorToolActivity.inFlight}）。 */
  const running = new Set<string>();
  let unsubscribeEvents: (() => void) | undefined;
  return {
    reset: () => {
      reported = false;
      toolCalls = 0;
      worldToolCalls = 0;
      lastTool = undefined;
      scheduled.clear();
      // ask 边界上不该还有工具在跑（上一个 ask 的 turn 已经落地或被 abort），归零只是不让
      // 一次异常路径把残留带进下一个 ask——那会让闸门永远把它当成工具侧请求放行。
      running.clear();
    },
    counts: () => ({ toolCalls, worldToolCalls }),
    inFlight: () => running.size,
    lastTool: () => lastTool,
    observe: (runtime, sessionId) => {
      if (typeof (runtime as Partial<AgentRuntime>).subscribeEvents !== "function") return;
      unsubscribeEvents = runtime.subscribeEvents({
        onSessionEvent: (event: SessionEvent) => {
          if (event.sessionId !== sessionId) return;
          if (event.type === SessionEventType.ToolCallScheduled) {
            holdScheduled(scheduled, event.payload as ToolCallScheduledPayload);
            return;
          }
          // 一次调用的结束：执行器在 started 之后的 try/catch 两支上各发一条，所以 started 必有配对
          // （权限拒绝、schema 失败、registry miss 都发生在 started **之前**，只发 error，不影响配对）。
          if (
            event.type === SessionEventType.ToolCallResult ||
            event.type === SessionEventType.ToolCallError
          ) {
            running.delete(String((event.payload as { toolCallId: unknown }).toolCallId));
            return;
          }
          if (event.type !== SessionEventType.ToolCallStarted) return;
          const capability = event.payload as ToolCallStartedPayload;
          running.add(String(capability.toolCallId));
          const hold = scheduled.get(String(capability.toolCallId));
          scheduled.delete(String(capability.toolCallId));
          lastTool =
            summarizeToolCall({
              ...(hold === undefined ? {} : { input: hold.input }),
              toolName: capability.toolName ?? hold?.toolName,
            }) ?? lastTool;
          toolCalls++;
          if (isWorldTouchingToolCall(capability)) worldToolCalls++;
          if (!isWorkspaceMutatingToolCall(capability)) return;
          if (reported) return;
          reported = true;
          handlers.onMutating();
        },
      });
    },
    unsubscribe: () => {
      unsubscribeEvents?.();
      unsubscribeEvents = undefined;
    },
  };
}

/** scheduled 事件上暂存下来的一手（名字 + 入参），等对应的 started 取走。 */
interface ToolCallSummaryHold {
  toolName?: string;
  input?: unknown;
}

/** 存一手 scheduled 入参，并把表压在 {@link MAX_PENDING_SCHEDULED_CALLS} 之内（丢最老的）。 */
function holdScheduled(
  scheduled: Map<string, ToolCallSummaryHold>,
  payload: ToolCallScheduledPayload,
): void {
  scheduled.set(String(payload.toolCallId), { toolName: payload.toolName, input: payload.input });
  while (scheduled.size > MAX_PENDING_SCHEDULED_CALLS) {
    const oldest = scheduled.keys().next();
    if (oldest.done === true) return;
    scheduled.delete(oldest.value);
  }
}
