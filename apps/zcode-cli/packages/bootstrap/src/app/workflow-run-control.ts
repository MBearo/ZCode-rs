// ============================================================
// run 的活体控制面：一次命令同时落到引擎与座位闸门
// ============================================================
// 一个在飞 run 的两个执行点住在不同的
// 层里——调度器在引擎（`@zcode/dynamic-workflow`），座位闸门在 driver 之下（本包）——而发命令的
// 那一侧（run service 的 `retuneConcurrency`）两个都够不着：引擎是 harness 在子进程装配起来之后
// 才存在的，闸门是 launch 造的。
//
// 所以 run service 先造一个**空的**句柄放进注册表条目，两边各自在自己出生的那一刻把自己接上去：
// harness 拿到 `bind(engine)`（与 `signal` 同一条缝递进去），launch 拿到 `bindSeatGate(gate)`。
// 句柄本身不判断任何事：存活判定、no-op 语义与落库全在引擎的 `setMaxConcurrency` 里，闸门只在
// 引擎说"这次真的改了"之后才跟着换上界——两个执行点因此不可能各执一词。
//
// 顺序是载荷性的：**引擎先**。引擎的布尔值就是这次命令的裁决（已结算 / 值没变 ⇒ false，什么也
// 没发生），闸门若抢在前面换了上界，一个已经结算的 run 就会留下一个与 journal 行不符的内存上界。

import type { RunControlBinding } from "@zcode/dynamic-workflow-runtime";
import type { WorkflowRunSeatGate } from "./workflow-seat-gate.js";

export interface WorkflowRunControl extends RunControlBinding {
  /**
   * 就地改本 run 自己的并发上界。返回**这次是否真的改了**：`false` 即什么也没发生——引擎还没
   * 接上（launch 之前的那几个微任务）、run 已结算，或新值与当前值相同。调用方据此回落。
   */
  setMaxConcurrency(maxConcurrency: number): boolean;
  /** launch 造好座位闸门之后接上去；缺席即这个 run 只有调度器一个执行点。 */
  bindSeatGate(gate: Pick<WorkflowRunSeatGate, "setLimit">): void;
}

export function createWorkflowRunControl(): WorkflowRunControl {
  let engine: { setMaxConcurrency(maxConcurrency: number): boolean } | undefined;
  let seatGate: Pick<WorkflowRunSeatGate, "setLimit"> | undefined;
  return {
    bind: (bound) => {
      engine = bound;
    },
    bindSeatGate: (gate) => {
      seatGate = gate;
    },
    setMaxConcurrency: (maxConcurrency) => {
      if (engine === undefined) return false;
      if (!engine.setMaxConcurrency(maxConcurrency)) return false;
      seatGate?.setLimit(maxConcurrency);
      return true;
    },
  };
}
