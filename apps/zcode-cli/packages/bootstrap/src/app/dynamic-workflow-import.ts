// ============================================================
// amend-resume 的导入构建：读前驱 journal → ImportedRunCache
// ============================================================
//
// 本模块是 amend-resume 唯一的**读前驱**处，三个调用点共用它：
//   1. `port.amend` 的**预检**（{@link preflightAmendImport}）：停下在飞前驱之前就判定
//      `run_not_found` / `missing_boundaries`——被拒时前驱照旧在跑、一行不建；
//   2. `port.amend` 在前驱结算之后构建缓存（{@link buildImportedCache}）；
//   3. `port.resume` 见 `record.resumedFrom` 在场：崩溃后重建同一张表。
//
// 两处共用同一套构建规则：修订 run 的 journal 只包含已消费的执行前缀，未消费的导入需要重建。
// 已完成条目只由 journal 决定，遍历顺序取自 journal 的插入序；未完成 ask 的边界还依赖源转录。
//
// 一处例外要说清楚，它是这张表里**第一个不是 journal 纯函数**的数：在飞 ask 的接续
// （`inFlight`）的边界取自**前驱会话此刻的消息条数**，不是 journal 里的某一列。
//
// 它在两次构建之间大体稳定，靠的是「前驱已经结算、没人再写它的会话」：
//   - 提交时由 {@link AmendImportOptions.quietSessions} 保证（被取代的前驱刚 abort，driver 等它
//     的 turn 落地；没等到的会话不接续）；
//   - 重建时前驱早已终态、本进程里没有它的 driver，天然无人在写，所以不带这个集合（缺席 =
//     全部静默）。
//
// 但「大体」不是「一定」，而且**不必**是：两条路能让重建算出一个更大的 M——前驱被重新 resume
// 过又写了几轮（被 supersede 的前驱不可 resume，所以只有「修订一个早已 stopped 的 run、之后
// 又去 resume 它」构造得出），或者一条迟到的后台通知消息落进了那个会话。两者都只会让 M **变大**，
// 而正确性不依赖 M 稳定：
//   - 抄进去的那一侧已经把门关死了——`seedActorTranscript` 只往「空的、或只装着本会话种子
//     消息的」目标里写（workflow-actor-transcript.ts 的性质 2）。后继一旦跑出自己的消息，更大的
//     M 再送回来也一个字节都不会被写进去，所以绝无「前驱的消息被追加到本会话历史之后」这种
//     静默错乱；
//   - 后继还没跑出自己的消息时，更大的 M 抄来的是**同一条 ask 的更晚快照**，顺序仍由下标决定
//     （种子 id 按 (会话, 下标) 纯确定，重抄是 upsert），所以那只是多带一点上文，不会变错。
// 这条也是给**将来**每一个非 journal 事实的兜底：不必逐个去证明它们不会变大，复制点一次性堵死。
//
// 唯一的 I/O 是 journal 读与转录条数读（两者都经窄端口注入），本模块自己不碰会话存储实现。

import type { Logger, SessionId } from "@zcode/contracts";
import type {
  ActorRecord,
  ImportedActorCandidate,
  ImportedAskEntry,
  ImportedInFlightAsk,
  ImportedRunCache,
  ImportedWorldEntry,
  NodeRecord,
  RunRecord,
} from "@zcode/dynamic-workflow";
import { TERMINAL_RUN_STATUSES } from "./dynamic-workflow-run-observation.js";
import type { ActorTranscriptStore } from "./workflow-actor-transcript.js";

/**
 * 构建导入缓存所需的 journal 读面：三个读方法，全部按 runId。
 *
 * 结构上是引擎 `JournalStorePort` 的真子集（生产直接把 journal 传进来），窄化的理由与
 * {@link ActorTranscriptStore} 同款：构建器只读这三样，声明成整个端口会让「导入构建依赖
 * journal 的全部能力（含写）」变成一句真话——而它一个字节都不写。**前驱只读**是不变式，让类型把它说出来。
 */
interface ImportedCacheJournalReader {
  getRun(runId: string): RunRecord | undefined;
  listActors(runId: string): ActorRecord[];
  listNodes(runId: string): NodeRecord[];
}

/**
 * 导入构建被拒的三个理由。**判别键而非文案**：模型据它选下一步动作（换 run / 等它结算 /
 * 放弃修订走全新 run），所以三者必须可分辨。可操作文案归工具层。
 *
 * 与端口的 `DynamicWorkflowRunSubmitRefusalReason`（contracts）**逐字面同集**：service 把这里的
 * reason 原样交出去，所以两处一旦漂移就是编译错误，而不是一个悄悄变成 `undefined` 的判别键。
 * 刻意不从 contracts import：本模块是领域侧的构建器，端口词汇表反过来依赖它才是正确方向。
 */
type ImportedCacheRefusalReason =
  /** journal 里没有这个前驱 run。 */
  | "run_not_found"
  /** 前驱仍在飞（非终态）。修订**任意终态** run 都合法，含 completed。 */
  | "not_amendable"
  /** 前驱有已完结但缺消息边界的 ask：无 marker 前驱整体拒绝（无回退降级）。 */
  | "missing_boundaries";

/** 构建结果：成功带表与 lineage 指针，失败只带判别键。 */
type BuildImportedCacheResult =
  | { ok: true; cache: ImportedRunCache; resumedFrom: string }
  | { ok: false; reason: ImportedCacheRefusalReason };

/**
 * amend 预检的两个拒绝理由：与端口的 `DynamicWorkflowRunAmendRefusalReason` 逐字面同集。
 * 没有 `not_amendable`——在飞的前驱由 amend 停下，不是被拒。
 */
type AmendPreflightRefusalReason = Exclude<ImportedCacheRefusalReason, "not_amendable">;

type AmendPreflightResult =
  | { ok: true; run: RunRecord }
  | { ok: false; reason: AmendPreflightRefusalReason };

/**
 * amend 的**预检**：前驱存在 ∧ 每个已完结 ask 都有消息边界。**不看状态**——这两条都是前驱
 * journal 的性质，停止不会改变它们，所以在停止之前就能判定；预检过了再停，被拒的 amend 才
 * 不会留下一个被白白停掉的 run。在飞 run 的已完结 ask 早已连边界一起落库，所以对在飞前驱
 * 的预检与对已结算前驱的一样决定性。
 */
export function preflightAmendImport(
  journal: Pick<ImportedCacheJournalReader, "getRun" | "listNodes">,
  predecessorRunId: string,
): AmendPreflightResult {
  const run = journal.getRun(predecessorRunId);
  if (run === undefined) return { ok: false, reason: "run_not_found" };
  if (!completedAsksHaveBoundaries(journal.listNodes(predecessorRunId))) {
    return { ok: false, reason: "missing_boundaries" };
  }
  return { ok: true, run };
}

/**
 * 门 3 的谓词，预检与构建共用：无 marker 只有两种成因——marker 列引入之前写下的 journal，与 driver
 * 侧记账失败——两者都意味着「这个 run 的边界记账不可信」，所以严格到全表而不是只看会被导入
 * 的那些。未完结的 ask 没有边界是**正常**的（它们从不进导入前缀），所以只看 completed 行。
 */
function completedAsksHaveBoundaries(nodes: readonly NodeRecord[]): boolean {
  for (const node of nodes) {
    if (node.kind !== "ask" || node.status !== "completed") continue;
    if (node.messageBoundary === undefined) return false;
  }
  return true;
}

/**
 * 构建导入缓存要的三样依赖。字段名与 `DynamicWorkflowRunServiceDeps` 逐字对齐（本接口是它的
 * 结构子集），所以 run service 的两个调用点直接把 `deps` 原样递进来——多一层改名映射，就多一处
 * 会漂移的接线，而漂移的症状是「转录面明明接上了却不做诚实性检查」这类静默降级。
 */
interface AmendImportDeps {
  journal: ImportedCacheJournalReader;
  /**
   * 会话转录读面。在场时多做一道**源诚实性检查**（见 {@link honorsBoundary}）；缺席时
   * 候选照收——driver 侧的种子兑现仍会大声失败，那是 corruption 级的兜底。
   */
  actorTranscriptStore?: ActorTranscriptStore;
  logger?: Logger;
}

/**
 * 构建期能看到的**运行期旁证**，与 journal 事实相对。今天只有一条：哪些前驱会话已经写完了。
 *
 * 刻意不进 {@link AmendImportDeps}：deps 是装配（journal、转录面、logger），一个进程里从头到尾
 * 是同一份；本对象是**这一次构建**才成立的观察，两次构建可以不同。混进 deps 会让「同一份 deps
 * 必给同一张表」这句话变味。
 */
export interface AmendImportOptions {
  /**
   * 已**静默**（不再有在写的 turn）的前驱会话 id。只影响在飞 ask 的接续：完结前缀的边界是
   * journal 事实，与会话此刻长不长无关。
   *
   * **缺席 = 全部静默**，而不是「全都不静默」。两个调用点各取一半：amend 刚 abort 掉在飞前驱，
   * 被中止的 turn 可能仍在落最后几条消息，所以它带着 driver 算出的集合进来；resume 侧的重建
   * （{@link rebuildImportedCacheForResume}）面对的是一个早已结算、本进程里没有 driver 的前驱，
   * 没有任何东西在写它的会话——缺席即此。方向反过来的话，重建出的表会比提交时那张少一个
   * `inFlight`，而两侧必须是同一张表（见本文件头）。
   */
  quietSessions?: ReadonlySet<string>;
}

/**
 * 读取前驱 journal 与转录状态，构建 {@link ImportedRunCache}。
 * 已完成前缀由 journal 决定；未完成 ask 的接续还取决于源会话条数与静默状态。
 *
 * 三道门按序（先门后建：门不过时一行都不必读）：
 *   1. 前驱不存在 → `run_not_found`；
 *   2. 前驱非终态 → `not_amendable`；
 *   3. 前驱有 completed 但无 `messageBoundary` 的 ask → `missing_boundaries`。
 *
 * 门 3 之所以**严格到全表**（而不是只检查真正会被导入的那些）：无 marker 只有两种成因——
 * marker 列引入之前写下的 journal，与 driver 侧记账失败——两者都意味着「这个 run 的边界记账不可信」，
 * 而不是「这一条恰好没记上」。逐条放行等于让一个记账半坏的前驱产出一张看似完整的表，
 * 分歧时截断到一个错误的位置（模型看见的上文与 journal 记的边界悄悄错位）。
 * 缺少已完成 ask 边界的 journal 整体拒绝，不合成可能错误的转录边界。
 * 未完结的 ask 没有边界是**正常**的（它们从不进导入前缀），所以门只看 completed 行。
 */
export async function buildImportedCache(
  deps: AmendImportDeps,
  predecessorRunId: string,
  options?: AmendImportOptions,
): Promise<BuildImportedCacheResult> {
  const { actorTranscriptStore: transcripts, journal, logger } = deps;

  const run = journal.getRun(predecessorRunId);
  if (run === undefined) return { ok: false, reason: "run_not_found" };
  // 可修订集 = 任意终态，**刻意不复用** plain resume 的 isResumableRecord：那个谓词是
  // byte-identical resume 的门（stopped），而修订恰恰对它排除的两类
  // 最有用——脚本真失败（修 bug 保缓存）与 completed（温启动扩展分析）。两个集合各说各的。
  if (!TERMINAL_RUN_STATUSES.has(run.status)) return { ok: false, reason: "not_amendable" };

  const nodes = journal.listNodes(predecessorRunId);
  if (!completedAsksHaveBoundaries(nodes)) return { ok: false, reason: "missing_boundaries" };

  const actors = new Map<string, ImportedActorCandidate>();
  for (const record of namedUniqueActors(journal.listActors(predecessorRunId), logger)) {
    const name = record.name!;
    const candidate = await buildActorCandidate({
      actor: record,
      journal,
      ...(logger === undefined ? {} : { logger }),
      nodes,
      predecessorRunId,
      ...(options?.quietSessions === undefined ? {} : { quietSessions: options.quietSessions }),
      ...(transcripts === undefined ? {} : { transcripts }),
    });
    if (candidate !== undefined) actors.set(name, candidate);
  }

  return {
    ok: true,
    cache: { actors, world: buildWorldQueues(nodes) },
    resumedFrom: predecessorRunId,
  };
}

/**
 * 前驱里的**可作候选**的 actor 行：名字非空 ∧ 该名在本 run 内唯一。
 *
 * 匿名不收（名字是缓存身份键，没有名字就没有可比对的坐标）；重名**两个都不收**——
 * 引擎的 `DuplicateActorName` 是后加的运行期不变式，早于它写下的 journal 里可以真的存在
 * 重名行，而「按名取候选」在那种前驱上是掷骰子。挑一个不如都不挑：代价是这两个 actor 全新
 * 重跑，而错挑的代价是把另一个 actor 的会话前缀当成本 actor 的上文。
 */
function namedUniqueActors(records: ActorRecord[], logger?: Logger): ActorRecord[] {
  const byName = new Map<string, ActorRecord[]>();
  for (const record of records) {
    const name = record.name;
    if (name === undefined || name === "") continue;
    const bucket = byName.get(name);
    if (bucket === undefined) byName.set(name, [record]);
    else bucket.push(record);
  }
  const unique: ActorRecord[] = [];
  for (const [name, bucket] of byName) {
    if (bucket.length === 1) {
      unique.push(bucket[0]!);
      continue;
    }
    logger?.warn?.("Dynamic workflow amend: duplicate actor name in predecessor, skipped", {
      actorName: name,
      count: bucket.length,
      event: "dynamic_workflow.amend.duplicate_actor_name",
      module: "bootstrap.app",
    });
  }
  return unique;
}

/**
 * 一个候选 actor 的可导入前缀 + 转录源。任一环节缺料即回 `undefined`（该 actor 全新重跑）。
 *
 * **降级而不失败**是这里的总基调（与门的「整体拒绝」相反）：缺前缀、缺 persona、链上无会话、
 * 源会话被清理，全都只是「这个 actor 没有缓存」——journal 里没有缓存行就是没有，不撒谎。
 * 唯一会整体拒绝的是边界记账不可信，因为那会让**已收下的**候选截断到错误位置。
 */
async function buildActorCandidate(input: {
  actor: ActorRecord;
  journal: ImportedCacheJournalReader;
  logger?: Logger;
  nodes: NodeRecord[];
  predecessorRunId: string;
  quietSessions?: ReadonlySet<string>;
  transcripts?: ActorTranscriptStore;
}): Promise<ImportedActorCandidate | undefined> {
  const { actor, journal, logger, nodes, predecessorRunId, quietSessions, transcripts } = input;
  const name = actor.name!;

  // persona 是引擎在 createActor 时同步落的冻结身份，所以正常必在场；缺席只可能是被外力
  // 改写过的行。运行期比对没有比对物就无从谈起 persona 一致性——弃候选而不是拿 `{}` 顶。
  if (actor.persona === undefined) return undefined;

  const { entries, next } = completedAskPrefix(nodes, actor);
  // 既没有完结前缀、前缀后面也没有在飞的 ask：这个 actor 确实一点可导入的东西都没有。
  // 早退省掉下面的链行走与一次转录读。
  if (entries.length === 0 && next?.status !== "running") return undefined;

  const source = resolveTranscriptSource({
    actorName: name,
    journal,
    startRunId: predecessorRunId,
  });
  if (source === undefined) {
    // 链上没有任何祖先持有该 actor 的会话（从未建过，或会话已被清理）。全保真转录是本特性的
    // 裁决，没有转录就没有可接续的上文——降级为全新 actor。
    logger?.info?.("Dynamic workflow amend: no transcript source for actor, import dropped", {
      actorName: name,
      event: "dynamic_workflow.amend.transcript_source_missing",
      module: "bootstrap.app",
      runId: predecessorRunId,
    });
    return undefined;
  }

  // 空前缀的边界是 0（没有任何完结交换，接续位置只能从 0 往后算）。
  const boundary = entries.length === 0 ? 0 : entries[entries.length - 1]!.messageBoundary;
  // 转录条数**只读一次**：源诚实性检查与在飞 ask 的接续位置用的是同一个数。读两次等于给
  // 同一个事实开两个观察窗，而它们之间可以不相等。
  const messageCount =
    transcripts === undefined ? undefined : await countSource(transcripts, source.sessionId);
  if (transcripts !== undefined && (messageCount === undefined || messageCount < boundary)) {
    logger?.warn?.(
      "Dynamic workflow amend: transcript source shorter than boundary, import dropped",
      {
        actorName: name,
        event: "dynamic_workflow.amend.transcript_source_short",
        messageBoundary: boundary,
        module: "bootstrap.app",
        sessionId: source.sessionId,
      },
    );
    return undefined;
  }

  const inFlight = resolveInFlightAsk({
    actor,
    boundary,
    ...(logger === undefined ? {} : { logger }),
    ...(messageCount === undefined ? {} : { messageCount }),
    ...(next === undefined ? {} : { next }),
    predecessorRunId,
    ...(quietSessions === undefined ? {} : { quietSessions }),
    sourceSessionId: source.sessionId,
  });
  // 前缀为空且接续没谈成：这个候选一个字节都带不走，收下它只会让引擎为一张空表建会话。
  if (entries.length === 0 && inFlight === undefined) return undefined;

  return {
    persona: actor.persona,
    entries,
    ...(inFlight === undefined ? {} : { inFlight }),
    transcriptSourceSessionId: source.sessionId,
    ...(source.resolvedModel === undefined ? {} : { resolvedModel: source.resolvedModel }),
  };
}

/**
 * 前驱停下时**还在飞**的那条 ask。五个条件缺一不可：
 *
 *   1. 紧接前缀的那个位置上有一行，且它是 `running`——被取消的 ask 保留 running 行，所以停掉的
 *      run 也有；`failed` 与序号空洞都不是「还在飞」，它们只是前缀停下的另外两种理由；
 *   2. 转录源就是前驱**自己**那一行的会话：那半场未完的对话只存在于这里，从更早祖先解析出的
 *      源只承载完整前缀（chain 上每一跳都只保证前缀等价）；
 *   3. 数得出会话条数（有转录面且读得到）——没有数就没有接续位置，driver 也无从截断；
 *   4. 该会话已经**静默**，见 {@link AmendImportOptions.quietSessions}；
 *   5. 条数**严格大于**前缀边界。排队却从未派发的 ask 没有多出来的转录可带，而一个等于边界
 *      （或为 0）的 messageCount 会让 driver 播种出一段「其实就是前缀」甚至空无一物的种子，
 *      却把 actor 标记成接续过——分歧判定与转录内容随之对不上。
 *
 * 任一条不成立都只是**不接续**（完结前缀照旧导入），与本模块「降级而不失败」的总基调一致。
 */
function resolveInFlightAsk(input: {
  actor: ActorRecord;
  boundary: number;
  logger?: Logger;
  messageCount?: number;
  next?: NodeRecord;
  predecessorRunId: string;
  quietSessions?: ReadonlySet<string>;
  sourceSessionId: string;
}): ImportedInFlightAsk | undefined {
  const { actor, boundary, logger, messageCount, next, quietSessions, sourceSessionId } = input;
  if (next === undefined || next.status !== "running") return undefined;

  const drop = (reason: string): undefined => {
    logger?.info?.("Dynamic workflow amend: in-flight ask not carried", {
      actorName: actor.name,
      event: "dynamic_workflow.amend.in_flight_dropped",
      module: "bootstrap.app",
      reason,
      runId: input.predecessorRunId,
      sessionId: sourceSessionId,
    });
    return undefined;
  };

  if (actor.sessionId === undefined || actor.sessionId !== sourceSessionId) {
    return drop("transcript_source_is_ancestor");
  }
  if (messageCount === undefined) return drop("no_transcript_count");
  // 缺席 = 全部静默（见 {@link AmendImportOptions.quietSessions}）。
  if (quietSessions !== undefined && !quietSessions.has(sourceSessionId)) {
    return drop("session_not_quiescent");
  }
  if (messageCount <= boundary) return drop("no_transcript_beyond_prefix");

  return { inputHash: next.inputHash, messageBoundary: messageCount };
}

/**
 * 该 actor 的**最长全 completed ask 前缀**（按 actorSeq 0..k 连续），外加**紧接其后**那一行。
 *
 * 前缀在第一个非 completed 处停死，三种停法同一处理：失败、崩溃中（running）、序号空洞。
 * 失败的 ask 对新 run **无约束力**（模型有随机性，修订常常就是为了越过一次失败），所以它自己
 * 不导入；但跳过它去导入其后的条目会走私上下文——被跳过那一轮的问答仍在源会话转录里，
 * 而缓存却声称它没发生过。停在第一个非 completed 处是唯一自洽的读法。
 *
 * `next` 就是**让前缀停下的**那一行（空洞时缺席）。它与前缀同来同走，因为「在飞的那条 ask」
 * 按定义正是这一行：另起一次遍历去找 `actorSeq === entries.length` 的行，等于把「紧接前缀」
 * 这个坐标在两处各算一次。
 */
function completedAskPrefix(
  nodes: NodeRecord[],
  actor: ActorRecord,
): { entries: ImportedAskEntry[]; next?: NodeRecord } {
  const bySeq = new Map<number, NodeRecord>();
  for (const node of nodes) {
    if (node.kind !== "ask") continue;
    if (node.actorSiteId !== actor.siteId || node.actorOrdinal !== actor.ordinal) continue;
    if (node.actorSeq === undefined) continue;
    bySeq.set(node.actorSeq, node);
  }

  const entries: ImportedAskEntry[] = [];
  for (let seq = 0; ; seq++) {
    const node = bySeq.get(seq);
    if (node === undefined || node.status !== "completed") {
      return { entries, ...(node === undefined ? {} : { next: node }) };
    }
    // 边界必在场：门 3 已对整个前驱把关，所以这里不是乐观读而是不变式的兑现。
    const entry: ImportedAskEntry = {
      inputHash: node.inputHash,
      result: node.result,
      messageBoundary: node.messageBoundary!,
    };
    if (node.stats !== undefined) entry.stats = node.stats;
    entries.push(entry);
  }
}

/**
 * 沿 `resumed_from` 链回溯**最近一个**持有该名 actor 会话的祖先 run。
 *
 * 为什么需要走链：run B 里某 actor 全程命中缓存 ⇒ B 从未给它建过会话（惰性创建），于是
 * B→C 的修订要接续该 actor 时，转录只存在于 A。count 边界跨前缀复制不变，所以在链上任何
 * 持会话祖先处，B 抄来的边界值都直接可用——这正是链式修订成立的根基。
 *
 * `resolvedModel` 与会话取自**同一行**：pin 的意义是「接续这段转录时别换模型」，取自别的行
 * 就是在为一段不属于它的转录做承诺。
 *
 * 环防御（seen）是纯防御：supersede 只能指向已终结的更早 run，构造不出环。但这个 while 若真
 * 遇到损坏数据就是死循环，而防御的代价是一个 Set。
 */
function resolveTranscriptSource(input: {
  actorName: string;
  journal: ImportedCacheJournalReader;
  startRunId: string;
}): { sessionId: string; resolvedModel?: string } | undefined {
  const { actorName, journal, startRunId } = input;
  const seen = new Set<string>();
  let runId: string | undefined = startRunId;

  while (runId !== undefined && !seen.has(runId)) {
    seen.add(runId);
    const matches = journal.listActors(runId).filter((actor) => actor.name === actorName);
    // 0 = 这一代根本没有这个 actor（链对该名字断了）；>1 = 重名，按名取会话是掷骰子。
    // 两种都停在这里而不是继续上溯：上一代的会话不是**这段**转录的源。
    if (matches.length !== 1) return undefined;
    const actor = matches[0]!;
    if (actor.sessionId !== undefined) {
      return {
        sessionId: actor.sessionId,
        ...(actor.resolvedModel === undefined ? {} : { resolvedModel: actor.resolvedModel }),
      };
    }
    runId = journal.getRun(runId)?.resumedFrom;
  }
  return undefined;
}

/**
 * 源会话此刻的消息条数；读失败回 `undefined`。
 *
 * 两个读者共用这一次读取（见 {@link buildActorCandidate}）：
 * - 检查源会话是否达到已完成前缀的边界。条数不足或无法读取时，构建器丢弃该候选，
 *   让后继重新执行；driver 复制转录时仍会拒绝短于边界的源，防止写入不完整的上下文。
 * - 计算未完成 ask 的接续位置（见 {@link resolveInFlightAsk}）。
 *
 * 条数口径必须与 driver 记账及 core 历史恢复一致，均使用同一消息存储接口。
 */
async function countSource(
  transcripts: ActorTranscriptStore,
  sessionId: string,
): Promise<number | undefined> {
  try {
    return (await transcripts.messages({ sessionID: sessionId as SessionId })).length;
  } catch {
    return undefined;
  }
}

/**
 * world 节点的内容表：`inputHash` → 按 journal 插入序排好的记录队列（第 n 次出现对第 n 条）。
 *
 * 键直接用**前驱记录的 inputHash**，不重算：引擎对 `{op,args}` 的哈希口径（engine.ts 的
 * worldRead）就是写进这一列的那个值，重算一遍等于在这里复制一份哈希契约，而它一旦漂移，
 * 表面上是「缓存莫名不命中」。
 *
 * 只收 completed：失败的世界读取重新执行（失败对新 run 无约束力），running 的更不必说。
 * world-run 与 world-read 同表——导入 world-run 是**安全特性**而不是优化：修订续跑绝不静默
 * 重放一次已 journal 的效应（部署脚本跑两次）。
 */
function buildWorldQueues(nodes: NodeRecord[]): ReadonlyMap<string, ImportedWorldEntry[]> {
  const world = new Map<string, ImportedWorldEntry[]>();
  for (const node of nodes) {
    if (node.kind !== "world-read" && node.kind !== "world-run") continue;
    if (node.status !== "completed") continue;
    const queue = world.get(node.inputHash);
    const entry: ImportedWorldEntry = {
      inputHash: node.inputHash,
      kind: node.kind,
      result: node.result,
    };
    if (queue === undefined) world.set(node.inputHash, [entry]);
    else queue.push(entry);
  }
  return world;
}

/**
 * resume 侧的入口：重建修订 run 的导入缓存。**任何失败都只降级、不拒绝 resume**。
 *
 * 与提交侧共用同一个 {@link buildImportedCache}——这不是复用的顺手，而是正确性前提：修订 run 的
 * journal 只对「已到达的执行前缀」自含，未消费的导入靠这次重建补回，两侧算出不同的表就意味着
 * 「重建」变成了「另建一张」。
 *
 * 三条理由让「重建失败」与「提交时构建失败」判然不同：
 *   - 修订 run 已经存在了。拒绝 resume 等于把一个可续跑的 run 变成永久卡死的 run；
 *   - 已消费的命中在本 run 的 journal 里是**真行**，replay 不需要这张表——run 的自含性不依赖它；
 *   - 未消费的导入退化成 live 重执行，结果正确，只是花掉本可省下的 token。
 *
 * 所以这里连门的三个理由都不区分：对 resume 而言 `run_not_found`（前驱被清理）与
 * `missing_boundaries` 是同一件事——「这次没有缓存可用」。前驱 journal 因此是修订 run 的**存续
 * 依赖，但只是加速结构**：丢了变贵，不变错。记一条 info 便于事后解释账单。
 *
 * **不带 {@link AmendImportOptions.quietSessions}**：走到这里的前驱早已终态，本进程里没有它的
 * driver，没有任何东西在写它的会话。带一个空集合进来会让重建出的表比提交时那张少一个
 * `inFlight`，而两侧必须是同一张表（见本文件头）。
 */
export async function rebuildImportedCacheForResume(
  deps: AmendImportDeps,
  /** 被 resume 的修订 run 与它的 `resumed_from`（调用方已确认后者在场）。 */
  run: { runId: string; predecessorRunId: string },
): Promise<ImportedRunCache | undefined> {
  const { predecessorRunId, runId } = run;
  const built = await buildImportedCache(deps, predecessorRunId);
  if (built.ok) return built.cache;
  deps.logger?.info?.("Dynamic workflow amend cache rebuild skipped; resuming without it", {
    event: "dynamic_workflow.amend.rebuild_skipped",
    module: "bootstrap.app",
    reason: built.reason,
    resumedFrom: predecessorRunId,
    runId,
  });
  return undefined;
}
