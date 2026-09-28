// ============================================================
// Dynamic Workflow Run Service：就地改一个在飞 run 的并发上界
// ============================================================
// 这是 `DynamicWorkflowRunPort.retuneConcurrency` 的实现体，与三条启动入口（dynamic-workflow-run-submit.ts）刻意分居：那三条
// 都会**铸一个 run**，这一条一个 run 都不铸——它只对着已经在飞的那个发一条命令。
//
// 两种拒绝在**不同的地方**判定，这正是让它们分得开的原因：
//   - `unchanged` 是本模块自己的答案，读的是服务此刻持有的上界，在句柄被调之前就判完；
//   - 于是引擎的 setter 只剩一个说 false 的理由——run 在这两步之间结算了——端口把它报成 `not_live`。
//
// 顺序也是载荷性的：先句柄（引擎换 caps + 写行 + 记事件 + 闸门换上界，一个同步片），成了才动
// 服务自己的内存副本。反过来就会在 run 恰好结算的那一瞬留下一个与 `dwf_run` 行不符的内存上界，
// 而快照与详情读的正是那一份。

import type {
  DynamicWorkflowRunRetuneRequest,
  DynamicWorkflowRunRetuneResult,
} from "@zcode/contracts";
import type { JournalStorePort } from "@zcode/dynamic-workflow";
import type { RunRegistryEntry } from "./dynamic-workflow-run-observation.js";
import { clampRunConcurrency } from "./workflow-concurrency-ceiling.js";

/** 本模块借用的 service 内部状态；全是引用，本文件不持有任何自己的状态。 */
export interface DynamicWorkflowRunRetuneContext {
  runs: Map<string, RunRegistryEntry>;
  journal: JournalStorePort;
  /** 与 caps 起点、两条读面判据同一个函数（见 run service 的 `concurrencyCeiling`）。 */
  concurrencyCeiling: () => number;
}

export function retuneRunConcurrency(
  ctx: DynamicWorkflowRunRetuneContext,
  request: DynamicWorkflowRunRetuneRequest,
): DynamicWorkflowRunRetuneResult {
  const ceiling = ctx.concurrencyCeiling();
  // `null` = 天花板 = 解除本 run 自己的限制。与提交时**同一条**钳制（工具层已经钳过一次是为了让
  // 确认窗显示将要生效的值；端口再钳是端口自己的契约，两次必然同值）。
  const next = clampRunConcurrency(request.maxConcurrency ?? undefined, ceiling);

  const entry = ctx.runs.get(request.runId);
  if (entry === undefined || entry.terminal !== undefined || entry.control === undefined) {
    // 本 service 手上没有这个活条目：从来不是本进程的 run、早已结算的 run，或者升级前留下的
    // 无控制面条目。三者对调用方是同一个下一步——走一次真正的修订。
    return { ok: false, reason: "not_live" };
  }

  // 此刻生效的上界：**有条目就读条目**（三条建条目的路都落值），冷行才回退到 journal。
  // 两者都没有只可能是 submit → createRun 之间那几个微任务里的接线异常，按「跑在天花板上」读。
  const current =
    entry.maxConcurrency ?? ctx.journal.getRun(request.runId)?.caps.maxConcurrency ?? ceiling;
  if (current === next) return { ok: false, reason: "unchanged", current };

  if (!entry.control.setMaxConcurrency(next)) {
    // 条目还在、引擎却说没改：run 在存活判定与这一行之间结算了（竞态），或者引擎还没接上
    // （launch 之前的那几个微任务）。两者都是「这一刻不可就地改」。
    return { ok: false, reason: "not_live", current };
  }
  // 内存副本一并挪动：快照读的是 `entry.maxConcurrency ?? 行`，条目优先——不挪就会在 retune
  // 之后继续报提交时那个数，而 `AmendWorkflow` 的 resolveInput 正是读这张快照判「沿用什么」。
  entry.maxConcurrency = next;
  return { ok: true, maxConcurrency: next, previous: current, ceiling };
}
