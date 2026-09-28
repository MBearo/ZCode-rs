// ============================================================
// driver 侧的自适应并发半身：per-actor 准入 + 模型状态观察 + 失败归因
// ============================================================
// 从 workflow-driver.ts
// 拆出：那个文件只管「会话与 turn 的编排」，而这里三件事都不碰 turn 编排——它们只把治理器端口包成
// 一个 actor 的 `ModelRequestAdmission`、只读 actor runtime 的会话事件流、只看 run 级的成功/重试节拍
// （stall 时钟）。模型失败归因（原 modelFailureReasonOf）
// 已移到 adapters 的策略表（`inspectWorkflowModelFailure`）：runner 与 driver 读同一份。

import {
  SessionEventType,
  type ModelNetworkStatusEvent,
  type ModelRequestAdmission,
  type SessionEvent,
  type SessionId,
} from "@zcode/contracts";
import type { AgentRuntime } from "@zcode/core";
import type {
  AskLastTool,
  AskProgress,
  AskWaitInfo,
  InstanceRef,
  RunStallInfo,
} from "@zcode/dynamic-workflow";
import { createActorToolActivity, type ActorToolCounts } from "./workflow-driver-tool-activity.js";
import {
  workflowConcurrencyKey,
  type WorkflowConcurrencyPort,
} from "./workflow-concurrency-governor.js";
import type { WorkflowRunSeatGate } from "./workflow-seat-gate.js";

/** 与治理器同一条纪律：这两种 retry 不是 provider 失败，不值一个徽标。 */
const NON_FAILURE_RETRY_REASONS: ReadonlySet<string> = new Set([
  "reasoning_signature_repair",
  "auth_refresh",
]);

interface ActorModelActivityHandlers {
  /** 该 ask 的下一个模型请求在等：等槽位（`cause: "slot"`）或退避中（`cause: "backoff"`）。 */
  onWaiting(info: AskWaitInfo): void;
  /** 该 ask 的模型请求真的发出去了（首个请求、或一段等待之后的那次）。 */
  onExecuting(): void;
  /** 本 actor 的任一模型请求（turn 或工具侧）成功完成——run 级 stall 时钟的归零信号。 */
  onRequestCompleted?(): void;
  /** runner 排定了一次重试（签名修复 / 鉴权刷新除外）——run 级 stall 时钟的上膛信号。 */
  onRetryScheduled?(reason: string): void;
  /**
   * 在飞 ask 的子代理即将执行一个会改写工作区的工具（每个 ask 至多一次；实例由本模块按 `live` 解析，
   * 不在飞就不报）。引擎据此关闭 amend-resume 的导入缓存。
   */
  onMutating?(instance: InstanceRef): void;
}

/**
 * 一个 actor 的**观察面**：它 runtime 的 `ModelRequestAdmission`（准入）、它会话事件流里的模型状态
 * （executing / waiting 徽标的来源），以及同一条流里的工具调用（第一笔工作区写入 → 关导入缓存，
 * 加上这个 ask 的工具计数，见 workflow-driver-tool-activity.ts）。三者共用 per-ask 生命周期，所以
 * driver 只持有一个对象、只 observe / reset / unsubscribe 一次；「等槽位」之后的那次
 * `model_request_started` 也因此能被认出是恢复。
 */
export interface ActorModelActivity {
  /** 交给 runtime deps 的准入端口；治理器端口缺席时为 undefined（不受闸门约束）。 */
  readonly admission: ModelRequestAdmission | undefined;
  /** 换 ask 时归零：上一个 ask 的 waiting / executing 相位与工具计数都不能带到下一个 ask。 */
  reset(): void;
  /** 本 ask 至今观察到的工具调用计数（喂给 AskStats）。 */
  toolCounts(): ActorToolCounts;
  /** 本 ask 至今最近一次开跑的工具调用（喂给 `node-progress` 的 lastTool）。 */
  lastTool(): AskLastTool | undefined;
  /**
   * 记下「又一轮 turn 解析完了」，并给出这一刻的 ask 进度（`turn` 从 1 起）。
   * 计数住在这里而不是 SessionState 上，因为它与工具计数是同一条 per-ask 生命周期：
   * 同一个 reset 归零，同一个 ask 内累加，nudge 起的新一轮算第二个 turn。
   */
  noteTurnResolved(): AskProgress;
  /** 订阅 runtime 的会话事件（模型状态 + 工具调用）；最小 stub runtime 没有 subscribeEvents 时空操作。 */
  observe(runtime: AgentRuntime, sessionId: SessionId): void;
  unsubscribe(): void;
}

/**
 * 一条请求链的相位。链 = 「一个逻辑请求及其全部重试」：按 (querySource, queryId, toolCallId) 键入——
 * turn step 有 queryId 无 toolCallId，并行的两次 WebSearch 各有自己的 toolCallId，压缩 / 标题 sidecar
 * 各有 querySource——重试换 requestId 但键不变，所以退避中的链与它的下一次尝试是同一条。
 */
type ChainPhase = "queued" | "executing" | "backoff";

interface Chain {
  phase: ChainPhase;
  /** 最近一条等待信息（queued / backoff）；链结束后若只剩等待者，用最新的那条报 waiting。 */
  wait?: AskWaitInfo;
  waitSeq: number;
}

/**
 * 聚合规则（取代单相位）：driver 为每个 actor 维护在飞请求链的集合——
 *   - `model_request_queued`（`tryAcquire` 未命中）→ 链 queued；
 *   - `model_request_admitted` / `model_request_started` → 链 executing；
 *   - `model_retry_scheduled`（`reasoning_signature_repair` / `auth_refresh` 除外）→ 链 backoff；
 *   - `model_request_completed` / 不可重试的 `model_request_failed` → 链结束（出集合）。
 * 子代理相位：任一链 executing → executing；否则有链在等 → waiting（带最近一条等待信息）；集合为空 →
 * 保持上一相位、不发事件（同一 ask 内工具执行期间，工具自己不排队时子代理仍显示 executing）。
 * 一个子代理可以同时持多张票（并行工具调用），所以不能再用单相位。
 *
 * 这是一条 driver **内部**的观察订阅，不是 transcript 的直播通道：v4 网关只排水连续 seq，所以
 * transcript 直播必须走 child runtime 的构造期 eventSink（run service 测试钉着这条）；这里只读
 * 模型网络状态，漏掉 seq 1 无关紧要。
 */
export function createActorModelActivity(input: {
  port: WorkflowConcurrencyPort | undefined;
  runId: string;
  /** 当前在飞 ask 的实例（结算 / 取消后为 undefined）；只有工具活动的上报需要它。 */
  live?: () => InstanceRef | undefined;
  handlers: ActorModelActivityHandlers;
  /**
   * 本 run 的座位闸门与这个子代理在闸门里的键。在场时准入端口再包一层：先等座位、再过治理器。缺席即这个 run
   * 的上界从不中途变动（snippet 执行、不带闸门的装配），准入端口与从前逐字相同。
   */
  seat?: { gate: WorkflowRunSeatGate; key: string };
}): ActorModelActivity {
  const chains = new Map<string, Chain>();
  let executing = false;
  /** 上一条已报出的等待信息；executing 一报即清。同一段等待里内容相同的等待只报一次。 */
  let reportedWait: AskWaitInfo | undefined;
  /** 本 ask 内已解析的 turn 数（`node-progress` 的 `turn`）；换 ask 归零。 */
  let turnsResolved = 0;
  let waitSeq = 0;
  let unsubscribeEvents: (() => void) | undefined;
  const { handlers, live, port, runId, seat } = input;
  // 同一条会话事件流的第二个读者：工具调用。实现单独成文件（判定与计数都在那里），这里只把它
  // 编进同一个生命周期，好让 driver 侧仍然只有一个观察对象。
  const toolActivity = createActorToolActivity({
    onMutating: () => {
      const instance = live?.();
      if (instance !== undefined) handlers.onMutating?.(instance);
    },
  });

  const anyExecuting = (): boolean => {
    for (const chain of chains.values()) if (chain.phase === "executing") return true;
    return false;
  };
  /**
   * 聚合后仍在等：报一次 waiting（每一段等待恰好一条）。
   * 一个子代理并行发 4 个 WebSearch、四条链在同一毫秒排队，四条 queued 事件各报一次
   * `waiting(slot)`——实测 80 条等待记录有 26 条是这种同秒同文的重复。子代理相位没有变，
   * 事件就不该再发：与上一条已报出的等待信息逐字段相同时吞掉；换 executing 后再等才重新报。
   */
  const reportWaiting = (info: AskWaitInfo): void => {
    if (anyExecuting()) return;
    executing = false;
    if (reportedWait !== undefined && sameWait(reportedWait, info)) return;
    reportedWait = info;
    handlers.onWaiting(info);
  };
  const reportExecuting = (): void => {
    reportedWait = undefined;
    if (executing) return;
    executing = true;
    handlers.onExecuting();
  };
  const setWaiting = (key: string, phase: "queued" | "backoff", wait: AskWaitInfo): void => {
    waitSeq += 1;
    chains.set(key, { phase, wait, waitSeq });
    reportWaiting(wait);
  };
  /** 一条链结束：若没有链在执行、却还有链在等，子代理此刻才真正进入等待——用最新的等待信息报。 */
  const endChain = (key: string): void => {
    if (!chains.delete(key) || anyExecuting()) return;
    let latest: Chain | undefined;
    for (const chain of chains.values()) {
      if (chain.wait !== undefined && (latest === undefined || chain.waitSeq > latest.waitSeq))
        latest = chain;
    }
    if (latest?.wait !== undefined) reportWaiting(latest.wait);
  };

  // 准入端口就是治理器端口的窄包装：快路径 = tryAdmit，排队 = admit（waiting(slot)
  // 由 runner 的 queued 事件报，这里不再自己发）。acquire 仍先试快路径，兼容没有走 tryAcquire 的调用方。
  const governed: ModelRequestAdmission | undefined =
    port === undefined
      ? undefined
      : {
          tryAcquire: ({ model }) => port.tryAdmit(runId, workflowConcurrencyKey(model)),
          acquire: async ({ model, signal }) => {
            const key = workflowConcurrencyKey(model);
            return (
              port.tryAdmit(runId, key) ??
              (await port.admit(runId, key, signal ?? new AbortController().signal))
            );
          },
        };
  // 本 run 自己的上界在**共享 cap 之上**再包一层。闸门要的那条子代理事实——
  // 此刻有没有工具在跑——就取自同一个对象里的工具观察面，所以两个读者共用一份账。
  const admission =
    seat === undefined
      ? governed
      : seat.gate.wrap(seat.key, { toolsInFlight: () => toolActivity.inFlight() }, governed);

  return {
    admission,
    reset: () => {
      chains.clear();
      executing = false;
      reportedWait = undefined;
      turnsResolved = 0;
      toolActivity.reset();
    },
    toolCounts: () => toolActivity.counts(),
    lastTool: () => toolActivity.lastTool(),
    noteTurnResolved: () => {
      turnsResolved++;
      const lastTool = toolActivity.lastTool();
      return {
        turn: turnsResolved,
        toolCalls: toolActivity.counts().toolCalls,
        ...(lastTool === undefined ? {} : { lastTool }),
      };
    },
    observe: (runtime, sessionId) => {
      toolActivity.observe(runtime, sessionId);
      if (typeof (runtime as Partial<AgentRuntime>).subscribeEvents !== "function") return;
      unsubscribeEvents = runtime.subscribeEvents({
        onSessionEvent: (event: SessionEvent) => {
          if (event.type !== SessionEventType.ModelNetworkStatus || event.sessionId !== sessionId)
            return;
          const status = event.payload as ModelNetworkStatusEvent;
          const key = chainKey(status);
          switch (status.type) {
            case "model_request_queued":
              setWaiting(key, "queued", { cause: "slot" });
              return;
            case "model_request_admitted":
            case "model_request_started":
              chains.set(key, { phase: "executing", waitSeq: chains.get(key)?.waitSeq ?? 0 });
              reportExecuting();
              return;
            case "model_retry_scheduled": {
              if (NON_FAILURE_RETRY_REASONS.has(status.reason)) return;
              handlers.onRetryScheduled?.(status.reason);
              setWaiting(key, "backoff", {
                cause: "backoff",
                reason: status.reason,
                attempt: status.nextAttempt,
                delayMs: status.delayMs,
                ...(status.retryAfterMs === undefined ? {} : { retryAfterMs: status.retryAfterMs }),
              });
              return;
            }
            case "model_request_completed":
              handlers.onRequestCompleted?.();
              endChain(key);
              return;
            case "model_request_failed":
              // retryable:true 的 failed 紧随一条 retry_scheduled——那条才改相位。
              if (!status.retryable) endChain(key);
              return;
            default:
              return;
          }
        },
      });
    },
    unsubscribe: () => {
      toolActivity.unsubscribe();
      unsubscribeEvents?.();
      unsubscribeEvents = undefined;
    },
  };
}

function sameWait(a: AskWaitInfo, b: AskWaitInfo): boolean {
  return (
    a.cause === b.cause &&
    a.reason === b.reason &&
    a.attempt === b.attempt &&
    a.delayMs === b.delayMs &&
    a.retryAfterMs === b.retryAfterMs
  );
}

function chainKey(status: ModelNetworkStatusEvent): string {
  const parts = [status.querySource, status.queryId, status.toolCallId].map((part) =>
    part === undefined ? "" : String(part),
  );
  return parts.some((part) => part.length > 0) ? parts.join("|") : status.requestId;
}

// ————————————————————————————— run 级 stall 时钟—————————————————————————————

/** 连续多久没有一次成功的模型请求就通知主代理一次。 */
const WORKFLOW_STALL_NOTIFY_AFTER_MS = 20 * 60_000;

/** 时钟与定时器（可注入，测试用假时间）。 */
export interface WorkflowClock {
  now?: () => number;
  /** 返回取消函数。 */
  schedule?: (callback: () => void, delayMs: number) => () => void;
}

interface RunStallClockOptions extends WorkflowClock {
  /** 缺省 {@link WORKFLOW_STALL_NOTIFY_AFTER_MS}。 */
  afterMs?: number;
  onStalled: (info: RunStallInfo) => void;
}

/**
 * 一个 run 的停滞观察：所有 actor（turn 与工具侧请求一体）共用一只表。
 *   - `noteSuccess`：任一 `model_request_completed` → 归零、撤闹钟、清 reason 计数，本段结束；
 *   - `noteRetryScheduled`：一次 `model_retry_scheduled` → 计 reason；本段还没上膛就定闹钟到
 *     「上次成功 + afterMs」；
 *   - 闹钟响：距上次成功 ≥ afterMs 且本段至少排定过一次重试 → `onStalled` 恰好一次（每个 stall
 *     段一条），直到下一次成功才重新上膛；
 *   - `noteCap`：治理器扇出的 cap 变化——通知里顺带报此刻的 cap（最近一次扇出的值；没见过就缺席）。
 * 纯观察，不做任何决策；run 结算后 `dispose` 撤闹钟。
 */
export interface RunStallClock {
  noteSuccess(): void;
  noteRetryScheduled(reason: string): void;
  noteCap(cap: number): void;
  dispose(): void;
}

export function createRunStallClock(options: RunStallClockOptions): RunStallClock {
  const now = options.now ?? Date.now;
  const schedule = options.schedule ?? defaultSchedule;
  const afterMs = options.afterMs ?? WORKFLOW_STALL_NOTIFY_AFTER_MS;
  let lastSuccessAt = now();
  let notified = false;
  let cancelTimer: (() => void) | undefined;
  let cap: number | undefined;
  const reasons = new Map<string, number>();
  let disposed = false;

  const dominantReason = (): string | undefined => {
    let best: string | undefined;
    let bestCount = 0;
    for (const [reason, count] of reasons) {
      if (count > bestCount) {
        best = reason;
        bestCount = count;
      }
    }
    return best;
  };
  const fire = (): void => {
    cancelTimer = undefined;
    if (disposed || notified) return;
    const sinceMs = now() - lastSuccessAt;
    if (sinceMs < afterMs) {
      // 时钟漂移 / 注入时钟不单调：按剩余量再等一次，而不是漏掉这一段。
      cancelTimer = schedule(fire, afterMs - sinceMs);
      return;
    }
    if (reasons.size === 0) return;
    notified = true;
    const reason = dominantReason();
    options.onStalled({
      sinceMs,
      ...(reason === undefined ? {} : { reason }),
      ...(cap === undefined ? {} : { cap }),
    });
  };

  return {
    noteSuccess: () => {
      if (disposed) return;
      lastSuccessAt = now();
      notified = false;
      reasons.clear();
      cancelTimer?.();
      cancelTimer = undefined;
    },
    noteRetryScheduled: (reason) => {
      if (disposed || notified) return;
      reasons.set(reason, (reasons.get(reason) ?? 0) + 1);
      if (cancelTimer !== undefined) return;
      cancelTimer = schedule(fire, Math.max(0, afterMs - (now() - lastSuccessAt)));
    },
    noteCap: (next) => {
      cap = next;
    },
    dispose: () => {
      disposed = true;
      cancelTimer?.();
      cancelTimer = undefined;
    },
  };
}

const defaultSchedule = (callback: () => void, delayMs: number): (() => void) => {
  const timer = setTimeout(callback, delayMs);
  // 不让一个等停滞的定时器把进程钉住：run 结束、进程要退出时它不该有投票权。
  if (typeof timer === "object" && timer !== null && "unref" in timer) timer.unref();
  return () => clearTimeout(timer);
};
