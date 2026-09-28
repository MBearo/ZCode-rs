// ============================================================
// Dynamic Workflow Run Service：journal 能力探测与 store 窄化
// ============================================================
// dynamic-workflow-run-service.ts 顶到 oxlint max-lines 上限（400 行），把 journal /
// task link store 的结构性窄化与四组能力探测拆到本文件；公开面仍从 dynamic-workflow-run-service.ts
// 导出。
//
// 这里的每一条探测都遵守同一条纪律：store 是端口、实现可替换，所以按能力探测而不是 instanceof；
// 缺席即**可见降级**（对应读面不实现 / 回空 / 不构造），绝不静默退回内存实现。

import type { DwfRunIntrospectionQueries, DwfRunSessionListItem } from "@zcode/adapters/storage";
import type { CreateSessionTaskLinkInput, Logger, SessionStorePort } from "@zcode/contracts";
import type { JournalStorePort, RunRecord } from "@zcode/dynamic-workflow";

/** task link 落库面（生产是 SqliteSessionStore；测试传 spy）。 */
export interface DynamicWorkflowTaskLinkStore {
  createSessionTaskLink(input: CreateSessionTaskLinkInput): Promise<unknown>;
}

/**
 * task link 落库面的结构性窄化。与 {@link resolveDynamicWorkflowJournalStore} 同一处理：
 * store 是端口，实现可替换，所以按能力探测而不是 instanceof。缺失时跳过建 link——
 * actor 会话本身仍落库，只是会话树里少一条归属边（可降级，不影响 run 的正确性）。
 */
export function isDynamicWorkflowTaskLinkStore(
  store: SessionStorePort | undefined,
): store is SessionStorePort & DynamicWorkflowTaskLinkStore {
  return (
    typeof (store as { createSessionTaskLink?: unknown } | undefined)?.createSessionTaskLink ===
    "function"
  );
}

/**
 * 从 session store 里窄化出 dwf journal。结构性判断而非 instanceof——store 是端口，
 * 实现可替换（precedent: isScriptWorkflowStore，script-workflow-utils.ts）。
 */
export function resolveDynamicWorkflowJournalStore(
  sessionStore: SessionStorePort | undefined,
  logger?: Logger,
): JournalStorePort | undefined {
  const candidate = sessionStore as
    | (SessionStorePort & { workflowJournalStore?: () => JournalStorePort })
    | undefined;
  if (typeof candidate?.workflowJournalStore !== "function") {
    // 可见降级：端口不构造 → CreateWorkflow 回占位诊断。绝不退回内存 journal——
    // 那会让 run 看起来跑起来了，却在进程退出时把一切静默丢掉。
    logger?.info?.("Dynamic workflow run service disabled: session store has no dwf journal", {
      event: "dynamic_workflow.run_service.unavailable",
      module: "bootstrap.app",
      reason: "session_store_missing_workflow_journal_store",
    });
    return undefined;
  }
  return candidate.workflowJournalStore();
}

/**
 * 宿主侧的 journal 面：引擎端口 + 孤儿收敛与枚举用的窄查询。
 *
 * 刻意**不加宽**引擎的 {@link JournalStorePort}：引擎只按 runId 读写自己那一行，从不按父会话
 * 找 run——这两条查询是宿主的需求，加进领域端口等于要求每个 journal 实现都为引擎不做的事
 * 负责（内存实现就不提供它们）。与 {@link isDynamicWorkflowTaskLinkStore} 同一处理：能力探测
 * 而不是 instanceof，store 是端口、实现可替换。
 */
interface DynamicWorkflowJournalStore extends JournalStorePort {
  listNonTerminalRuns(parentSessionId: string): RunRecord[];
  /**
   * 某父会话名下的 run，最近更新在前，最多 limit 条（枚举面，服务 listRunsForSession）。
   *
   * 回 {@link DwfRunSessionListItem} 而不是 `RunRecord`：`RunRecord` 刻意不带时间（引擎不
   * 关心），而枚举面要报 `updatedAt`。窄投影不读 `result_json`（无界产物，列表不展示），
   * 但**保留 failure**——`resumable` 的谓词依赖 failure.code。
   */
  listRunsByParentSession(parentSessionId: string, limit: number): DwfRunSessionListItem[];
}

/** journal 是否带孤儿收敛所需的窄查询（生产的 SQLite 实现带，引擎的内存实现不带）。 */
export function supportsNonTerminalRunQuery(
  journal: JournalStorePort,
): journal is JournalStorePort & Pick<DynamicWorkflowJournalStore, "listNonTerminalRuns"> {
  return (
    typeof (journal as Partial<DynamicWorkflowJournalStore>).listNonTerminalRuns === "function"
  );
}

/** journal 是否带枚举窄查询。两条查询独立探测：缺一条只降级对应的读面，不连坐。 */
export function supportsRunEnumeration(
  journal: JournalStorePort,
): journal is JournalStorePort & Pick<DynamicWorkflowJournalStore, "listRunsByParentSession"> {
  return (
    typeof (journal as Partial<DynamicWorkflowJournalStore>).listRunsByParentSession === "function"
  );
}

/**
 * 带 run 内省查询的 journal（`ListWorkflowRuns` / `GetWorkflowRun` 的取数底座）。
 *
 * 签名的**唯一来源**是 adapters 的 {@link DwfRunIntrospectionQueries}（`import type`，运行时
 * 零依赖）。刻意不在这里手抄一遍：这四条查询不在引擎的 {@link JournalStorePort} 上（引擎从不
 * 枚举 run、也不做聚合计数），所以它们只能靠能力探测接上——一旦签名漂移，编译器什么都不会说，
 * 只会让两个工具静默降级成「本会话没有这个能力」。
 */
export interface DynamicWorkflowIntrospectableJournal
  extends JournalStorePort, DwfRunIntrospectionQueries {}

/**
 * journal 是否带「每一世的活动区间」读面（完成卡的时长口径）。
 *
 * **独立探测，不并入 {@link supportsRunIntrospection} 的四条**：那四条一起探是因为它们共同
 * 支撑两个工具的可用性，而这一条只支撑一个数字。缺它的后果是时长退回「本世墙钟」——一个更
 * 保守的答案，不是一个坏掉的工具，所以它不该连坐 `GetWorkflowRun` 的可用性（引擎自带的内存
 * journal 就不提供它，而内存 journal 里的 run 本来也活不过进程）。
 */
export function supportsRunLifeSpans(
  journal: JournalStorePort,
): journal is JournalStorePort & Pick<DwfRunIntrospectionQueries, "listRunLifeSpans"> {
  return typeof (journal as Partial<DwfRunIntrospectionQueries>).listRunLifeSpans === "function";
}

/**
 * journal 是否带 run 内省查询。**四条一起探**：能力是整体的（列表要 listRuns，详情要另外三条），
 * 部分在场的实现只会让某一个工具在运行时炸掉，而不是可见地降级。
 */
export function supportsRunIntrospection(
  journal: JournalStorePort,
): journal is DynamicWorkflowIntrospectableJournal {
  const candidate = journal as Partial<DwfRunIntrospectionQueries>;
  return (
    typeof candidate.countNodesByStatus === "function" &&
    typeof candidate.getRunRow === "function" &&
    typeof candidate.listRecentLogEvents === "function" &&
    typeof candidate.listRuns === "function"
  );
}
