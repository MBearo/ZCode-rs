// ============================================================
// AgentRuntime-backed WorkflowDriver：typed 结果的提交桥接（submit_result）
// ============================================================
// 修复原因：workflow-driver.ts 又顶到 oxlint max-lines 上限（400 行），把会话级 submit 端口拆到
// 本文件成自由函数，与隔壁的升级桥接（workflow-driver-escalation.ts 的 `makeSessionEscalatePort`）
// 逐条对称——两者本就是同一副形状的两条时序（mid-turn 阻塞 → 上报 → 引擎回裁决 → 解开 deferred）。
// 公开面不变，driver 类上只剩一处调用。
//
// 对 driver 状态的全部触碰都经 {@link SubmitBridgeHost} 显式递进来（会话表 + 向上回报面），
// 本文件不持有任何自己的状态——原方法体逐字保留，只把 `this.` 换成 `host.`。

import type {
  SessionId,
  SubmitResultRequest,
  SubmitVerdict as ContractsSubmitVerdict,
  WorkflowSubmitPort,
} from "@zcode/contracts";
import { WorkflowError, type WorkflowReportSink } from "@zcode/dynamic-workflow";
import { defer, rejectWith } from "./workflow-driver-helpers.js";
import type { SessionState } from "./workflow-driver-types.js";

/**
 * driver 交给提交桥接的宿主面。两样都是 driver 私有状态的**引用**（不是副本）：`sessions` 就是
 * 类里那张会话表，`sink` 是 Boundary B 的向上回报面（`askSubmitAttempted` 在本调用栈内被引擎
 * 同步回裁决，所以这两者必须是同一世的那一对）。
 */
export interface SubmitBridgeHost {
  readonly sessions: ReadonlyMap<string, SessionState>;
  readonly sink: WorkflowReportSink;
}

/**
 * 造一个会话级 submit 端口：`submit_result` handler mid-turn 调用它并阻塞等裁决。
 *
 * closure 绑定本会话，模型无法覆盖路由身份（instance 取自 `currentInstance`）。
 */
export function makeSessionSubmitPort(
  host: SubmitBridgeHost,
  sessionId: SessionId,
): WorkflowSubmitPort {
  return {
    respond: (request: SubmitResultRequest): Promise<ContractsSubmitVerdict> => {
      const state = host.sessions.get(sessionId);
      const instance = state?.currentInstance;
      if (state === undefined || instance === undefined) {
        // 无在飞 ask 却收到 submit：不路由到引擎，直接拒绝（避免悬挂）。
        return Promise.resolve(rejectWith("no active ask is awaiting a submitted result"));
      }
      // 未声明结果类型的 ask 可能仍注册了 submit_result；这里立即拒绝提交并提示使用普通回复。
      // 不能把它交给引擎后等待裁决：引擎对 untyped ask 不处理 submit，等待中的 deferred 将无法结束。
      if (!state.currentTyped) {
        return Promise.resolve(
          rejectWith(
            "this ask does not accept submit_result; provide your answer as your final message",
          ),
        );
      }
      // 当前实例不变式：至多一个挂起 deferred。若已有（不应发生），先拒旧的避免泄漏。
      state.pendingSubmit?.reject(
        new WorkflowError("DriverError", "This submit was superseded by a newer submit."),
      );
      const deferred = defer<ContractsSubmitVerdict>();
      state.pendingSubmit = deferred;
      // 同步上报：引擎在本调用栈内校验并经 respondToSubmit 回裁决（同步解开 deferred）。
      host.sink.askSubmitAttempted(instance, request.result);
      return deferred.promise;
    },
  };
}
