/**
 * scheduler.ts 顶到 oxlint max-lines 上限（400 行），把调度器的内部类型（Deferred /
 * AskNode / Actor）与引擎注入的依赖面 SchedulerHost 拆到本文件；公开面仍从 scheduler.ts 导出
 * （SchedulerHost 在那里原地再导出，engine.ts 的导入路径不变）。
 *
 * 单独成文件的理由不只是行数：scheduler-submit.ts 里的自由函数也要拿到 AskNode / SchedulerHost，
 * 从这里导入，两侧都不必反向 import 调度器本体。
 */

import { INSTRUCTIONS_HEAD_MAX_CHARS, refToString, WorkflowError } from "./types.js";
import type { ImportedActorState } from "./imported-cache.js";
import type {
  ActorId,
  ActorRef,
  AskSpec,
  AskStats,
  Caps,
  InstanceRef,
  PersonaSpec,
  RunEvent,
  SessionRef,
  ValidateFn,
  WorkflowDriver,
} from "./types.js";

/** 一个可外部结算的 promise。 */
export interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason: unknown) => void;
}

export function defer<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** 引擎注入给调度器的依赖面。 */
export interface SchedulerHost {
  readonly runId: string;
  /**
   * 本 run 的并发上界。**每次派发前现读**，不是构造时抄下的一份：`setMaxConcurrency` 会整份
   * 换掉引擎持有的 caps，
   * 而调度器的派发判据必须看见新值。引擎侧因此以 getter 实现这个属性。
   */
  readonly caps: Caps;
  readonly driver: WorkflowDriver;
  readonly validate: ValidateFn;
  /** 分配某站点的下一个执行序号（与 world-read/actor 共用一套计数器）。 */
  nextOrdinal(siteId: string): number;
  /**
   * 受 replay 结算次序约束地释放一次命中。
   * 非 resume、或次序表里没有这个实例时立即执行 `release`。
   */
  holdForReplay(instance: InstanceRef, release: () => void): void;
  /** 事件既落 journal 又扇出（Boundary C）。 */
  record(event: RunEvent): void;
  isRunSettled(): boolean;
  /** run 已结算时用于 reject 的错误。 */
  runError(): WorkflowError;
  /** run 级失败。 */
  failRun(error: WorkflowError): void;
  /**
   * 导入缓存是否已关闭（amend-resume）。关门由引擎自己做
   * （driver 上报 askMutating、或 live 的 world-run），调度器只读这个位——一个 ask 转 live 本身
   * **不**关门：它还什么都没改。
   */
  importCacheClosed(): boolean;
  /** 该记录行在崩溃前是否 live 跑过（resume 时引擎从事件恢复；非 resume 恒 false）。 */
  wasLiveBeforeResume(instance: InstanceRef): boolean;
  /**
   * 该记录行的准入是否发生在导入缓存关闭**之前**（按事件次序恢复，见 engine-world.ts 的
   * recoverImportClosure）。续跑前驱在飞 ask 的判定要它才能在 resume 时精确复原；非 resume 恒 false。
   */
  wasQueuedBeforeImportClose(instance: InstanceRef): boolean;
}

/** 一个 live（需真正派发执行）的 ask 节点。 */
export interface AskNode {
  instance: InstanceRef;
  actor: Actor;
  actorSeq: number;
  instructions: string;
  hash: string;
  spec: AskSpec;
  deferred: Deferred<unknown>;
  repairsRemaining: number;
  nudgesRemaining: number;
  settled: boolean;
  dispatched: boolean;
  lastStats?: AskStats;
  /**
   * 准入时算好的指令开头（{@link AskNode.instructions} 的前 N 字符）。存在节点上而不是两处
   * 各算一次：`node-queued` 与 `node-dispatched` 必须带**同一个**串（派发重复出生事实，
   * 见 types.ts 的 `node-dispatched`），存下来这件事就由构造保证，不靠两处调用保持同步。
   */
  instructionsHead?: string;
}

/** 调度器维护的 actor 运行态。 */
export interface Actor {
  ref: ActorRef;
  id: ActorId;
  persona: PersonaSpec;
  name?: string;
  /** journal 中该 actor 已记录的 ask 节点数——replay 时 live 节点须等其全部准入后才放行。 */
  recordedCount: number;
  /** 下一个待准入的 actorSeq。 */
  nextAdmitSeq: number;
  /** 已到达但未准入的记录节点释放动作，按 actorSeq 挂起（hold 规则）。 */
  pendingRecorded: Map<number, () => void>;
  /** 已到达但在等记录节点排空的 live 节点释放动作，按到达顺序。 */
  pendingLive: Array<() => void>;
  /** 已准入待派发的 live 节点（FIFO = 准入顺序）。 */
  liveQueue: AskNode[];
  /** 正在执行的 live 节点（actor 串行，至多一个）。 */
  current?: AskNode;
  /** 会话惰性创建，缓存其 promise（每 actor 一次）。 */
  sessionPromise?: Promise<SessionRef>;
  session?: SessionRef;
  /**
   * amend-resume 的导入消费态（引擎在 createActor 里按名 + persona 匹配后挂上，见
   * imported-cache.ts 的 `matchImportedActor`）。缺席即该 actor 全新重跑。
   */
  imported?: ImportedActorState;
}

// 合入后按当前格式化规则展开会超过调度器的 400 行限制；纯辅助函数与现有 defer 一起收在此处，行为不变。
/** replay 命中但 inputHash 不一致——纯度契约被破坏，run 大声失败。 */
export function hashMismatch(instance: InstanceRef, expected: string, got: string): WorkflowError {
  return new WorkflowError(
    "InputHashMismatch",
    `Replay hit at ${refToString(instance)} but inputHash differs (expected ${expected}, got ` +
      `${got}): the script is not deterministic, so the journal cannot be replayed.`,
    // 结构化 mismatch 与 ScriptHashMismatch 对齐：两个哈希不一致错误共用同一个字段，
    // 读端不必再从 message 文本里抠哈希。
    { mismatch: { expected, got } },
  );
}

/** cause → 一行有界文本（Error 取 message，其余 String()；空则给占位）。 */
export function describeCause(cause: unknown): string {
  const text = cause instanceof Error ? cause.message : String(cause);
  const trimmed = text.trim();
  if (trimmed.length === 0) return "unknown error";
  return trimmed.length > 300 ? `${trimmed.slice(0, 300)}…` : trimmed;
}

/**
 * 作者指令的开头（{@link INSTRUCTIONS_HEAD_MAX_CHARS} 个字符，去两端空白，**不加省略号**）。
 * 空指令返回 undefined：缺席的键比一个空串诚实——读面据此退回「不知道它被交代了什么」。
 */
export function headOfInstructions(instructions: string): string | undefined {
  const trimmed = instructions.trim();
  if (trimmed.length === 0) return undefined;
  return trimmed.length <= INSTRUCTIONS_HEAD_MAX_CHARS
    ? trimmed
    : trimmed.slice(0, INSTRUCTIONS_HEAD_MAX_CHARS);
}

/** 按原有准入顺序清空 actor 队列；派发仍由调度器唯一负责。 */
export function drainActorAdmission(actor: Actor): void {
  let progressed = true;
  while (progressed) {
    progressed = false;
    const release = actor.pendingRecorded.get(actor.nextAdmitSeq);
    if (release !== undefined) {
      actor.pendingRecorded.delete(actor.nextAdmitSeq);
      actor.nextAdmitSeq++;
      release();
      progressed = true;
      continue;
    }
    if (actor.nextAdmitSeq >= actor.recordedCount && actor.pendingLive.length > 0) {
      const admit = actor.pendingLive.shift()!;
      admit();
      progressed = true;
    }
  }
}
