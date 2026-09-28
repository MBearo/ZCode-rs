// Dynamic Workflow Run Port：调整运行中的并发上限。
// 定义请求、结果与能力边界，由 dynamic-workflow-run.port.ts 统一再导出，
// 调用方通过 `@zcode/contracts` 使用。

/**
 * {@link import("./dynamic-workflow-run.port.js").DynamicWorkflowRunPort.retuneConcurrency}
 * 的请求。
 */
export interface DynamicWorkflowRunRetuneRequest {
  runId: string;
  /**
   * 新的并发上界。`null` = 天花板 = 解除本 run 自己的限制（与 `AmendWorkflow` 的
   * `max_concurrency: null` 同义）。给了数就钳到 `[1, 天花板]`——钳制用的是端口自己那一份实现，
   * 工具层无须、也不该再钳一次得到第二个答案。
   */
  maxConcurrency: number | null;
}

/**
 * retune 被拒的结构化理由。两者对调用方是**两个不同的下一步**，所以必须可分辨：
 *
 *   - `not_live`：本 service 手上没有这个在飞 run（从来不是本进程的、已经结算的），或者它恰好
 *     在存活判定与本次调用之间结算了。调用方回落到一次真正的修订（`AmendWorkflow` 的既有那条路）。
 *   - `unchanged`：值与此刻生效的上界相同。什么都没写、什么都没停——`current` 就是那个值，
 *     调用方据它写出「已经是 n 了」。
 */
export type DynamicWorkflowRunRetuneRefusalReason = "not_live" | "unchanged";

/**
 * retune 的结构化结果。失败走 reason 而不是 throw，与
 * {@link import("./dynamic-workflow-run.port.js").DynamicWorkflowRunAmendResult} 同一条论证：
 * 两种理由都是调用方可预期的业务分支。
 *
 * `ceiling` 与 `previous` 不是锦上添花：模型面的回话要说「至多 n 个子代理同时运行」，而 n 等于
 * 天花板时该说的是「限制已解除」（与 `CreateWorkflow` 的回话同一条判据）；事件日志要说「8 → 2」，
 * 从单个新值推不出前一个。两者都只有端口这一侧知道，留给调用方重算就是第二份天花板实现。
 */
export type DynamicWorkflowRunRetuneResult =
  | { ok: true; maxConcurrency: number; previous: number; ceiling: number }
  | {
      ok: false;
      reason: DynamicWorkflowRunRetuneRefusalReason;
      /** 此刻生效的上界；`unchanged` 恒在场，`not_live` 只在还读得到时在场。 */
      current?: number;
    };
