// ============================================================
// TUI 侧的 workflowRuns 镜像
// ============================================================
// 单时钟：运行态**只**由共享 reducer 逐事件归约维护（@zcode/shared 的
// workflow-runs-reducer，与 v4 投影同一份实现），外加冷启动/恢复时的一次性补种。
// 没有轮询、没有 setInterval——legacy workflow 面板每秒全量重拉是反面教材。
//
// 镜像比协议状态键多两样东西，都是刻意的：
//   1. `logTailByRunId`：log 事件**不在** workflowRuns schema 里（它们只进 journal 事件日志），
//      但展开详情要显示 log 尾，所以 TUI 自己留最近若干条（有界）。
//   2. `seedByRunId`：冷补种从 listDynamicWorkflowRuns 带回的**展示元信息**（label / updatedAt）。
//      运行态（状态、步数、用量、resumable）**不**从摘要来：冷启动 / `/resume` 时 journal 的事件
//      经 `replayWorkflowRuns` 回放进同一个 reducer（冷回放），镜像里的 run 与重启前逐字节一致；`resumable` 是状态位，由 CLI 在
//      run-settled 载荷上裁定——dynamic-workflow-run.port.ts 的既有裁定「渲染服务端布尔」照旧。

import {
  reduceWorkflowRunsState,
  workflowRunStepCounts as sharedWorkflowRunStepCounts,
  type WorkflowRunActor,
  type WorkflowRunUsage,
  type WorkflowRunProgressEnvelope,
  type WorkflowRunState,
  type WorkflowRunsState,
} from "@zcode/shared/zcode-protocol-v4";

/** log 尾的界：条数与单条长度都限，避免一个话多的 run 把镜像吃成无界。 */
const TUI_WORKFLOW_LOG_TAIL_LIMITS = {
  maxEntries: 10,
  maxEntryLength: 200,
} as const;

/**
 * 冷补种摘要里 TUI 会渲染的**展示**字段（运行态一律走回放，见文件头）。
 *
 * `label` / `updatedAt` 是 additive optional（见 spec 的 `/dwf` 边界行「实现期修订」）：
 * 服务端还没带上时就是 undefined，渲染侧一律退回 runId，绝不在这里造一个假 label。
 */
export type TuiWorkflowRunSeed = {
  runId: string;
  label?: string;
  updatedAt?: number;
};

export type TuiWorkflowMirror = {
  /** 共享 reducer 维护的权威运行态。 */
  state: WorkflowRunsState;
  logTailByRunId: Readonly<Record<string, readonly string[]>>;
  seedByRunId: Readonly<Record<string, TuiWorkflowRunSeed>>;
};

export const EMPTY_TUI_WORKFLOW_MIRROR: TuiWorkflowMirror = {
  state: { revision: 0, runs: [] },
  logTailByRunId: {},
  seedByRunId: {},
};

/**
 * 一条 dwf 进度事件 → 新镜像。
 *
 * **无变化时返回传入的同一个引用**，这样 React 的 setState 会直接跳过重渲染：
 * 共享 reducer 的「null = 语义无变化」契约在这里落成「不重绘」，不需要额外的相等判断。
 */
export function applyWorkflowProgressToMirror(
  mirror: TuiWorkflowMirror,
  envelope: WorkflowRunProgressEnvelope,
): TuiWorkflowMirror {
  const state = reduceWorkflowRunsState(mirror.state, envelope);
  const logTailByRunId = appendWorkflowLogTail(mirror.logTailByRunId, envelope);
  if (state === null && logTailByRunId === mirror.logTailByRunId) return mirror;
  return {
    ...mirror,
    ...(state === null ? {} : { state }),
    logTailByRunId,
  };
}

/**
 * 冷补种：把 `listDynamicWorkflowRuns` 的会话级摘要里的**展示名**并进镜像。
 *
 * 只填元信息，**不**伪造运行态条目——运行态由冷回放（`replayWorkflowRuns` → 共享 reducer）
 * 给出，与重启前逐字节一致；摘要没有的东西（label 之外）这里一个也不造。
 */
export function seedWorkflowMirror(
  mirror: TuiWorkflowMirror,
  seeds: readonly TuiWorkflowRunSeed[],
): TuiWorkflowMirror {
  if (seeds.length === 0) return mirror;
  const seedByRunId = { ...mirror.seedByRunId };
  let changed = false;
  for (const seed of seeds) {
    if (!seed.runId) continue;
    const existing = seedByRunId[seed.runId];
    if (existing && sameSeed(existing, seed)) continue;
    seedByRunId[seed.runId] = seed;
    changed = true;
  }
  return changed ? { ...mirror, seedByRunId } : mirror;
}

/**
 * 步数进度：**已结算 / 已排程**（settled / observed）。
 *
 * 动态工作流没有静态总数，所以分母是已排程节点数，绝不冒充全程百分比。
 *
 * 数法只有一处——@zcode/shared 的 `workflowRunStepCounts`（run 卡、时间线摘要与这里共用）。
 * 这里此前自己数 `nodes`，于是一条撞过节点界的 run 在三个读面上显示三个数字，而且三个都比
 * 真实步数小：触界是**拒新**，被拒的实例根本不在 `nodes` 里，只在 usage 的两个计数器上。
 * 本函数只保留 TUI 的字段名（卡片与 i18n 说的是 nodesSettled / nodesTotal）。
 */
export function workflowRunStepCounts(run: WorkflowRunState): {
  nodesSettled: number;
  nodesTotal: number;
} {
  const { total, settled } = sharedWorkflowRunStepCounts(run);
  return { nodesSettled: settled, nodesTotal: total };
}

/** 卡片渲染需要的全部事实——让视图成为 props 的纯函数（TUI 测试按函数式调用组件）。 */
export type TuiWorkflowCard = {
  runId: string;
  status: WorkflowRunState["status"];
  /** `stopped` 的原因；reducer 从 run-settled 载荷搬运。 */
  stopReason?: WorkflowRunState["stopReason"];
  nodesSettled: number;
  nodesTotal: number;
  label?: string;
  resumable?: boolean;
  usage?: WorkflowRunUsage;
  actors: readonly WorkflowRunActor[];
  logTail: readonly string[];
  error?: string;
  resultPreview?: string;
  truncated?: boolean;
};

/**
 * 工具卡 → workflow run 的联接，按 `toolCallId`（schema 注释里它就是「工具卡 → 详情页的关联键」）。
 * 与 GUI `buildWorkflowRunByToolCallId` 同规。运行态只有一个来源——镜像状态（live 事件与
 * 冷回放经同一个 reducer），补种只给展示名。
 */
export function buildTuiWorkflowCardIndex(
  mirror: TuiWorkflowMirror,
): ReadonlyMap<string, TuiWorkflowCard> {
  const byToolCallId = new Map<string, TuiWorkflowCard>();
  for (const run of mirror.state.runs) {
    // 没有 toolCallId 的 run 没有可联接的卡片，不进表。
    if (!run.toolCallId) continue;
    byToolCallId.set(run.toolCallId, cardFromRun(run, mirror));
  }
  return byToolCallId;
}

function cardFromRun(run: WorkflowRunState, mirror: TuiWorkflowMirror): TuiWorkflowCard {
  const { nodesSettled, nodesTotal } = workflowRunStepCounts(run);
  const seed = mirror.seedByRunId[run.runId];
  return {
    runId: run.runId,
    status: run.status,
    ...(run.stopReason === undefined ? {} : { stopReason: run.stopReason }),
    nodesSettled,
    nodesTotal,
    // label 只有服务端知道；投影条目不带它，命中补种就用补种的。
    ...(seed?.label === undefined ? {} : { label: seed.label }),
    // resumable 是状态位（CLI 在 run-settled 载荷上裁定，reducer 搬运），TUI 不重推导。
    ...(run.resumable === undefined ? {} : { resumable: run.resumable }),
    usage: run.usage,
    actors: run.actors,
    logTail: mirror.logTailByRunId[run.runId] ?? [],
    ...(run.error === undefined ? {} : { error: run.error }),
    ...(run.resultPreview === undefined ? {} : { resultPreview: run.resultPreview }),
    ...(run.truncated === undefined ? {} : { truncated: run.truncated }),
  };
}

function appendWorkflowLogTail(
  current: Readonly<Record<string, readonly string[]>>,
  envelope: WorkflowRunProgressEnvelope,
): Readonly<Record<string, readonly string[]>> {
  if (envelope.eventType !== "log" || !envelope.runId) return current;
  const message = logMessage(envelope.payload);
  if (message === undefined) return current;
  const previous = current[envelope.runId] ?? [];
  const appended = [...previous, message].slice(-TUI_WORKFLOW_LOG_TAIL_LIMITS.maxEntries);
  return { ...current, [envelope.runId]: appended };
}

function logMessage(payload: Record<string, unknown> | undefined): string | undefined {
  if (!payload) return undefined;
  const raw = payload.message;
  if (typeof raw !== "string") return undefined;
  const collapsed = raw.replace(/\s+/gu, " ").trim();
  if (collapsed.length === 0) return undefined;
  return collapsed.slice(0, TUI_WORKFLOW_LOG_TAIL_LIMITS.maxEntryLength);
}

function sameSeed(left: TuiWorkflowRunSeed, right: TuiWorkflowRunSeed): boolean {
  return left.label === right.label && left.updatedAt === right.updatedAt;
}
