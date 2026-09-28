// AgentRuntime-backed WorkflowDriver：dispose 时的会话释放与静默状态。
// `dispose()` 不等待仍在执行的 turn；释放会话时记录这些 promise，供 amend 判断哪些会话已停止写入。
//
// 引擎结算并不表示被中止的 turn 已完成最后的消息持久化。amend 需要先等待会话静默，
// 再读取消息条数作为 `inFlight.messageBoundary`。已完成 ask 的边界已记入 journal，不受此影响。
// 等待超时的会话不导入未完成部分，后继从完整前缀重新执行，不能将超时视为静默。
//
// 此处只等待 turn 主线，不保证所有流式工具 part 都已落库：部分工具句柄可能已与 turn 的
// promise 脱离。转录复制读取当时可见的 part；历史恢复会将 pending/running 工具视为中断，
// 缺失的 part 不会生成无结果的 tool_use，空 assistant 消息会被跳过。
// part 按同一 ID 更新，不增加消息条数，因此迟到的 part 不会改变消息边界。

import type { WorkflowClock } from "./workflow-driver-concurrency.js";
import { defaultSchedule } from "./workflow-driver-helpers.js";
import type { AgentRuntimeWorkflowDriverDeps, SessionState } from "./workflow-driver-types.js";

/**
 * 等一个被中止的 turn 把尾巴写完的上界。
 *
 * 取值理由：它等的是**已经 abort 的** turn 走完 core 侧那几步已 await 的持久化
 * （abandon → 取消快照 → assistant 消息），是毫秒量级的收尾，不是一次模型请求。5 秒给足了
 * 慢磁盘与一次重试的余量，又不会让「修订一个在飞 run」这个交互明显变慢——而超了也只是
 * 少一次接续，不是失败。
 */
export const AMEND_TRANSCRIPT_QUIESCE_MS = 5_000;

/** 一个 run 的 actor 会话静默探询面。driver 私有能力，**不上** Boundary B 的 `WorkflowDriver`。 */
export interface ActorSessionQuiescence {
  /**
   * 此刻已经静默（不再有在写的 turn）的会话 id。有界等待，见 {@link AMEND_TRANSCRIPT_QUIESCE_MS}。
   *
   * 「不在集合里」有两种成因，调用方对两者同等处理（都不接续）：会话到点仍在写，或者这个
   * driver 压根不认识它——后者只会出现在「前驱这一世从没给该 actor 建过会话」的形状上，
   * 保守读成不静默即可（代价是少一次接续，而反过来是拿一个没人为其负责的条数去截断转录）。
   */
  quietSessions(): Promise<ReadonlySet<string>>;
}

/** driver 侧的写面：{@link ActorSessionQuiescence} 加一个 dispose 时的登记口。 */
export interface ActorSessionQuiescenceLedger extends ActorSessionQuiescence {
  /**
   * 登记一个会话与它**此刻**仍在飞的 turn 收尾链（`SessionState.turn`）。`pendingTurn` 缺席
   * 即该会话没有在飞 turn，立刻算静默。
   *
   * 由 `dispose()` 对它名下每个会话各调一次：那是唯一一个「还看得见全部会话、且已经没有新
   * turn 会被发起」的时刻（dispose 之后 `sessions` 就被清空了）。
   */
  noteDisposed(sessionId: string, pendingTurn: Promise<unknown> | undefined): void;
}

/**
 * 造一本会话静默账。纯内存、无 I/O；时钟由调用方注入。
 *
 * 所有权：每个 driver 实例恰好一本，随 driver 一起生灭。刻意**不做**进程级注册表——一个会话
 * 属于哪个 run 已经由 driver 的归属关系说清了，第二张表只会带来「两边不一致时信谁」。
 */
export function createActorSessionQuiescence(options?: {
  clock?: WorkflowClock;
  /** 缺省 {@link AMEND_TRANSCRIPT_QUIESCE_MS}。 */
  quiesceMs?: number;
}): ActorSessionQuiescenceLedger {
  /** sessionId → dispose 那一刻仍在飞的 turn（`undefined` = 当时就没有在飞 turn）。 */
  const disposed = new Map<string, Promise<unknown> | undefined>();
  const schedule = options?.clock?.schedule ?? defaultSchedule;
  const quiesceMs = options?.quiesceMs ?? AMEND_TRANSCRIPT_QUIESCE_MS;

  return {
    noteDisposed(sessionId, pendingTurn) {
      disposed.set(sessionId, pendingTurn);
    },

    async quietSessions() {
      const quiet = new Set<string>();
      const waits: Promise<unknown>[] = [];
      for (const [sessionId, pending] of disposed) {
        if (pending === undefined) {
          quiet.add(sessionId);
          continue;
        }
        // turn 收尾链的成败与静默无关：无论 resolve 还是 reject，它都不会再写这个会话了。
        waits.push(
          pending.then(
            () => quiet.add(sessionId),
            () => quiet.add(sessionId),
          ),
        );
      }
      if (waits.length > 0) await settleWithin(Promise.all(waits), quiesceMs, schedule);
      // 快照一份再交出去：到点之后才落地的会话仍会往 `quiet` 里补写，而调用方读到的必须是
      // 「问的那一刻」的事实——一个会在背后自己变大的集合比一个保守的集合危险得多。
      return new Set(quiet);
    },
  };
}

/**
 * dispose 的每会话释放：退订活动观察、撤掉退避重驱、**登记静默**，再等在飞 turn 落地后关
 * actor runtime。
 *
 * 对每个 runtime 跑 app 关会话的**同一条**链——`closeBrowserSession` 内部依次 beginShutdown、
 * node_repl 会话释放、浏览器会话关闭；不另造一套子代理关闭链，那会漂移。关闭失败只 warn，
 * 结算不因它抛。
 *
 * 登记必须在 `state.turn.then(...)` **之前**：此刻 `state.turn` 就是要等的那条链，而 then 会把
 * 它换成另一个 promise。整个函数同步返回（关闭挂在 then 上），dispose 因此一如既往不阻塞。
 */
export function releaseActorSessions(
  deps: AgentRuntimeWorkflowDriverDeps,
  sessions: Iterable<SessionState>,
  ledger: ActorSessionQuiescenceLedger,
): void {
  for (const state of sessions) {
    state.modelActivity.unsubscribe();
    state.cancelRedrive?.();
    state.cancelRedrive = undefined;
    ledger.noteDisposed(state.sessionId, state.turn);
    const close = (): void => closeActorRuntime(deps, state);
    if (state.turn === undefined) close();
    else state.turn.then(close, close);
  }
}

function closeActorRuntime(deps: AgentRuntimeWorkflowDriverDeps, state: SessionState): void {
  // Promise.resolve().then(...)：把同步抛出也归到同一条 warn 路径（最小 stub runtime 没有这个方法）。
  void Promise.resolve()
    .then(() => state.runtime.closeBrowserSession())
    .catch((error: unknown) => {
      deps.logger?.warn?.("Dynamic workflow actor runtime close failed", {
        errorMessage: error instanceof Error ? error.message : String(error),
        event: "dynamic_workflow.actor_runtime.close_failed",
        module: "bootstrap.app",
        sessionId: state.sessionId,
      });
    });
}

/** 等 `work` 落地，最多等 `delayMs`。到点即返回（不抛），闹钟在任一分支后都撤掉。 */
async function settleWithin(
  work: Promise<unknown>,
  delayMs: number,
  schedule: (callback: () => void, delayMs: number) => () => void,
): Promise<void> {
  let cancel: (() => void) | undefined;
  const deadline = new Promise<void>((resolve) => {
    cancel = schedule(resolve, delayMs);
  });
  try {
    await Promise.race([work, deadline]);
  } finally {
    // work 先落地时必须撤掉闹钟：一个还没响的 setTimeout 会把 CLI 的退出拖到上界。
    cancel?.();
  }
}
