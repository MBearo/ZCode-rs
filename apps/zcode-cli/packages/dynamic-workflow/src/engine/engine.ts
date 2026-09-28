/**
 * 执行引擎的确定性状态机（Boundary A 的实现 + 向下驱动 Boundary B + 向上回报 sink）。
 *
 * 核心不做任何 I/O、不读时钟、不用随机数：随时间/调度变化的决策要么被 journal 记录、要么被
 * "每站点序号 + 每 actor FIFO 的 actorSeq"这套确定性规则固定，从而首次执行与 replay 逐字一致。
 *
 * 关键不变量：
 * - 实例身份 = 站点 id × 每站点执行序号（第 n 次 ask#3 = ask#3@n）。actor 身份 = 创建站点 × 序号。
 * - journal 决策：每次 host 调用先查 (siteId, ordinal)；命中即短路（无 driver 调用），并防御性
 *   校验 inputHash——不一致则整个 run 大声失败（结构化错误，绝不静默偏移）。
 * - 每 actor FIFO + actorSeq 准入顺序 + replay 的 hold 规则：见 scheduler.ts（本文件把 ask 生命周期委托给它）。
 *
 * 本文件聚焦 run 生命周期：host API 入口、用量记账、run 结算与事件记录。用户面产物、report、
 * world 节点与导入缓存、run 终态三条路径的方法体各在兄弟模块（engine-artifacts.ts / engine-report.ts /
 * engine-world.ts / engine-settlement.ts），经 engine-state.ts 的 {@link EngineState} 接缝读写这里的
 * 私有状态；本类上只留薄委托（拆分原因：oxlint max-lines 上限 400 行）。出生阶段坐标的两个
 * 打戳函数（事件与 ProviderStop）是纯函数，住在 engine-phase-stamp.ts，只读本类拥有的
 * `instancePhases` 表。
 */

import { ImportedWorldQueue, matchImportedActor } from "./imported-cache.js";
import type { ArtifactContentOp, ArtifactPresetOp } from "../facade/registry.js";
import { AskScheduler, type SchedulerHost } from "./scheduler.js";
import type { ArtifactIdState, EngineState, RunSettlement } from "./engine-state.js";
import {
  declarePresetArtifact,
  publishContentArtifact,
  rememberArtifactRow,
} from "./engine-artifacts.js";
import { publishReport } from "./engine-report.js";
import { closeImportCache, readWorld, recoverImportClosure } from "./engine-world.js";
import { recoverSettleOrder, ReplaySettleOrder } from "./replay-order.js";
import { settleCompleted, settleFailed, settleStopped } from "./engine-settlement.js";
import type {
  ActorId,
  ArtifactRef,
  AskProgress,
  AskSpec,
  AskStats,
  AskWaitInfo,
  Caps,
  ConcurrencyChange,
  ImportedRunCache,
  InstanceRef,
  JournalStorePort,
  PersonaSpec,
  RunEvent,
  RunStallInfo,
  RunStatus,
  RunStopReason,
  ValidateFn,
  WorkflowDriver,
  WorkflowHostApi,
  WorkflowReportSink,
  WorldReadOp,
} from "./types.js";
import { refToString, WorkflowError } from "./types.js";
import { runLaunchedEvent, type RunLaunchConfig } from "./engine-launch.js";
import { enrichProviderStopPhase, stampBirthPhase } from "./engine-phase-stamp.js";

/** 引擎构造配置。 */
export interface EngineConfig {
  runId: string;
  driver: WorkflowDriver;
  caps: Caps;
  /**
   * 每 ask 站点的静态规格（typed + schema）。**必须覆盖脚本里的每一个 ask 站点**——
   * 站点表与 schema 合成来自同一次编译，因此缺席只可能是接线错误，引擎按硬错误处理
   * （MissingAskSpec）。untyped 站点要显式记为 `{ typed: false }`。
   */
  askSpecs: ReadonlyMap<string, AskSpec>;
  /** 注入的 schema 校验器（核心不 import schema 实现）。 */
  validate: ValidateFn;
  /** run 元数据（落 dwf_run，仅在本次 createRun 时写入；resume 时不覆写记录）。 */
  scriptText?: string;
  /**
   * run 的展示名（`CreateWorkflow` 的可选 `input.name`）。引擎不读它，只在建 run 时随
   * `scriptText` 一起落库——宿主的枚举面据它给 run 起标签。见 {@link RunRecord.name}。
   */
  name?: string;
  /**
   * 脚本文本的哈希。resume 时与 journal 记录里的值比对：两侧都有且不同即拒绝本次 resume
   * （V1 的 resume 只对逐字节相同的脚本有效）。
   */
  scriptHash?: string;
  /**
   * 本次 run 的实参（已校验回填）。引擎不读它，只在建 run 时随 `scriptText` 一起落库；
   * 沙箱侧的注入走 harness 的 spawn payload，不经引擎。见 {@link RunRecord.args}。
   */
  args?: Record<string, unknown>;
  parentSessionId?: string;
  cwd?: string;
  /**
   * 发起 run 的 CreateWorkflow 工具调用 id（见 {@link RunRecord.toolCallId}）。
   * 引擎不读它，只随其余元数据在 createRun 时落库。
   */
  toolCallId?: string;
  /**
   * 本次 run 修订自哪个前驱 run（见 {@link RunRecord.resumedFrom}）。引擎不读它，
   * 只随其余元数据在 createRun 时落库——导入缓存的构建在 run service，不在核心。
   */
  resumedFrom?: string;
  /**
   * 发起 run 那一轮的锚点。引擎不读它，只在建 run 那一世
   * 紧跟首条 `run-started` 记一条 `run-launched`；resume 命中既有行时不再记（锚点跨生命周期唯一）。
   * `phaseNames` 随锚点同车：脚本声明的阶段表，引擎同样不读，只落 journal。`phaseAlongside`
   * 与它按位置对齐（下标指向同一张表），同车同规。
   * `subagentModel` 也同车：本 run 子代理的选型（规范 picker 串），引擎同样不读——模型面整个
   * 在宿主侧（bootstrap 的 workflow-actor-model.ts），宿主从这条事件读回它，零 SQL。
   * `scriptPath` 同车同规：本 run 的脚本来自哪个文件（绝对路径），引擎不读，宿主从这条事件
   * 读回它交给模型面。
   */
  launch?: RunLaunchConfig;
  /**
   * 建 run 时的用量起点：前驱 run 的 `spentTokens`。amend 路径给出，全新 submit 缺席（= 从零起账），
   * resume 路径给了也无用——命中既有行时用量从行里恢复。
   *
   * 语义是「本 run 报的是整条 lineage 的花费」：每个前驱的数字本身已是累计值，所以链式修订
   * 按构造求和，没人需要走 `resumed_from` 链。命中缓存不再加钱（那笔账就在这个继承值里），
   * 只有本次现跑的 live turn 往上加。
   */
  inheritedTokens?: number;
  /**
   * amend-resume 的导入缓存（{@link ImportedRunCache}）。**纯数据注入**——核心因此仍是
   * 零 I/O 的确定性状态机：读前驱 journal、走 `resumed_from` 链、解析转录源，全部发生在
   * run service，引擎只拿到一张构建好的表并按运行期身份（actor 名 + persona、
   * `{op,args}` 内容 + 出现序）比对。缺席即本次不是修订续跑。
   */
  importedCache?: ImportedRunCache;
}

/** run 的最终结算（定义随结算模块的接缝迁到 engine-state.ts，这里原地再导出）。 */
export type { RunSettlement } from "./engine-state.js";

interface SettledDeferred {
  promise: Promise<RunSettlement>;
  resolve: (value: RunSettlement) => void;
}

export class WorkflowEngine implements WorkflowHostApi, WorkflowReportSink {
  private readonly runId: string;
  private readonly driver: WorkflowDriver;
  private readonly journal: JournalStorePort;
  /**
   * 本 run 的并发上界。**可变**（见 {@link setMaxConcurrency}）：一次只改 `max_concurrency`
   * 的修订就地作用在活着的 run 上。整份替换而不是原地改字段——已记进事件与 journal 的那几份
   * caps 必须保持它们被记下时的样子（调用方递进来的那个对象同理不被回写）。
   */
  private caps: Caps;
  private readonly askSpecs: ReadonlyMap<string, AskSpec>;
  private readonly validate: ValidateFn;
  private readonly scheduler: AskScheduler;

  /** 每站点执行序号计数器（ask / world-read / actor 共用，站点 id 互不相同）。 */
  private readonly ordinals = new Map<string, number>();
  /**
   * 阶段进入次数，按名字。**不与**
   * `ordinals` 共表：阶段名是作者的任意字符串，一个恰好叫 `report#1` 的阶段不能挪动那个
   * 站点的节点序号（site-id stability），反之亦然。
   */
  private readonly phaseOrdinals = new Map<string, number>();
  /**
   * 控制流当前所在的阶段名（`enterPhase` 维护；第一个标记之前为 undefined）。只在铸造点被
   * 读取——见 {@link nextOrdinal}。
   */
  private currentPhase: string | undefined;
  /**
   * 实例（`siteId@ordinal`）→ 它**出生时**的阶段名。写在铸造点、读在 {@link record} 的打戳漏斗：静态因果图按阶段拷贝
   * 站点，运行时实例必须带同一个坐标，否则一条车道上的实例会被那个站点的每一份阶段拷贝
   * 同时认领（本 bug 的形状：五个阶段各 20 个子代理，五张卡各显示 100 个）。
   */
  private readonly instancePhases = new Map<string, string>();

  /**
   * 本 run 内已被认领的**非空有效 actor 名** → 认领它的 actor（用于失败信息里指认前一处）。
   * 见 {@link createActor} 的查重与 replay 安全性说明。
   */
  private readonly actorNames = new Map<string, string>();

  /** amend-resume 的导入缓存（纯数据，缺席即本次不是修订续跑）。 */
  private readonly importedCache?: ImportedRunCache;
  /** world 导入队列的消费游标（第 n 次出现对第 n 条）。 */
  private readonly importedWorld: ImportedWorldQueue;
  /**
   * 导入缓存是否已**关闭**：第一笔写入之前置真、永不重开
   * ——某个 live 子代理即将执行改写工具（driver 上报 askMutating），或一条 world.run live 执行。
   * 关闭后 world 节点与带工具的 ask 不再问导入表（哈希只比对文本，比不出工作区已被改写）；
   * 纯 ask（toolCalls 0）照常命中。resume 时从 `import-cache-closed` 事件恢复，见 {@link recoverImportClosure}。
   */
  private importClosed = false;
  /**
   * replay 的结算次序闸：命中缓存的
   * 结算按 journal 记下的**首生结算次序**释放，而不是按准入次序——join 之后的每一个站点序号
   * 都由那个次序决定。非 resume 恒为空闸（人人放行）。
   */
  private replaySettleOrder = ReplaySettleOrder.empty();
  /** resume 时从事件恢复的「崩溃前曾 live 的 ask 实例」（`siteId@ordinal`）；非 resume 为空。 */
  private liveAskInstances: ReadonlySet<string> = new Set();
  /** resume 时从事件次序恢复的「准入早于关门的 ask 实例」；续跑在飞 ask 的判定据此复原。 */
  private queuedBeforeImportClose: ReadonlySet<string> = new Set();

  /**
   * 本 run 已发布的报告条数（REPORT_CAPS.maxItemsPerRun 的计数器）。resume 时按 journal 里
   * kind:"report" 的行数恢复——上限是 run 级的，跨 resume 必须连续计数，否则一个反复
   * resume 的 run 可以无限报告。
   */
  private reportCount = 0;
  /**
   * 本 run 每个**用户面产物** id 的状态。与 `reportCount`
   * 同一条恢复法：resume 时从 journal 的 `kind: "artifact"` 行重建，此后在内存里维护——
   * 上限（32 个 id / 每 id 16 版）与版本号都是 run 级的事实，跨 resume 必须连续，否则一个
   * 反复 resume 的 run 可以无限发布。
   *
   * ⚠ 术语：artifact = 用户面产物，不是 `RunSettlement.artifact`（顶层返回值）。
   */
  private readonly artifacts = new Map<string, ArtifactIdState>();
  /** 累计 token 用量（观察面：只记账、只广播，永远不会让 run 失败）。 */
  private spentTokens = 0;
  private runSettled = false;
  private runFailure?: WorkflowError;

  private readonly settledDeferred: SettledDeferred;
  /** 兄弟模块自由函数读写私有状态的接缝（构造函数里用箭头闭包装配，见 engine-state.ts）。 */
  private readonly state: EngineState;

  constructor(config: EngineConfig) {
    this.runId = config.runId;
    this.driver = config.driver;
    this.journal = config.driver.journal;
    // 自己留一份：调用方（harness / run service）手里的那个对象不该因为一次 retune 而被改写。
    this.caps = { maxConcurrency: config.caps.maxConcurrency };
    this.askSpecs = config.askSpecs;
    this.validate = config.validate;
    this.importedCache = config.importedCache;
    this.importedWorld = new ImportedWorldQueue(config.importedCache?.world ?? new Map());

    let resolve!: (value: RunSettlement) => void;
    const promise = new Promise<RunSettlement>((res) => {
      resolve = res;
    });
    this.settledDeferred = { promise, resolve };

    this.state = {
      runId: this.runId,
      driver: this.driver,
      journal: this.journal,
      artifacts: this.artifacts,
      importedCache: this.importedCache,
      importedWorld: this.importedWorld,
      isRunSettled: () => this.runSettled,
      runError: () => this.runError(),
      failRun: (error) => this.failRun(error),
      record: (event) => this.record(event),
      nextOrdinal: (siteId) => this.nextOrdinal(siteId),
      holdForReplay: (instance, release) => {
        this.replaySettleOrder.hold(instance, release);
      },
      reportCount: () => this.reportCount,
      countReport: () => {
        this.reportCount++;
      },
      importClosed: () => this.importClosed,
      closeImport: () => {
        this.importClosed = true;
      },
      markSettled: (failure) => {
        this.runSettled = true;
        if (failure !== undefined) this.runFailure = failure;
        // 闸门随结算永久打开：还挂在次序表上的释放动作一律放掉，否则脚本那侧的 promise
        // 永不兑现（沙箱会被关掉，但同进程跑脚本的装配就此挂死）。
        this.replaySettleOrder.open();
      },
      abortInFlight: (error, emitCancelled) => this.scheduler.abortInFlight(error, emitCancelled),
      resolveSettled: (settlement) => this.settledDeferred.resolve(settlement),
    };

    // caps 对调度器是**现读**：setMaxConcurrency 整份换掉 this.caps，而派发判据必须看见新值
    // （见 SchedulerHost.caps）。箭头闭包而不是对象字面量里的 `this`——后者指的是字面量自己。
    const readCaps = (): Caps => this.caps;
    const host: SchedulerHost = {
      runId: this.runId,
      get caps(): Caps {
        return readCaps();
      },
      driver: this.driver,
      validate: this.validate,
      nextOrdinal: (siteId) => this.nextOrdinal(siteId),
      holdForReplay: (instance, release) => {
        this.replaySettleOrder.hold(instance, release);
      },
      record: (event) => this.record(event),
      isRunSettled: () => this.runSettled,
      runError: () => this.runError(),
      failRun: (error) => this.failRun(error),
      importCacheClosed: () => this.importClosed,
      wasLiveBeforeResume: (instance) => this.liveAskInstances.has(refToString(instance)),
      wasQueuedBeforeImportClose: (instance) =>
        this.queuedBeforeImportClose.has(refToString(instance)),
    };
    this.scheduler = new AskScheduler(host);

    const existing = this.journal.getRun(this.runId);
    if (existing === undefined) {
      // 修订 run 从前驱的累计值起账；
      // 全新 run 缺席即 0。归一（非有限值与负数按缺席、小数下取整）就在这一处：这个数要落
      // `spent_tokens` 列，协议 schema 要求非负整数，而它是过了 harness 边界的纯数据——一个 NaN
      // 会同时毒化列值与投影补丁，所以脏值必须死在写库之前，而不是死在投影的补丁校验里。
      const inherited = config.inheritedTokens ?? 0;
      this.spentTokens = Number.isFinite(inherited) ? Math.max(0, Math.floor(inherited)) : 0;
      this.journal.createRun({
        runId: this.runId,
        caps: this.caps,
        spentTokens: this.spentTokens,
        status: "running",
        // 元数据只在创建 run 时落库；保存传入的脚本与会话字段，供恢复和 script_hash 校验使用。
        // name 是后来加入同一条路的展示元数据。缺省的字段保持缺席（不落成 undefined 键）。
        ...(config.scriptText === undefined ? {} : { scriptText: config.scriptText }),
        // 实参与 scriptText 同一条元数据路，且同样只在建 run 时写一次：resume 从这里读回
        // 重放，绝不接受调用方给的新实参。
        ...(config.args === undefined ? {} : { args: config.args }),
        ...(config.name === undefined ? {} : { name: config.name }),
        ...(config.scriptHash === undefined ? {} : { scriptHash: config.scriptHash }),
        ...(config.parentSessionId === undefined
          ? {}
          : { parentSessionId: config.parentSessionId }),
        ...(config.cwd === undefined ? {} : { cwd: config.cwd }),
        ...(config.toolCallId === undefined ? {} : { toolCallId: config.toolCallId }),
        // lineage 也只在建 run 时写一次：修订是 supersede（新 run），前驱行零触碰。
        ...(config.resumedFrom === undefined ? {} : { resumedFrom: config.resumedFrom }),
      });
    } else {
      // resume 的前置条件：脚本必须与建 run 时逐字节相同。哈希
      // 不一致说明调用方拿另一份脚本复用了同一个 runId——站点 id 是 journal 的键，继续下去
      // 会把新脚本的站点对上旧脚本的结果。拒绝**这次 resume**（同步抛出），而不是先接受
      // 再 failRun：后者会把 failed 盖到一份仍能用正确脚本 resume 的记录上。
      if (
        existing.scriptHash !== undefined &&
        config.scriptHash !== undefined &&
        existing.scriptHash !== config.scriptHash
      ) {
        throw new WorkflowError(
          "ScriptHashMismatch",
          `Cannot resume run ${this.runId}: its script changed (recorded hash ${existing.scriptHash}, ` +
            `got ${config.scriptHash}). Resume needs the byte-identical script; to run a revised ` +
            `script, start a new run with resume_from instead.`,
          { mismatch: { expected: existing.scriptHash, got: config.scriptHash } },
        );
      }
      // resume：结算次序按本 run 自己的事件恢复（本闸是 replay 正确性的一部分，不是观察面：
      // 站点序号依调用到达顺序，而扇出的到达顺序只有首生的结算次序能复现）。节点行只读一次，
      // 下面的报告计数与产物恢复共用它。
      const nodes = this.journal.listNodes(this.runId);
      this.replaySettleOrder = recoverSettleOrder(this.journal, this.runId, nodes);
      // 报告计数按 journal 里 kind:"report" 的行数恢复；用量从记录恢复（跨生命周期连续）。
      this.reportCount = nodes.filter((n) => n.kind === "report").length;
      // 产物状态与 reportCount 同席恢复：id 归属（种类、预置 spec）与已成功版本数全部由
      // journal 行派生，所以崩溃恢复后第 3 版仍然是第 3 版，而不是从 1 重新数起。
      for (const node of nodes) {
        if (node.kind !== "artifact" || node.status !== "completed") continue;
        rememberArtifactRow(this.state, node.artifactId, node.result);
      }
      this.spentTokens = existing.spentTokens;
      // 修订 run 的崩溃恢复：导入表被整表重建，而「门是否已关」不落库——从事件精确恢复。
      if (this.importedCache !== undefined) {
        const recovered = recoverImportClosure(this.journal, this.runId);
        this.liveAskInstances = recovered.live;
        this.queuedBeforeImportClose = recovered.queuedBeforeClose;
        this.importClosed = recovered.closed;
      }
      this.journal.updateRunStatus(this.runId, "running");
    }
    this.record({ type: "run-started", runId: this.runId, caps: this.caps });
    // 建 run 那一世才记锚点，且放在 run-started 之后：投影把 run-started 当作建条目事件，
    // 锚点事件对它只是抬水位（reducer 的 default 分支）。
    if (existing === undefined && config.launch !== undefined) {
      this.record(runLaunchedEvent(config.launch, config));
    }
    // 继承（amend）或恢复（resume）来的用量在这里补发一条：投影把 `run-started` 当作
    // 「用量清零」（workflow-runs-started.ts 剥掉上一世的结算残影），所以不补发的话，卡片会在
    // 第一轮 live turn 落地之前显示 0——而「这条 run 还没花钱」是假的。一世没有任何 live ask 时
    // 更糟：投影会一直停在 0，直到冷回放才对得上行值。
    // 零不发：reset 本身已经说了零，再发一条是每个全新 run 都要付的一条噪音事件。
    if (this.spentTokens > 0) this.record({ type: "usage-updated", spentTokens: this.spentTokens });
  }

  /** run 结算 promise：完成/失败/取消时兑现。 */
  get settled(): Promise<RunSettlement> {
    return this.settledDeferred.promise;
  }

  /** 当前 run 状态。 */
  status(): RunStatus {
    return this.journal.getRun(this.runId)?.status ?? "running";
  }

  // ——————————————————————————————— Boundary A ———————————————————————————————

  /**
   * 建一个 actor。**有效名**是 `normalizePersona` 之后的 `spec.name`（persona.name 压过 name
   * 实参），非空即必须在本 run 内唯一——重名让整个 run 大声失败（DuplicateActorName）。
   *
   * 为什么是 run 级失败而不是改名/警告：具名 actor 是 amend-resume 缓存导入的身份键，而任何
   * run 都是未来修订的潜在前驱，前驱里重名会让导入匹配歧义。
   * 规则因此对所有 run 生效，不只是修订 run。匿名（缺席或空串）从不查、任意多个合法——
   * 代价已裁决：没有缓存资格。
   *
   * **replay 安全**：查重表是纯内存的、随引擎实例而生。逐字节相同脚本的 resume 在一个**全新**
   * 引擎实例里从头重跑同一串 createActor 调用，每个名字因此恰好被登记一次，不会把上一轮
   * 自己的登记当成重名。这也是它不能落 journal 的理由——落库的表会在 resume 时与自己撞车。
   *
   * **导入缓存的附着也在这里**（amend-resume）：按有效名查 `importedCache.actors`，
   * persona 规范化后一致才把候选挂到调度器的 actor 态上。比对之所以在**运行期**而不是
   * 提交时静态比对两份脚本：名字与 persona 都是运行期值（`agent()` 的实参可以是动态表达式），
   * 静态比对是第二份真相，恰是本包处处要防的（判定见 imported-cache.ts 的 `matchImportedActor`）。
   */
  createActor(siteId: string, name?: string, persona?: string | PersonaSpec): ActorId {
    this.assertRunning("createActor");
    const ordinal = this.nextOrdinal(siteId);
    const ref = { siteId, ordinal };
    const id = refToString(ref);
    const spec = normalizePersona(name, persona);
    const effectiveName = spec.name;
    if (effectiveName !== undefined && effectiveName !== "") {
      const claimed = this.actorNames.get(effectiveName);
      if (claimed !== undefined) {
        // assertRunning 同姿态的同步表面：createActor 无 promise 可拒，只能抛。
        const err = new WorkflowError(
          "DuplicateActorName",
          `Subagent name "${effectiveName}" is used twice in this run (${claimed} and ${id}). ` +
            `A named subagent is the identity an amended re-run matches its cache by, so names must ` +
            `be unique; give this one its own name or drop the name to make it anonymous.`,
        );
        this.failRun(err);
        throw err;
      }
      this.actorNames.set(effectiveName, id);
    }
    this.scheduler.registerActor(ref, id, name, spec, matchImportedActor(this.importedCache, spec));
    // putActor 是整条记录的替换，而 sessionId / resolvedModel 是 **driver 拥有**的字段
    // （前者由 ensureSession 写、后者由宿主侧的 runtime 工厂写）。这里必须把既有值原样带过去，
    // 否则 replay 命中同一 (siteId, ordinal) 时会把两者抹成空。
    const existing = this.journal.getActor(this.runId, siteId, ordinal);
    this.journal.putActor({
      runId: this.runId,
      siteId,
      ordinal,
      name,
      persona: spec,
      sessionId: existing?.sessionId,
      resolvedModel: existing?.resolvedModel,
    });
    this.record({ type: "actor-created", actor: ref, name, persona: spec });
    return id;
  }

  ask(siteId: string, actorId: ActorId, instructions: string): Promise<unknown> {
    if (this.runSettled) return Promise.reject(this.runError());
    if (!this.scheduler.hasActor(actorId)) {
      // 未知 actor：脚本/lowering 契约被破坏，整个 run 大声失败。
      const err = new WorkflowError("UnknownActor", `Unknown subagent handle: ${actorId}.`);
      this.failRun(err);
      return Promise.reject(err);
    }
    const spec = this.askSpecs.get(siteId);
    if (spec === undefined) {
      // 站点表与 schema 合成来自同一次编译，所以 miss 只可能是接线错误（两份编译产物被
      // 拼在一起）。`?? { typed: false }` 兜底会把 typed ask 静默降级成 untyped：
      // 不注册 submit_result、拿末轮文本当结果、schema 校验整个消失——一个降级到
      // 「结果永远合法」的类型系统比没有类型系统更糟，所以宁可让整个 run 大声失败。
      const err = new WorkflowError(
        "MissingAskSpec",
        `Ask site ${siteId} has no spec: askSpecs is incomplete (compile output mismatch).`,
      );
      this.failRun(err);
      return Promise.reject(err);
    }
    return this.scheduler.admitAsk(siteId, actorId, instructions, spec);
  }

  /** world 节点（world-read / world-run）：方法体在 engine-world.ts 的 readWorld。 */
  worldRead(siteId: string, op: WorldReadOp, args: unknown[]): Promise<unknown> {
    return readWorld(this.state, siteId, op, args);
  }

  log(message: string): void {
    if (this.runSettled) return;
    this.record({ type: "log", message });
  }

  /**
   * 控制流经过了一个 `phase("…")` 标记（Boundary A 的 `enterPhase`）。与 `log` 同一副姿态：同步、无返回值、
   * 结算后 no-op、**不落 journal 行**——标记不是站点，没有 `dwf_node` 可写。
   *
   * ordinal 按名字计数，用自己的表（{@link phaseOrdinals}）：阶段名与 site id 互不消耗对方的
   * 序号（site-id stability）。
   * **每次求值都发**：`for { phase("B"); ask }` 的第二轮就是 B 的第二次进入；若「与当前阶段
   * 同名即不发」，单阶段循环体的轮次就丢了。resume 重跑会把前缀再发一遍——这里刻意不去重
   * （去重需要引擎读事件表），reducer 以 `rounds = max(rounds, ordinal)` 单调归约。
   */
  enterPhase(name: string): void {
    if (this.runSettled) return;
    const trimmed = name.trim();
    if (trimmed.length === 0) return;
    const ordinal = (this.phaseOrdinals.get(trimmed) ?? 0) + 1;
    this.phaseOrdinals.set(trimmed, ordinal);
    // 此后铸造的实例都出生在这个阶段。
    this.currentPhase = trimmed;
    this.record({ type: "phase-entered", name: trimmed, ordinal });
  }

  /** 发布一条中间结果（Boundary A 的 `report`）：方法体在 engine-report.ts 的 publishReport。 */
  report(siteId: string, item: unknown, artifactId?: string): void {
    publishReport(this.state, siteId, item, artifactId);
  }

  /**
   * 发布一个**内容产物**（`artifact.file` / `artifact.markdown`）：方法体在 engine-artifacts.ts
   * 的 publishContentArtifact。
   */
  publishArtifact(siteId: string, op: ArtifactContentOp, args: unknown[]): Promise<ArtifactRef> {
    return publishContentArtifact(this.state, siteId, op, args);
  }

  /**
   * 声明一个**预置产物**（`artifact.chart` / `table` / `metrics` / `board`）：方法体在
   * engine-artifacts.ts 的 declarePresetArtifact。
   */
  declareArtifact(siteId: string, op: ArtifactPresetOp, args: unknown[]): void {
    declarePresetArtifact(this.state, siteId, op, args);
  }

  // —————————————————————— 外部驱动的 run 生命周期 ——————————————————————

  /** 沙箱脚本成功返回顶层 artifact，结算为 completed：方法体在 engine-settlement.ts 的 settleCompleted。 */
  complete(artifact: unknown): void {
    settleCompleted(this.state, artifact);
  }

  /**
   * 外部停止：中止在飞 ask（deferred 以 Cancelled reject、补发 node-settled(cancelled)），run
   * 结算 `stopped(reason)`；已完结的 journal 条目保留（可 resume）。四个 reason 走同一条路：
   * `user` / `model`（cancel 入口传进来的 initiator）、`interrupted`（harness 的沙箱故障）、
   * `provider`（driver 经 `stopRun` 报上来的确定性模型侧错误）。`error` 只对后两者在场。
   * first-wins：已结算即忽略。方法体在 engine-settlement.ts 的 settleStopped。
   */
  stop(reason: RunStopReason, error?: WorkflowError, supersededBy?: string): void {
    settleStopped(this.state, reason, error, supersededBy);
  }

  /**
   * 外部失败：complete 的失败对偶。harness 用它把子进程 `{complete, ok:false, error}`（脚本抛出）、
   * 子进程崩溃、墙钟超时、NDJSON 解析失败等都归为 run 失败，
   * 而非伪装成 cancel——否则 journal 的 failure 与调用方看到的结果会分叉。
   * 走内部 failRun 路径：first-wins（对 complete/cancel/内部失败）、driver 侧取消在飞 ask、
   * journal run 状态 failed + failure_json、发 run-failure 事件、settled 结算为 failed。
   */
  fail(error: WorkflowError): void {
    this.failRun(error);
  }

  /**
   * 就地改本 run **自己**的并发上界。
   * 一次只带 `max_concurrency` 的修订作用在活着的 run 上：同一个 runId、不铸后继、不 supersede、
   * 在飞 ask 一个不丢——这正是它与 AmendWorkflow 的全部差别，也是它存在的唯一理由。
   *
   * 返回**这次是否真的改了**。两条 no-op 都返回 false 且不写库、不发事件：run 已结算（宿主的
   * 存活判定与本调用之间的竞态——调用方据此回落到一次真正的 amend），以及新值与当前值相同。
   *
   * 改成时三件事在同一个同步步骤里发生，于是三者永远一致：整份换掉 caps（调度器现读）、写
   * `dwf_run.caps_max_concurrency`（resume 沿用行里的 caps，不落库就恢复成旧上界）、记一条
   * `run-caps-changed`。**抬高**还多一次 `pumpAll()`——上界是派发前现读的，但没有别的事件会
   * 触发重扫，排队的 ask 否则要等到下一次结算。调低不召回在飞 ask：它们照常跑完，上界只管
   * 「还能不能再放一个」。
   *
   * 钳到 `[1, 天花板]` 归调用方——天花板是宿主事实（机器核数），引擎既看不见也不该看见。
   * 这里只做落库归一，与构造函数里的 `inheritedTokens` 同一条纪律：这个数要落
   * `caps_max_concurrency`（integer not null），一个 NaN 会同时毒化列值与派发判据。
   */
  setMaxConcurrency(maxConcurrency: number): boolean {
    if (this.runSettled) return false;
    if (!Number.isFinite(maxConcurrency)) return false;
    const previous = this.caps;
    const next = Math.max(1, Math.floor(maxConcurrency));
    if (next === previous.maxConcurrency) return false;
    const caps: Caps = { maxConcurrency: next };
    this.caps = caps;
    this.journal.updateRunCaps(this.runId, caps);
    this.record({ type: "run-caps-changed", runId: this.runId, caps, previous });
    if (next > previous.maxConcurrency) this.scheduler.pumpAll();
    return true;
  }

  // ——————————————————————————— Boundary B（向上回报）———————————————————————————

  askSubmitAttempted(instance: InstanceRef, payload: unknown): void {
    this.scheduler.submitAttempted(instance, payload);
  }

  askTurnEnded(instance: InstanceRef, finalText: string): void {
    this.scheduler.turnEnded(instance, finalText);
  }

  /**
   * ask 内进度观察：**纯透传**，
   * 与 askStats → usage-updated 同一副姿态——不改调度、不进 inputHash、replay 不比对它。
   * 与用量同理，结算后到达的迟到观察只丢事件（这里没有账要入），不给已完结的 run 长尾巴。
   */
  askProgress(instance: InstanceRef, progress: AskProgress): void {
    if (this.runSettled) return;
    this.record({ type: "node-progress", instance, ...progress });
  }

  askStats(instance: InstanceRef, stats: AskStats): void {
    this.scheduler.noteStats(instance, stats);
    this.spentTokens += stats.tokens;
    // 用量不能只在 createRun(0) 落库一次：累计不回写会让 resume 恢复出零用量。
    // 累加后立刻持久化，且早于事件——事件载荷与列值在同一同步步骤产生，二者永远相等。
    this.journal.updateRunUsage(this.runId, this.spentTokens);
    // 结算后到达的 straggler stats（真实 actor 的用量在 turn 解析后才知道，最后一个
    // ask 的 stats 常晚于 complete/cancel 到达）不能照发事件——事件被追加在 run-settled 之后，
    // 下游投影不预期（run-settled 必须是事件流最后一条）。结算后只记账（用量行 + noteStats
    // 的节点回填都是 journal 行更新，不是事件），不再发事件。账仍要入：可 resume 的 run 用量
    // 跨生命周期连续。
    if (this.runSettled) return;
    this.record({ type: "usage-updated", spentTokens: this.spentTokens });
  }

  askFailed(instance: InstanceRef, error: WorkflowError): void {
    this.scheduler.failed(instance, error);
  }

  /**
   * 确定性的模型侧错误：不结算节点，
   * 整个 run 以 `stopped(provider)` 停下。run 已结算后到达的调用被 `stop` 的 first-wins 忽略。
   */
  stopRun(error: WorkflowError): void {
    this.stop("provider", enrichProviderStopPhase(error, this.instancePhases));
  }

  /** run 级停滞观察：只 record；run 已结算后到达的调用忽略。 */
  runStalled(info: RunStallInfo): void {
    if (this.runSettled) return;
    this.record({ type: "run-stalled", ...info });
  }

  // 自适应并发的三个纯观察：引擎只 record，
  // 不据它们做任何决策。run 已结算或节点已结算后到达的调用被忽略——那是迟到的观察，
  // 落 journal 只会让一个已完结的 run 长出尾巴。
  askWaiting(instance: InstanceRef, info: AskWaitInfo): void {
    if (this.runSettled || !this.scheduler.isLive(instance)) return;
    this.record({ type: "node-waiting", instance, ...info });
  }

  askExecuting(instance: InstanceRef): void {
    if (this.runSettled || !this.scheduler.isLive(instance)) return;
    this.record({ type: "node-executing", instance });
  }

  /**
   * 唯一一个引擎据以做决策的 driver 观察：在飞 ask 的子代理
   * 即将改写工作区 ⇒ 关导入缓存。迟到的观察（run 或节点已结算）忽略，与上面两个同一姿态。
   */
  askMutating(instance: InstanceRef): void {
    if (this.runSettled || !this.scheduler.isLive(instance)) return;
    closeImportCache(this.state, instance, "mutating-tool", this.scheduler.liveActorName(instance));
  }

  concurrencyChanged(change: ConcurrencyChange): void {
    if (this.runSettled) return;
    this.record({ type: "concurrency-changed", ...change });
  }

  // ——————————————————————————————— 内部 ———————————————————————————————

  /** run 级失败（first-wins）：方法体在 engine-settlement.ts 的 settleFailed。 */
  private failRun(error: WorkflowError): void {
    settleFailed(this.state, error);
  }

  /**
   * 站点序号的**唯一**铸造点（createActor / admitAsk 第一行 / worldRead / report / artifact 全
   * 同步经它），因此也是实例出生阶段的唯一记录点：这一行总在脚本调用的同一个 tick 里执行，
   * 而事件未必——ask 的 `node-queued` 会被 hold 规则推迟到下一个标记之后才发出。
   */
  private nextOrdinal(siteId: string): number {
    const next = (this.ordinals.get(siteId) ?? 0) + 1;
    this.ordinals.set(siteId, next);
    if (this.currentPhase !== undefined)
      this.instancePhases.set(refToString({ siteId, ordinal: next }), this.currentPhase);
    return next;
  }

  private assertRunning(op: string): void {
    if (this.runSettled)
      throw (
        this.runFailure ??
        new WorkflowError("Cancelled", `Run already settled; ${op} is no longer accepted.`)
      );
  }

  private runError(): WorkflowError {
    return this.runFailure ?? new WorkflowError("Cancelled", "Run already settled.");
  }

  /**
   * 每个事件既落 journal（dwf_event，持久）又经 driver.emit 扇出（Boundary C，实时）。
   *
   * 所有事件的唯一漏斗，出生阶段也就在这里补上（{@link stampBirthPhase}）。两条路线拿到的
   * 必须是**同一个对象**：bootstrap 的 `createJournalSequenceCapture` 按引用相等核对序号。
   */
  private record(event: RunEvent): void {
    const stamped = stampBirthPhase(event, this.instancePhases);
    this.journal.appendEvent(this.runId, stamped);
    this.driver.emit(stamped);
  }
}

/** persona 规范化：字符串视为 system prompt；display name 落到 persona.name。 */ function normalizePersona(
  name: string | undefined,
  persona: string | PersonaSpec | undefined,
): PersonaSpec {
  const base: PersonaSpec =
    typeof persona === "string" ? { system: persona } : persona ? { ...persona } : {};
  if (base.name === undefined && name !== undefined) base.name = name;
  return base;
}
