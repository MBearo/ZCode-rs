/**
 * ask 调度器：把"每 actor FIFO + hold 规则 + 并发上限派发 + ask 结算"从引擎主体拆出，
 * 使两个文件都聚焦、可读（并满足单文件行数上限）。引擎通过 {@link SchedulerHost} 注入其
 * 依赖（driver、caps、序号分配、run 级失败），调度器只负责 ask 生命周期，不管 run 生命周期。
 *
 * 关键规则见 engine.ts 顶部说明：命中短路、actorSeq 准入顺序、replay 的 hold 规则、串行 actor。
 *
 * 内部类型与 SchedulerHost 在 scheduler-types.ts；submit / turn 的向上回报处理在 scheduler-submit.ts
 * （拆分原因：oxlint max-lines 上限 400 行）。SchedulerHost 在此原地再导出，导入路径不变。
 */

import { inputHash } from "./hash.js";
import { importedAskRecord, type ImportedActorState } from "./imported-cache.js";
import {
  defer,
  drainActorAdmission,
  hashMismatch,
  describeCause,
  headOfInstructions,
  type Actor,
  type AskNode,
  type Deferred,
  type SchedulerHost,
} from "./scheduler-types.js";
import { handleSubmitAttempted, handleTurnEnded, type SubmitSeam } from "./scheduler-submit.js";
import type {
  ActorId,
  ActorRef,
  AskMessage,
  AskSpec,
  AskStats,
  InstanceRef,
  JournalStorePort,
  NodeRecord,
  PersonaSpec,
  SessionRef,
} from "./types.js";
import { NUDGE_ATTEMPTS, refToString, REPAIR_ATTEMPTS, WorkflowError } from "./types.js";

export { hashMismatch } from "./scheduler-types.js";
export type { SchedulerHost } from "./scheduler-types.js";

export class AskScheduler {
  private readonly actors = new Map<ActorId, Actor>();
  private readonly actorOrder: Actor[] = [];
  private readonly liveNodes = new Map<string, AskNode>();
  /**
   * 「转录早于本次派发」的 ask 实例（`siteId@ordinal`）：续跑前驱在飞 ask 的，以及 resume 时
   * 据 running 行重新派发的。它们落 journal 的 stats 要抹掉 `worldToolCalls`，见 {@link journaledStats}。
   * 结算后不清理——迟到的 stats 回填还要查它。
   */
  private readonly priorTranscriptAsks = new Set<string>();
  private activeAsks = 0;

  /** scheduler-submit.ts 的自由函数经它查 live 节点、按结果结算（见 {@link SubmitSeam}）。 */
  private readonly submitSeam: SubmitSeam;

  constructor(private readonly host: SchedulerHost) {
    this.submitSeam = {
      host,
      liveNode: (instance) => this.liveNodes.get(refToString(instance)),
      settleOk: (node, artifact) => this.settleOk(node, artifact),
      settleFailed: (node, error) => this.settleFailed(node, error),
    };
  }

  private get journal(): JournalStorePort {
    return this.host.driver.journal;
  }

  hasActor(id: ActorId): boolean {
    return this.actors.has(id);
  }

  /**
   * 注册一个 actor（创建站点 × 序号）。`imported` 是 amend-resume 的导入候选：引擎已在
   * createActor 里按有效名 + 规范化 persona 匹配过，调度器只负责消费它。
   */
  registerActor(
    ref: ActorRef,
    id: ActorId,
    name: string | undefined,
    persona: PersonaSpec,
    imported?: ImportedActorState,
  ): Actor {
    const recordedCount = this.journal
      .listNodes(this.host.runId)
      .filter(
        (n) => n.kind === "ask" && n.actorSiteId === ref.siteId && n.actorOrdinal === ref.ordinal,
      ).length;
    const actor: Actor = {
      ref,
      id,
      persona,
      name,
      recordedCount,
      nextAdmitSeq: 0,
      pendingRecorded: new Map(),
      pendingLive: [],
      liveQueue: [],
      ...(imported === undefined ? {} : { imported }),
    };
    // 不能把 journal 记录的 sessionId 预解析成 actor.session/sessionPromise：
    // 让 resume 后的 ensureSession 跳过 driver.createActorSession。但生产 driver 的会话表
    // **只**在 createActorSession 里填充，startAsk 按表查会话——于是 resume 重新派发的每个
    // ask 都以「未知会话」失败。纯 replay（零派发）与 fake driver（startAsk 不查表）都暴露
    // 不了它。会话身份本就归 driver 所有（生产铸造函数按 (runId, actorRef) 纯确定，重挂时
    // 铸出同一个 id）；journal 的 dwf_actor.session_id 是记录、不是权威，所以这里绝不从
    // journal 短路会话——首次派发一律走 driver.createActorSession（见 ensureSession）。
    this.actors.set(id, actor);
    this.actorOrder.push(actor);
    return actor;
  }

  /** 受理一次 ask：完结命中短路（走 hold 规则），running 命中或未命中则 live 派发。 */
  admitAsk(
    siteId: string,
    actorId: ActorId,
    instructions: string,
    spec: AskSpec,
  ): Promise<unknown> {
    const actor = this.actors.get(actorId)!;
    const ordinal = this.host.nextOrdinal(siteId);
    const instance: InstanceRef = { siteId, ordinal };
    const hash = inputHash(instructions);
    const deferred = defer<unknown>();

    const recorded = this.journal.getNode(this.host.runId, siteId, ordinal);
    if (recorded !== undefined) {
      // 命中即防御性校验 inputHash——不一致说明纯度契约被破坏，run 大声失败。
      if (recorded.inputHash !== hash) {
        const err = hashMismatch(instance, recorded.inputHash, hash);
        this.host.failRun(err);
        return Promise.reject(err);
      }
      const seq = recorded.actorSeq ?? 0;
      const reconcile = (): void =>
        actor.imported?.reconcileRecorded(
          seq,
          recorded.inputHash,
          this.host.wasLiveBeforeResume(instance),
          this.host.wasQueuedBeforeImportClose(instance),
        );
      if (recorded.status === "running") {
        // 崩溃于执行中：按记录的 actorSeq 位置重新 live 派发（hold 规则保证其准入次序）。
        // 这一条的转录里已经有上一轮跑出来的那些消息，所以按「转录早于本次派发」记 stats。
        actor.pendingRecorded.set(seq, () => {
          reconcile();
          this.admitLive(instance, actor, seq, instructions, hash, spec, deferred, true);
        });
      } else {
        // completed / failed：短路结算，无 driver 调用。
        actor.pendingRecorded.set(seq, () => {
          reconcile();
          this.releaseCachedAsk(instance, recorded, deferred);
        });
      }
      this.drainAdmission(actor);
      return deferred.promise;
    }

    // 未命中（fresh）：注册到 pendingLive，按到达顺序在记录节点排空后准入并分配新的 actorSeq。
    // 分配到 seq 之后先问导入缓存（amend-resume）：命中即 cached settle，不 live。
    actor.pendingLive.push(() => {
      const seq = actor.nextAdmitSeq++;
      if (this.tryImportedSettle(instance, actor, seq, hash, deferred)) return;
      // 未命中之后才知道它是不是「续跑前驱的在飞 ask」——判定在 take 里随分歧一起做出。
      const carried = actor.imported?.carriedAt(seq) === true;
      this.admitLive(instance, actor, seq, instructions, hash, spec, deferred, carried);
    });
    this.drainAdmission(actor);
    this.pumpAll();
    return deferred.promise;
  }

  /**
   * 把一个 ask 作为 live 节点准入：建节点、准入即落 running 记录、入队并记事件。
   *
   * `priorTranscript` 为真表示这一条不是从空转录开跑的（续跑前驱在飞 ask，或 resume 据 running
   * 行重新派发）——它只影响 stats 怎么落库，见 {@link journaledStats}。
   */
  private admitLive(
    instance: InstanceRef,
    actor: Actor,
    seq: number,
    instructions: string,
    hash: string,
    spec: AskSpec,
    deferred: Deferred<unknown>,
    priorTranscript = false,
  ): void {
    if (priorTranscript) this.priorTranscriptAsks.add(refToString(instance));
    // 指令开头随出生事件一起落轨：这里的 instructions 还是
    // 作者的原文——driver 的质量 / schema 尾注在 startAsk 里才追加，所以摘要里不会混进引擎的话。
    // 算一次存在节点上：派发要重复出生事实，两条事件带的必须是同一个串。
    const instructionsHead = headOfInstructions(instructions);
    const node: AskNode = {
      instance,
      actor,
      actorSeq: seq,
      instructions,
      hash,
      spec,
      deferred,
      repairsRemaining: REPAIR_ATTEMPTS,
      nudgesRemaining: NUDGE_ATTEMPTS,
      settled: false,
      dispatched: false,
      ...(instructionsHead === undefined ? {} : { instructionsHead }),
    };
    this.liveNodes.set(refToString(instance), node);
    // 准入即落 running（携 actorSeq + inputHash）：崩溃于执行中的节点 resume 可据此重新派发。
    this.journal.putNode({
      runId: this.host.runId,
      siteId: instance.siteId,
      ordinal: instance.ordinal,
      kind: "ask",
      actorSiteId: actor.ref.siteId,
      actorOrdinal: actor.ref.ordinal,
      actorSeq: seq,
      inputHash: hash,
      status: "running",
    });
    actor.liveQueue.push(node);
    this.host.record({
      type: "node-queued",
      instance,
      kind: "ask",
      actor: actor.ref,
      actorSeq: seq,
      ...(instructionsHead === undefined ? {} : { instructionsHead }),
    });
    // 转 live **不**关导入缓存。曾经在这里关：任一 ask live
    // 即视为工作区可能被改写。那是拿时钟代替依赖——同一个 Promise.all 里排在第一个未命中之后的
    // 兄弟 ask 全被判失效，而它们之间没有任何依赖（实测 50 路扇出丢 6 个命中，5 路扇出
    // 丢掉唯一的 1 个）。现在关门的是**第一笔写入**：driver 在子代理即将执行改写工具时上报
    // askMutating，或一条 world.run live 执行；在此之前的世界与前驱留下的世界相同，缓存命中都成立。
  }

  /**
   * fresh ask 准入时问一次导入缓存（amend-resume）。命中即 **cached settle** 并返回 true
   * （调用方不再 live）；判定与分歧记账全在 {@link ImportedActorState}。
   *
   * 命中写一行**真** dwf_node，只发 `node-settled cached:true`——与 replay 命中的
   * {@link releaseCachedAsk} 同一副姿态：不发 node-queued / node-dispatched，
   * 不入 liveQueue，不建会话，不占 `actor.current`。
   */
  private tryImportedSettle(
    instance: InstanceRef,
    actor: Actor,
    seq: number,
    hash: string,
    deferred: Deferred<unknown>,
  ): boolean {
    // 缓存已关闭 ⇒ 只放**纯** ask（前驱记下 toolCalls === 0）：它只依赖指令与转录前缀，与工作区
    // 无关，所以对旧世界的答案对新世界同样成立；带工具的条目即便同哈希也转 live——它读过的
    // 工作区可能已被改写。转 live 的那个 ask 让该 actor 分歧（转录从此不同），已消费前缀仍是种子
    // 边界的依据。
    const entry = this.host.importCacheClosed()
      ? actor.imported?.takeIfPure(seq, hash)
      : actor.imported?.take(seq, hash);
    if (entry === undefined) return false;
    this.journal.putNode(importedAskRecord(this.host.runId, instance, actor.ref, seq, hash, entry));
    this.host.record({ type: "node-settled", instance, outcome: "ok", cached: true });
    deferred.resolve(entry.result);
    return true;
  }

  // ——————————————————————————————— 向上回报 ———————————————————————————————

  /** submit_result 到达：方法体在 scheduler-submit.ts 的 handleSubmitAttempted。 */
  submitAttempted(instance: InstanceRef, payload: unknown): void {
    handleSubmitAttempted(this.submitSeam, instance, payload);
  }

  /** turn 结束（无 submit）：方法体在 scheduler-submit.ts 的 handleTurnEnded。 */
  turnEnded(instance: InstanceRef, finalText: string): void {
    handleTurnEnded(this.submitSeam, instance, finalText);
  }

  noteStats(instance: InstanceRef, stats: AskStats): void {
    const node = this.liveNodes.get(refToString(instance));
    if (node !== undefined) {
      node.lastStats = stats;
      return;
    }
    // 节点已离开 liveNodes——typed-accept 主导路径：submit 停 turn 并在 settle 时落库，而真实
    // actor 的用量在 turn 解析后（submit 之后）才知道，故 stats 在结算之后才到达。回填已结算的
    // journal 记录：只新增/覆写 stats，保留 status/result/actorSeq/inputHash/error/kind/actor 身份。
    // 尽力而为且幂等；预算扣减仍在 engine.askStats（此处只补 journal 完整性）。
    const recorded = this.journal.getNode(this.host.runId, instance.siteId, instance.ordinal);
    if (recorded === undefined) return;
    this.journal.putNode({ ...recorded, stats: this.journaledStats(instance, stats) });
  }

  /**
   * 落 journal 的 stats：**转录早于本次派发**的 ask 抹掉 `worldToolCalls`。
   *
   * 根因：driver 的工具计数器是内存态、每次 ask 起跑时清零，所以接着一段既有转录跑的 ask 只数
   * 得到自己这几轮，转录里原有的工具调用一个也数不到。少报的后果不是账目不准（那是 tokens 的
   * 事），而是**纯度判据被污染**：一个报 0 的 ask 会被后来的修订当成纯条目，在关门之后仍从缓存
   * 结算——而它其实碰过工作区。缺席这个键本就表示「碰过」（保守读法），所以抹掉才是诚实记录。
   * tokens / toolCalls / turns 照记：它们是用量，不是纯度声明。
   */
  private journaledStats(instance: InstanceRef, stats: AskStats): AskStats {
    if (!this.priorTranscriptAsks.has(refToString(instance))) return stats;
    const { worldToolCalls: _unreliable, ...rest } = stats;
    return rest;
  }

  failed(instance: InstanceRef, error: WorkflowError): void {
    const node = this.liveNodes.get(refToString(instance));
    if (node === undefined || node.settled) return;
    this.settleFailed(node, error);
  }

  /** 该实例是否仍是在飞（已准入、未结算）的 live ask——限流观察事件只对这样的节点有意义。 */
  isLive(instance: InstanceRef): boolean {
    const node = this.liveNodes.get(refToString(instance));
    return node !== undefined && !node.settled;
  }

  /** 在飞 ask 所属子代理的有效名（关门事件里点名用）；不在飞即 undefined。 */
  liveActorName(instance: InstanceRef): string | undefined {
    const node = this.liveNodes.get(refToString(instance));
    return node === undefined || node.settled ? undefined : node.actor.persona.name;
  }

  /** 中止所有在飞 ask：run 取消/失败时用。emitCancelled 为真时补发 node-settled(cancelled)。 */
  abortInFlight(error: WorkflowError, emitCancelled: boolean): void {
    for (const node of this.liveNodes.values()) {
      if (node.settled) continue;
      node.settled = true;
      if (node.dispatched) this.host.driver.cancelAsk(node.instance);
      if (emitCancelled) {
        this.host.record({ type: "node-settled", instance: node.instance, outcome: "cancelled" });
      }
      node.deferred.reject(error);
    }
    this.liveNodes.clear();
    this.activeAsks = 0;
  }

  // ——————————————————————————————— 内部：准入 / 派发 ———————————————————————————————

  private drainAdmission(actor: Actor): void {
    drainActorAdmission(actor);
    this.pumpActor(actor);
  }

  /**
   * 命中记录的 ask：按 journal 记下的**结算次序**释放。hold 规则定的是同一个 actor 内的准入次序，跨 actor 的
   * 完成次序要这一道闸才回得来——扇出分支在 await 之后的每一次 journal 调用都按它编号。
   */
  private releaseCachedAsk(
    instance: InstanceRef,
    recorded: NodeRecord,
    deferred: Deferred<unknown>,
  ): void {
    this.host.holdForReplay(instance, () => this.settleCachedAsk(instance, recorded, deferred));
  }

  private settleCachedAsk(
    instance: InstanceRef,
    recorded: NodeRecord,
    deferred: Deferred<unknown>,
  ): void {
    if (recorded.status === "completed") {
      this.host.record({ type: "node-settled", instance, outcome: "ok", cached: true });
      deferred.resolve(recorded.result);
    } else {
      // 已记录的失败也要短路复现：脚本可能已 try/catch 过它并据此分支，replay 必须重放同一 rejection。
      this.host.record({
        type: "node-settled",
        instance,
        outcome: "failed",
        cached: true,
        error: recorded.error,
      });
      deferred.reject(WorkflowError.fromJSON(recorded.error!));
    }
  }

  /**
   * 重扫每个 actor 的待派发队列。公开面**只为一个调用方**：引擎在抬高本 run 的并发上界之后
   * 调它。上界本身是
   * {@link pumpActor} 每次派发前现读的，但没有任何别的事件会触发一次重扫——排队的 ask
   * 否则要一直等到下一次结算才动，而「抬高之后立刻多跑几个」正是这条命令买的东西。
   */
  pumpAll(): void {
    for (const actor of this.actorOrder) this.pumpActor(actor);
  }

  private pumpActor(actor: Actor): void {
    if (this.host.isRunSettled()) return;
    if (actor.current !== undefined) return;
    if (actor.liveQueue.length === 0) return;
    if (this.activeAsks >= this.host.caps.maxConcurrency) return;
    const node = actor.liveQueue.shift()!;
    actor.current = node;
    this.activeAsks++;
    void this.dispatch(node);
  }

  private async dispatch(node: AskNode): Promise<void> {
    let session: SessionRef;
    try {
      session = await this.ensureSession(node.actor);
    } catch (cause) {
      if (this.host.isRunSettled() || node.settled) return;
      // 把 cause 的文本带进 message：WorkflowError.toJSON 只落 code/message，cause 不进 journal 也
      // 无人记日志，于是「创建 actor 会话失败」在 GUI / journal 里成了无法诊断的黑盒
      // （实机上底层其实是 session_task_link 的 FOREIGN KEY constraint failed）。
      this.settleFailed(
        node,
        new WorkflowError(
          "DriverError",
          `Failed to create the subagent session: ${describeCause(cause)}`,
          { cause },
        ),
      );
      return;
    }
    if (this.host.isRunSettled() || node.settled) return;
    // node-dispatched 在会话就绪之后。进程级并发闸门
    // 不在这里：它按**模型请求**准入，住在 driver 之下的 runtime deps 里；调度器只守
    // per-run 的 ask 级上界。
    // 这一条重复该实例的出生事实（types.ts 的 `node-dispatched`）：调度器手上现成的那几样
    // 与它的 `node-queued` 同源，两个出生阶段名由引擎在 record 里按同一张铸造表补。
    this.host.record({
      type: "node-dispatched",
      instance: node.instance,
      kind: "ask",
      actor: node.actor.ref,
      ...(node.actor.name === undefined ? {} : { actorName: node.actor.name }),
      ...(node.instructionsHead === undefined ? {} : { instructionsHead: node.instructionsHead }),
    });
    node.dispatched = true;
    const message: AskMessage = {
      instructions: node.instructions,
      typed: node.spec.typed,
      schema: node.spec.schema,
    };
    this.host.driver.startAsk(session, node.instance, message);
  }

  private ensureSession(actor: Actor): Promise<SessionRef> {
    if (actor.sessionPromise !== undefined) return actor.sessionPromise;
    // 种子只有到分歧点才知道（运行期发现），所以必须在这里、由引擎侧交给 driver——引擎持有
    // 导入态，driver 持有会话 store，这个签名是两者的最小汇合点。
    const seed = actor.imported?.seed();
    const promise = this.host.driver
      .createActorSession(actor.ref, actor.persona, seed)
      .then((session) => {
        actor.session = session;
        // resolvedModel 由宿主侧的 runtime 工厂在 createActorSession **内部**写下（它才知道
        // "lite" 落到哪个模型）。putActor 是整条记录的替换，所以这条写入必须把刚写下的值读回来
        // 带过去，否则这里就会把它抹掉——档位解析的审计与 resume 依据随之丢失。
        const resolvedModel = this.journal.getActor(
          this.host.runId,
          actor.ref.siteId,
          actor.ref.ordinal,
        )?.resolvedModel;
        this.journal.putActor({
          runId: this.host.runId,
          siteId: actor.ref.siteId,
          ordinal: actor.ref.ordinal,
          name: actor.name,
          persona: actor.persona,
          sessionId: session.id,
          resolvedModel,
        });
        return session;
      });
    actor.sessionPromise = promise;
    return promise;
  }

  // ——————————————————————————————— 内部：结算 ———————————————————————————————

  private settleOk(node: AskNode, artifact: unknown): void {
    if (node.settled) return;
    node.settled = true;
    this.journal.putNode(this.nodeRecordFor(node, { status: "completed", result: artifact }));
    this.host.record({ type: "node-settled", instance: node.instance, outcome: "ok" });
    this.finishLiveNode(node);
    node.deferred.resolve(artifact);
  }

  private settleFailed(node: AskNode, error: WorkflowError): void {
    if (node.settled) return;
    node.settled = true;
    // 结算失败必须落 journal（覆盖准入时的 running）：失败是"完结"，且脚本可能已观察到该 rejection
    // 并据此分支，replay 必须复现它——journal 化失败是重放正确性的硬性要求，而非可选。
    this.journal.putNode(this.nodeRecordFor(node, { status: "failed", error: error.toJSON() }));
    this.host.record({
      type: "node-settled",
      instance: node.instance,
      outcome: "failed",
      error: error.toJSON(),
    });
    this.finishLiveNode(node);
    // 节点失败只 reject 该 ask，不失败整个 run（脚本可 try/catch）。
    node.deferred.reject(error);
  }

  private finishLiveNode(node: AskNode): void {
    this.liveNodes.delete(refToString(node.instance));
    if (node.actor.current === node) {
      node.actor.current = undefined;
      this.activeAsks--;
    }
    this.pumpAll();
  }

  private nodeRecordFor(
    node: AskNode,
    outcome:
      | { status: "completed"; result: unknown }
      | { status: "failed"; error: NodeRecord["error"] },
  ): NodeRecord {
    const record: NodeRecord = {
      runId: this.host.runId,
      siteId: node.instance.siteId,
      ordinal: node.instance.ordinal,
      kind: "ask",
      actorSiteId: node.actor.ref.siteId,
      actorOrdinal: node.actor.ref.ordinal,
      actorSeq: node.actorSeq,
      inputHash: node.hash,
      status: outcome.status,
    };
    if (outcome.status === "completed") record.result = outcome.result;
    else record.error = outcome.error;
    if (node.lastStats !== undefined)
      record.stats = this.journaledStats(node.instance, node.lastStats);
    return record;
  }
}
