/**
 * amend-resume 导入缓存的**消费态**。
 *
 * 注入的 {@link ImportedRunCache} 是调用方构建的只读数据——run service 读前驱 journal 得到它，
 * 崩溃后 resume 用同一个纯函数重建同一张表。所以「消费到哪了」不能就地写回那张表，必须是引擎
 * 自己的状态：本模块就是这份状态，以及围绕它的全部判定（命中 / 分歧 / 种子）。
 *
 * 从 scheduler.ts 与 engine.ts 拆出来，理由同当初 scheduler 从 engine 拆出来：三个文件各自
 * 聚焦、可读，且满足单文件行数上限。
 */

import { canonicalJson } from "./hash.js";
import type {
  ActorRef,
  ActorSessionSeed,
  ImportedActorCandidate,
  ImportedAskEntry,
  ImportedRunCache,
  ImportedWorldEntry,
  InstanceRef,
  NodeRecord,
  PersonaSpec,
} from "./types.js";

/**
 * 一个已附着候选的 actor 的导入消费态：消费游标 + 分歧标志。
 *
 * **分歧单调**：`diverged` 一旦为真永不回头查缓存，导入后缀弃置不可复活。
 */
export class ImportedActorState {
  /** 已消费的导入条目数 = 下一个可命中的 actorSeq（也是转录截断边界的下标 + 1）。 */
  private consumed = 0;
  private diverged = false;
  /**
   * 「续跑前驱在飞 ask」的那个 seq（没有续跑即缺席）。至多一个：续跑本身就是分歧，
   * 分歧单调，所以第二次不会再有。
   */
  private carriedSeq?: number;

  constructor(private readonly candidate: ImportedActorCandidate) {}

  /**
   * fresh ask 在 seq 上问缓存：命中返回条目（并推进游标），否则置分歧并返回 undefined。
   *
   * 两种不命中同等处理：哈希不符（指令变了）与 `seq >= entries.length`（新脚本在此 actor 上
   * 扩了新 ask）。分歧的**级联**是免费且动态的——上游 ask 转 live 拿到新结果，下游用它插值出的
   * 指令哈希必变，于是下游自动分歧，不需要任何显式传播。
   *
   * 本方法只在导入缓存**开着**时被调用（关门后走 {@link takeIfPure}），所以未命中时可以谈续跑。
   */
  take(seq: number, hash: string): ImportedAskEntry | undefined {
    if (this.diverged) return undefined;
    const entry = this.candidate.entries[seq];
    if (entry === undefined || entry.inputHash !== hash) {
      this.diverge(seq, hash, true);
      return undefined;
    }
    this.consumed = seq + 1;
    return entry;
  }

  /**
   * 缓存关闭后的问法：只有**纯**条目（前驱记下 `worldToolCalls === 0`）还能命中。纯 ask 只依赖指令与
   * 转录前缀——两者都在哈希链里——与工作区无关，所以关门不影响它的答案。碰过外部世界的条目即便同哈希
   * 也不给：它读过的工作区可能已被改写；这个 ask 转 live，actor 从此分歧（转录不再与前驱一致，
   * 后缀条目不可复活）。没有 stats、或 stats 里没有这个键的老条目按「碰过」处理（保守）。
   *
   * 关门后同样不续跑在飞 ask：那半场转录里全是前驱对**旧工作区**的观察，与带工具的条目同理。
   */
  takeIfPure(seq: number, hash: string): ImportedAskEntry | undefined {
    if (this.diverged) return undefined;
    const entry = this.candidate.entries[seq];
    if (entry === undefined || entry.inputHash !== hash || entry.stats?.worldToolCalls !== 0) {
      this.diverge(seq, hash, false);
      return undefined;
    }
    this.consumed = seq + 1;
    return entry;
  }

  /** 该 seq 的 ask 是否续跑了前驱的在飞 ask（调度器据此决定 stats 记不记 worldToolCalls）。 */
  carriedAt(seq: number): boolean {
    return this.carriedSeq === seq;
  }

  /**
   * 据一行**已记录**的 ask 重推分歧状态（修订 run 崩溃后 resume 时用）。
   *
   * 分歧状态不落库，而修订 run 的 resume 会把导入缓存整表重建。若不据 journal 行重推分歧点，
   * 一个在原次执行中已于 seq k 分歧的 actor，其 seq k+n 的 fresh ask 可能恰好撞上导入条目的
   * 哈希而被**错误导入**——那等于把一段与本 run 实际转录无关的历史塞回来。逐 seq 拿记录行的
   * inputHash 与导入条目比对，恰好重建了原次执行当时的判定（准入按 seq 升序，hold 规则保证
   * 这一点），因此这个重推是精确的，不是保守近似。`wasLive` 补上哈希看不见的那一种 live
   * （缓存关闭后带工具的 ask，见 scheduler 的 tryImportedSettle）。
   *
   * `queuedBeforeClose` 是续跑判定的那一半事实：续跑与该 ask 的 `node-queued` 在准入的同一个
   * 同步片里发生，所以「当时门开着」等价于「这条 node-queued 早于第一条 import-cache-closed」
   * （见 engine-world.ts 的 recoverImportClosure）。必须精确——错判成续跑会让 seedActorTranscript
   * 往一个**已经分歧**的会话里多抄一段前驱消息（driver 的幂等判据只看目标够不够长）。
   */
  reconcileRecorded(
    seq: number,
    recordedHash: string,
    wasLive: boolean,
    queuedBeforeClose: boolean,
  ): void {
    if (this.diverged) return;
    // 缓存关闭之后，一个带工具 actor 的 ask 即便与导入
    // 条目同哈希也是 live 跑的——按哈希算成「已消费」会让种子边界取自前驱条目，而本会话的
    // 真实转录在那个位置根本不是那些消息。live 与否是事件里的事实（node-queued），不是哈希
    // 能推出来的。
    if (wasLive) {
      this.diverge(seq, recordedHash, queuedBeforeClose);
      return;
    }
    const entry = this.candidate.entries[seq];
    if (entry === undefined || entry.inputHash !== recordedHash) {
      // 没 live 过的行不可能是续跑（续跑的 ask 一定被派发过，一定有 node-queued）。
      this.diverged = true;
      return;
    }
    this.consumed = Math.max(this.consumed, seq + 1);
  }

  /**
   * 分歧 actor 的会话种子：源会话 + 复制多少条消息 + 承袭的模型 pin。
   *
   * 边界取**最后一条被消费**的导入条目的记账值：在 seq k 分歧意味着 0..k-1 的问答都已按缓存
   * 结算，新会话要接着的正是那 k 次完整交换之后的位置（含它们的 repair / nudge 轮）。
   * 一条也没消费就没有种子——全新会话、全新模型解析、不带 pin：pin 是为「转录接续下不静默
   * 换模型」存在的，没有接续就没有它的用武之地。
   *
   * **续跑是例外**：边界改取 `inFlight.messageBoundary`（前驱整个会话），因此一条都没消费也有
   * 种子——那正是「扇出第一轮在飞时被修订」的形状。
   */
  seed(): ActorSessionSeed | undefined {
    const inFlight = this.candidate.inFlight;
    if (this.carriedSeq !== undefined && inFlight !== undefined) {
      return this.seedAt(inFlight.messageBoundary);
    }
    if (this.consumed === 0) return undefined;
    const last = this.candidate.entries[this.consumed - 1];
    if (last === undefined) return undefined;
    return this.seedAt(last.messageBoundary);
  }

  /**
   * 置分歧，并在此顺带判定这次未命中是不是**续跑前驱的在飞 ask**。
   *
   * 续跑的四个条件缺一不可：此前未分歧（否则本 actor 的转录早已不是前驱那条）、`seq` 恰在
   * 前缀之后（在飞 ask 的位置）、整个前缀都已消费、指令哈希与在飞 ask 相符。`cacheOpen`
   * 是第五个：关门之后那半场转录只是对旧工作区的观察，不比缓存的 world 读取更可信。
   */
  private diverge(seq: number, hash: string, cacheOpen: boolean): void {
    const inFlight = this.candidate.inFlight;
    if (
      cacheOpen &&
      inFlight !== undefined &&
      seq === this.candidate.entries.length &&
      this.consumed === seq &&
      inFlight.inputHash === hash
    ) {
      this.carriedSeq = seq;
    }
    this.diverged = true;
  }

  private seedAt(messageCount: number): ActorSessionSeed {
    const seed: ActorSessionSeed = {
      sourceSessionId: this.candidate.transcriptSourceSessionId,
      messageCount,
    };
    if (this.candidate.resolvedModel !== undefined)
      seed.resolvedModel = this.candidate.resolvedModel;
    return seed;
  }
}

/**
 * world 节点的导入队列消费态：每个内容哈希一个游标（第 n 次出现对第 n 条记录）。
 * 与 {@link ImportedActorState} 同理，游标是引擎的状态，注入的队列保持只读。
 */
export class ImportedWorldQueue {
  private readonly cursors = new Map<string, number>();

  constructor(private readonly world: ReadonlyMap<string, ImportedWorldEntry[]>) {}

  /** 取该内容哈希的下一条记录，耗尽或从未记录即 undefined（调用方转 live）。 */
  take(hash: string): ImportedWorldEntry | undefined {
    const queue = this.world.get(hash);
    if (queue === undefined) return undefined;
    const cursor = this.cursors.get(hash) ?? 0;
    const entry = queue[cursor];
    if (entry === undefined) return undefined;
    this.cursors.set(hash, cursor + 1);
    return entry;
  }
}

/**
 * 为一个刚建出的 actor 找导入候选：按**有效名**查表，规范化 persona 一致才收。
 *
 * persona 比对用 `canonicalJson`——它跳过 undefined 成员、对象键排序，恰好就是要的规范化
 * （`{name:"a"}` 与 `{name:"a", system: undefined}` 同值）。不一致即**弃整个候选**，该 actor
 * 全新重跑：全保真转录下这是双重正确的——旧 system prompt 产的转录接新 persona 是身份错乱，
 * 而「我把 persona 修好了」这个意图本来就是要重跑。
 *
 * 匿名 actor 永不附着：名字是缓存身份键，没有名字就没有可比对的坐标（代价已裁决）。
 * 比对之所以在**运行期**而不是提交时静态比对两份脚本：名字与 persona 都是运行期值
 * （`agent()` 的实参可以是动态表达式），静态比对是第二份真相，恰是本包处处要防的。
 */
export function matchImportedActor(
  cache: ImportedRunCache | undefined,
  spec: PersonaSpec,
): ImportedActorState | undefined {
  if (cache === undefined) return undefined;
  const name = spec.name;
  if (name === undefined || name === "") return undefined;
  const candidate = cache.actors.get(name);
  if (candidate === undefined) return undefined;
  if (canonicalJson(spec) !== canonicalJson(candidate.persona)) return undefined;
  return new ImportedActorState(candidate);
}

/**
 * 一次 ask 缓存命中要落的**真** dwf_node 行（新 siteId、拷贝 result / stats / 边界）。
 *
 * 边界值必须一起拷过去，否则**这个** run 自己就不能再被修订——链式修订靠的正是 count offset
 * 跨前缀复制不变。
 */
export function importedAskRecord(
  runId: string,
  instance: InstanceRef,
  actor: ActorRef,
  seq: number,
  hash: string,
  entry: ImportedAskEntry,
): NodeRecord {
  const record: NodeRecord = {
    runId,
    siteId: instance.siteId,
    ordinal: instance.ordinal,
    kind: "ask",
    actorSiteId: actor.siteId,
    actorOrdinal: actor.ordinal,
    actorSeq: seq,
    inputHash: hash,
    status: "completed",
    result: entry.result,
    messageBoundary: entry.messageBoundary,
  };
  if (entry.stats !== undefined) record.stats = entry.stats;
  return record;
}
