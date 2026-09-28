// ============================================================
// AgentRuntime-backed WorkflowDriver：转录编排（amend-resume 的两件 driver 私有事）
// ============================================================
// workflow-driver.ts 顶到 oxlint max-lines 上限（400 行），把 driver 侧的转录编排——
// 分歧 actor 的转录截断（seedActorSession）、ask 边界记账的计数（countSessionTranscript）与回写
// （journalAskMessageBoundary）——拆到本文件成自由函数；公开面不变。机制本身仍住在
// workflow-actor-transcript.ts（复制与计数），这里只是 driver 对它的三次调用及其失败取舍，
// 原方法体逐字保留，只把 `this.deps` / `this.journal` 换成显式递进来的 deps。

import type { SessionId } from "@zcode/contracts";
import type { AgentRuntime } from "@zcode/core";
import {
  refToString,
  WorkflowError,
  type ActorSessionSeed,
  type InstanceRef,
} from "@zcode/dynamic-workflow";
import { countActorTranscript, seedActorTranscript } from "./workflow-actor-transcript.js";
import type { AgentRuntimeWorkflowDriverDeps, SessionState } from "./workflow-driver-types.js";

/**
 * 分歧 actor 的转录截断（amend-resume）：把源会话的前 N 条消息复制进刚铸的会话，再让 runtime
 * 以这段上文开场。
 *
 * 顺序是**载荷性**的，三步一步都不能换位：
 *   1. 工厂已返回 ⇒ 会话行已落库（`message.session_id` 对 `session(id)` 有 FK，先复制必失败）；
 *   2. 复制到**持久层**而不是内存 history——两个读者依赖落库的那一份：本 run 的重水化，
 *      以及**将来对本 run 的修订**（链行走会把这个会话当作转录源读）；
 *   3. 重水化走既有的 `resumeFromStore`（与 resume 重挂同一条机器），绝不另造一条水化路径。
 *      此刻会话行与消息都刚落好，所以这里不复制 launch 侧那条 `SessionNotFound → 全新`
 *      的降级分支：那条分支的成因是"会话被清理"，而在这里它只可能意味着接线错了，该大声失败。
 *
 * 第 3 步的**条件**：真复制了就必须水化；一条没抄（跳过）且 journal 已记下这个会话 id，
 * 说明 runtime 工厂刚才已经按 resume 路径重挂过了（launch 的 attachActorSession），再水化一次
 * 只会多发一条 SessionResumed、多跑一轮 SessionStart 钩子。两个条件都不成立的情形（跳过复制
 * 且 journal 无记录 = 上一世崩在复制与 putActor 之间）仍要水化，否则 runtime 会带着一个装满
 * 消息的会话从空上下文开跑。
 *
 * 这个条件对 {@link seedActorTranscript} 的**两种**跳过都成立，不必分辨是哪一种：两者的判据
 * 都是「目标会话已经有自己的内容」，而 `attachActorSession` 决定要不要重挂用的是同一个谓词
 * （journal 上有没有这个 actor 的 sessionId），所以两处永远同时成立或同时不成立。
 */
export async function seedActorSession(
  deps: AgentRuntimeWorkflowDriverDeps,
  input: {
    journaledSessionId: string | undefined;
    runtime: AgentRuntime;
    seed: ActorSessionSeed;
    sessionId: SessionId;
  },
): Promise<void> {
  const { journaledSessionId, runtime, seed, sessionId } = input;
  const store = deps.actorTranscriptStore;
  if (store === undefined) {
    throw new WorkflowError(
      "DriverError",
      `Subagent session ${sessionId} carries a transcript seed, but the driver has no ` +
        `transcript store (wiring error).`,
    );
  }
  const copied = await seedActorTranscript({
    ...(deps.logger === undefined ? {} : { logger: deps.logger }),
    seed,
    store,
    targetSessionId: sessionId,
  });
  if (copied === undefined && journaledSessionId !== undefined) return;
  await runtime.resumeFromStore();
}

/** 数本会话已落库的消息条数；失败即放弃记账（边界缺席，而不是一个错的边界）。 */
export async function countSessionTranscript(
  deps: AgentRuntimeWorkflowDriverDeps,
  state: SessionState,
  instance: InstanceRef,
): Promise<number | undefined> {
  const store = deps.actorTranscriptStore;
  if (store === undefined) return undefined;
  try {
    return await countActorTranscript(store, state.sessionId);
  } catch (error) {
    deps.logger?.warn?.("Dynamic workflow ask message boundary count failed", {
      errorMessage: error instanceof Error ? error.message : String(error),
      event: "dynamic_workflow.ask.message_boundary.count_failed",
      instance: refToString(instance),
      module: "bootstrap.app",
      sessionId: state.sessionId,
    });
    return undefined;
  }
}

/**
 * 把消息数边界补写进这个 ask 的 journal 行（`NodeRecord.messageBoundary`）。
 *
 * 与 stats 回填**同族**：引擎结算时写下的记录不含本字段（它是 driver 拥有的事实），所以这里
 * 走同一套读改写——`getNode` 拿到刚结算的整条记录，只加边界再 `putNode`，status / result /
 * actorSeq / inputHash / stats 一个不动。写在结算之后是必须的：引擎的结算是整条替换，写在
 * 它之前会被抹掉。
 *
 * 每个 ask 都写，不区分具名/匿名、修订/普通 run：**匿名与否是导入时才判定的**，而任何 run
 * 都是未来修订的潜在前驱。代价是每 ask 一次小写入。
 *
 * 两处早退各有理由：记录不在（run 已被清理）无处可写；`currentInstance` 已经换人说明这个会话
 * 上已经开始了下一个 ask，此刻数到的长度不再属于本次交换——宁可让边界缺席（该 ask 不可导入），
 * 也不写一个偏大的值（截断会多带一段下一次 ask 的开场）。
 */
export function journalAskMessageBoundary(
  deps: AgentRuntimeWorkflowDriverDeps,
  state: SessionState,
  instance: InstanceRef,
  boundary: number,
): void {
  const key = refToString(instance);
  if (state.currentInstance !== undefined && refToString(state.currentInstance) !== key) return;
  const runId = deps.runId ?? "run";
  try {
    const recorded = deps.journal.getNode(runId, instance.siteId, instance.ordinal);
    if (recorded === undefined) return;
    deps.journal.putNode({ ...recorded, messageBoundary: boundary });
  } catch (error) {
    // 记账写入失败不该打挂一个已经结算好的 ask（且这里跑在游离的 promise 上，抛出只会变成
    // unhandled rejection）。代价与计数失败相同：这个 ask 不可导入，run 因此不可作前驱。
    deps.logger?.warn?.("Dynamic workflow ask message boundary write failed", {
      errorMessage: error instanceof Error ? error.message : String(error),
      event: "dynamic_workflow.ask.message_boundary.write_failed",
      instance: key,
      module: "bootstrap.app",
      runId,
    });
  }
}
