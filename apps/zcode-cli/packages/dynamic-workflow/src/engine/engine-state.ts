/**
 * engine.ts 顶到 oxlint max-lines 上限（400 行），把引擎私有状态的显式接缝拆到本文件；
 * 公开面仍从 engine.ts 导出。
 *
 * WorkflowEngine 的几组方法（用户面产物、report、world 节点与导入缓存、run 结算）被拆成兄弟
 * 模块里的自由函数（engine-artifacts.ts / engine-report.ts / engine-world.ts / engine-settlement.ts）。
 * 它们不各自持有状态，而是经这张接缝读写引擎的私有字段：接缝由引擎在构造函数里用箭头闭包
 * 装配，字段本身仍是 private，所以引擎的公开面零变化；类上只留薄薄一层委托方法。
 *
 * 可变标量（报告计数、导入门）以「读方法 + 单向写方法」暴露，而不是 getter/setter——写法只有
 * 一种（计数只增、门只关），接缝面上就说清了这一点。
 */

import type { ImportedWorldQueue } from "./imported-cache.js";
import type { ArtifactOp } from "../facade/registry.js";
import type {
  ImportedRunCache,
  InstanceRef,
  JournalStorePort,
  RunEvent,
  RunStopReason,
  WorkflowDriver,
  WorkflowError,
} from "./types.js";

/**
 * run 的最终结算：`completed`（脚本 return）/
 * `errored`（脚本之错，不可 resume）/ `stopped`（被停下，一律可 resume；`error` 只对
 * `provider` / `interrupted` 在场）。
 */
export type RunSettlement =
  | { status: "completed"; artifact: unknown }
  | { status: "errored"; error: WorkflowError }
  | { status: "stopped"; reason: RunStopReason; supersededBy?: string; error?: WorkflowError };

/**
 * 一个用户面产物 id 在本 run 内的状态：它属于哪个成员种类、已成功几版、（预置才有）它的
 * 规范化 spec。三样都从 journal 行派生，所以 resume 重建与 live 记账得到同一份表。
 */
export interface ArtifactIdState {
  kind: ArtifactOp;
  /** 已成功的版本数（内容成员每成功一次 +1；预置声明恒为 1）。 */
  versions: number;
  /** 预置 spec 的 canonicalJson（重复声明的幂等判定按它比对）。内容成员缺席。 */
  spec?: string;
  /** 这个 id 是 run 的交付物（primary）。全 run 至多一个 id 带它；从 completed 行重建。 */
  primary?: true;
}

/** 引擎私有状态的接缝：兄弟模块里的自由函数经它读写 WorkflowEngine 的私有字段。 */
export interface EngineState {
  readonly runId: string;
  readonly driver: WorkflowDriver;
  readonly journal: JournalStorePort;
  /**
   * 本 run 每个**用户面产物** id 的状态。resume 时从 journal
   * 的 `kind: "artifact"` 行重建，此后在内存里维护——上限与版本号都是 run 级事实，跨 resume 连续。
   *
   * ⚠ 术语：artifact = 用户面产物，不是 `RunSettlement.artifact`（顶层返回值）。
   */
  readonly artifacts: Map<string, ArtifactIdState>;
  /** amend-resume 的导入缓存（纯数据，缺席即本次不是修订续跑）。 */
  readonly importedCache: ImportedRunCache | undefined;
  /** world 导入队列的消费游标（第 n 次出现对第 n 条）。 */
  readonly importedWorld: ImportedWorldQueue;

  isRunSettled(): boolean;
  /** run 已结算时用于 reject / throw 的错误。 */
  runError(): WorkflowError;
  /** run 级失败（first-wins；见 engine-settlement.ts 的 settleFailed）。 */
  failRun(error: WorkflowError): void;
  /** 事件既落 journal 又扇出（Boundary C）；出生阶段在引擎的漏斗里补上。 */
  record(event: RunEvent): void;
  /** 站点序号的唯一铸造点。 */
  nextOrdinal(siteId: string): number;
  /**
   * 受 replay 结算次序约束地释放一次命中。
   * 非 resume、或次序表里没有这个实例时立即执行 `release`。
   */
  holdForReplay(instance: InstanceRef, release: () => void): void;

  /** 本 run 已发布的报告条数（REPORT_CAPS.maxItemsPerRun 的计数器，跨 resume 连续）。 */
  reportCount(): number;
  /** 一条报告过了上限检查、即将落库：计数 +1。 */
  countReport(): void;

  /** 导入缓存是否已关闭。 */
  importClosed(): boolean;
  /** 关门（永不重开）。 */
  closeImport(): void;

  /** 置 run 为已结算；带 failure 时同时记下 run 级失败原因（供 runError 复用）。 */
  markSettled(failure?: WorkflowError): void;
  /** 中止所有在飞 ask（委托调度器）。 */
  abortInFlight(error: WorkflowError, emitCancelled: boolean): void;
  /** 兑现 `engine.settled`。 */
  resolveSettled(settlement: RunSettlement): void;
}
