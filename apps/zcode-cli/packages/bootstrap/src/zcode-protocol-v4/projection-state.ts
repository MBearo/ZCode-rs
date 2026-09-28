// ProductProjection 状态工厂与派生规则。
// 本文件只放纯函数：初始快照、availability/inputRouting 派生、revision 递进判定。
import type {
  ActionAvailability,
  ConversationDelta,
  ConversationSnapshot,
  GoalState,
  InputRouting,
  SessionActionAvailability,
  SessionControl,
  StatePatch,
} from "@zcode/shared/zcode-protocol-v4";

/**
 * 冷恢复合成事件的 traceId 哨兵。
 * 合成事件是「视图重建」而非权威用户动作：ModelSelected 等 config 类合成事件
 * 更新投影 config，但不得声明 种子权威（configModelTouchedByEvent）——
 * 否则历史最后一轮的模型会压掉 resume 回写的 runtime 真值（含草稿态选型）。
 */
export const HYDRATION_TRACE_ID = "hydrate-trace";

export function createInitialConversationSnapshot(
  sessionId: string,
  logEpoch: string,
): ConversationSnapshot {
  return {
    protocolVersion: 1,
    sessionId,
    logEpoch,
    seq: 0,
    revision: 0,
    control: {
      // draft 裁决：会话实体已存在但尚无输入。
      phase: "draft",
      sessionEnded: false,
      canStop: false,
      stopState: "idle",
      stopTargetKind: "unknown",
      activeWorks: [],
      lastError: null,
      apiRetry: null,
    },
    availability: computeAvailability({
      phase: "draft",
      goalStatus: null,
      compacting: false,
      goalVerifying: false,
      queueLength: 0,
      autoDrain: true,
    }),
    inputRouting: computeInputRouting(
      {
        phase: "draft",
        goalStatus: null,
        compacting: false,
        goalVerifying: false,
        queueLength: 0,
        autoDrain: true,
      },
      "queue",
    ),
    meta: { title: "", titleSource: "default" },
    // mode 初值 = core 默认协作模式（session-mode-port getMode 的 "build" 回落）。
    // SessionCreated 事件虽带 mode，但 draft 语义要求无可见 delta（不 bump revision）；
    // 持久化偏好非 build 时以首条 SessionModeChanged 为准（TODO：draft 期免 revision 种子通道）。
    config: {
      provider: "",
      model: "",
      thought: "",
      thoughtLevels: [],
      followupMode: "queue",
      mode: "build",
    },
    modelTransition: null,
    usage: {
      contextWindow: null,
      cumulative: {
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
      },
    },
    queue: { items: [], autoDrain: true },
    pendingInteractions: [],
    pendingCommands: [],
    backgroundWorks: [],
    subagents: { revision: 0, childSessionIds: [], running: [], endedTotal: 0 },
    goal: null,
    plan: null,
    // 软门禁：初始无待审核状态;activate() 上报后由投影写入。
    workspaceHookAdmission: null,
    rows: { window: [], totalCount: 0, firstRowId: null },
  };
}

const ALLOWED: ActionAvailability = { allowed: true };

function denied(reasonCode: string): ActionAvailability {
  return { allowed: false, reasonCode };
}

// guard 派生的输入面（与投影同源）。compacting/goalVerifying 不是独立 phase
// （phase 封闭枚举），从 activeWorks / goal.status 派生后传入。
// queueLength/autoDrain 用于 held 派生。
interface AvailabilityContext {
  phase: SessionControl["phase"];
  goalStatus: GoalState["status"] | null;
  compacting: boolean;
  goalVerifying: boolean;
  queueLength: number;
  autoDrain: boolean;
}

// 裁决表与 packages/formal-proof/src/model.ts 的 evaluate 逐条对齐
// （黄金测试 formal-proof-consistency 背书）；reasonCode = product-protocol guard id。
export function computeAvailability(context: AvailabilityContext): SessionActionAvailability {
  const { phase, goalStatus, compacting } = context;
  const running = phase === "running" || phase === "prewarming";
  const compact = compacting
    ? // formal-proof: duplicateCompactRejected —— running/queued compact 操作锁去重。
      denied("compactOperationLock")
    : phase === "draft"
      ? // formal-proof: idleCannotCompact —— 没有可压缩上下文。
        denied("idleCannotCompact")
      : // running/goal verifier 时命令进入 typed FIFO，completed 时立即执行或进入 held queue。
        ALLOWED;
  return {
    fork: compacting
      ? denied("compactOperationLock")
      : phase === "draft"
        ? denied("forkTargetNotStable")
        : ALLOWED,
    compact,
    switchModelConfig: ALLOWED,
    setFollowupMode: ALLOWED,
    queueEdit: ALLOWED,
    sendQueuedNow: compacting
      ? denied("compactOperationLock")
      : running
        ? ALLOWED
        : denied("sendQueuedNowRequiresRunning"),
    // 独立 pauseGoal 改变 target 产品态；verifier/notSatisfied 期间底层 target 仍是 active，
    // 因此也允许暂停，不能要求当前一定存在 provider abort controller。
    pauseGoal:
      goalStatus === "active" || goalStatus === "verifying" || goalStatus === "notSatisfied"
        ? ALLOWED
        : denied(goalStatus === null ? "noGoalToPause" : "goalNotActive"),
    // resumeGoal = stopPausesActiveGoalTarget 的逆操作：仅 paused 可恢复。
    resumeGoal:
      goalStatus === "paused"
        ? ALLOWED
        : denied(goalStatus === null ? "noGoalToResume" : "goalNotPaused"),
  };
}

export function computeInputRouting(
  context: AvailabilityContext,
  followupMode: "queue" | "guide",
): InputRouting {
  // formal-proof: compactingAcceptsFutureInput
  // —— compact 是维护步骤，输入是未来意图 → 入队，不打断 compact。
  if (context.compacting) {
    return { mode: "enqueue", reasonCode: "compactingAcceptsFutureInput" };
  }
  // goal verifier 是 completion-blocking active work，但不是普通
  // assistant active turn；只看 phase=running 会在 guide 模式下尝试 steer，
  // core 此时没有 steerable activeTurn，导致用户输入既不进 queue 也不进历史。
  if (context.goalVerifying) {
    return { mode: "enqueue", reasonCode: "goalVerifierAcceptsFutureInput" };
  }
  if (context.phase === "running" || context.phase === "prewarming") {
    return { mode: followupMode === "guide" ? "guide" : "enqueue" };
  }
  // completed + queue>0 + autoDrain=false 时，输入不静默入队；
  // 客户端呈现 clear/keep 选择，disposition 随 command 上行。
  const completed =
    context.phase === "completedSuccess" || context.phase === "completedInterrupted";
  if (completed && context.queueLength > 0 && !context.autoDrain) {
    return { mode: "choice", reasonCode: "heldQueueInputRequiresChoice" };
  }
  return { mode: "startNow" };
}

// revision 递进规则（封闭定义）：行结构变化 + A 区（usage/pendingCommands 除外）变化各 +1。
// `workflowRuns` **刻意不在列**，归入 usage/pendingCommands 的豁免类：它是高频派生数据
// （每个节点约 5 次迁移）、自带单调 revision 字段、且没有任何 command 的
// baseRevision CAS 读它——列入会让 run 在飞期间的每次节点迁移都抖动 conversation revision，
// 徒增 CAS 假失败。
const REVISION_BEARING_PATCH_KEYS: ReadonlyArray<keyof StatePatch> = [
  "control",
  "availability",
  "inputRouting",
  "meta",
  "config",
  "queue",
  "pendingInteractions",
  "backgroundWorks",
  "subagents",
  "goal",
  "plan",
];

export function deltaBumpsRevision(delta: ConversationDelta): boolean {
  switch (delta.op) {
    case "row.appended":
    case "row.upserted":
    case "row.removed":
      return true;
    case "row.delta":
      return false;
    case "state.updated":
      return REVISION_BEARING_PATCH_KEYS.some((key) => delta.patch[key] !== undefined);
    // 键级增量与整键 `workflowRuns` patch 同一条豁免（见上面的注释）：它们表达的就是那个键的变化，
    // 换个编码不该换记账规则——列入会让一条在飞的 run 每次节点迁移都抖动 conversation revision。
    case "workflowRun.updated":
    case "workflowRun.removed":
      return false;
  }
}
