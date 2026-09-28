// ============================================================
// run 级座位闸门：把本 run 自己的并发上界压到**下一次模型请求**上
// ============================================================
// 调度器是第一个执行点，但它只在
// 派发时看上界，而一个 ask 是子代理的一整轮、动辄数分钟——对一个已有八个在飞 ask 的 run 说
// 「最多两个」，光靠调度器要等六个 ask 自己跑完才看得见。本闸门是第二个执行点：超出上界的子代理
// 跑完手上这次请求之后**在下一个 turn step 前停住**，会话、转录与它在 run 里的位置一个不丢。
//
// 三条纪律：
//   1. **纯的**：不读时钟、不做 I/O、不订阅任何东西。两件事实（谁有在飞 ask、谁有工具在跑）都由
//      driver 既有的观察面喂进来（workflow-driver.ts 的 startAsk / emit，
//      workflow-driver-tool-activity.ts 的在飞计数），闸门自己不再记第二份账。
//   2. **与调度器零共享状态**，两者也从不互相调用。让它们一致的是算术：`activeAsks = working +
//      parked`——停驻的子代理那个 ask 仍然活着，仍然算在调度器的上界里。于是只要有人停驻，
//      `activeAsks ≥ limit`，调度器就派发不出新 ask，绝不可能插到一个停驻者前面。
//   3. **上界 ≥ 1 ⇒ 永不死锁**：停驻的前提是「工作中的人数已经超过上界」，所以总有人在工作；
//      FIFO 不空时最后一个座位不可能是空的。
//
// 工具侧的请求**永不**停驻：那个子代理本就在
// 工作、本就占着座位，让它的 WebSearch 排在自己后面就是排给自己看。准入调用上只有 `{model}`，
// 分不出是什么请求，所以闸门读 driver 已经为这个子代理记着的那条事实——它此刻有没有工具在跑。

import type { ModelRequestAdmission } from "@zcode/contracts";
import { refToString, type InstanceRef } from "@zcode/dynamic-workflow";

/** 闸门向 driver 要的唯一一条子代理事实（实现在 workflow-driver-tool-activity.ts）。 */
export interface SeatGateSubagent {
  /** 此刻有几个工具调用在跑；> 0 即这次请求是工具侧的，直接放行。 */
  toolsInFlight(): number;
}

/** 闸门此刻的两个计数（观察面：`activeAsks = working + parked` 的断言读它）。 */
export interface SeatGateStats {
  limit: number;
  working: number;
  parked: number;
}

export interface WorkflowRunSeatGate {
  /**
   * 换上界。抬高之后立刻按 FIFO 放行到新上界为止；压低不召回任何人——超出的那些跑完手上这次
   * 请求、在下一个 turn step 上自己停驻。
   */
  setLimit(limit: number): void;
  /** 一个 ask 派发到了这个子代理（driver.startAsk 的同一刻）。 */
  askStarted(key: string, instance: InstanceRef): void;
  /** 引擎为这个实例记下了 `node-settled`（ask 的**唯一**终点，见 workflow-driver.ts 的 emit）。 */
  askSettled(instance: InstanceRef): void;
  /**
   * 把一个子代理的准入端口包成「先过座位、再过治理器」的那一个。`inner` 缺席即这个 runtime
   * 本就不受闸门约束（没有治理器端口的装配），闸门也不凭空造一个——两条闸门要么一起在，
   * 要么一起不在。
   */
  wrap(
    key: string,
    subagent: SeatGateSubagent,
    inner: ModelRequestAdmission | undefined,
  ): ModelRequestAdmission | undefined;
  stats(): SeatGateStats;
}

/** FIFO 里的一位：它的键、解开它的两个口，以及撤掉 abort 监听的那一手。 */
interface ParkedSeat {
  key: string;
  grant: () => void;
  refuse: (reason: unknown) => void;
}

export function createWorkflowRunSeatGate(input: { limit: number }): WorkflowRunSeatGate {
  let limit = Math.max(1, Math.floor(input.limit));
  /** 有在飞 ask 且**没有**停驻的子代理。停驻的那一刻从这里移出去，放行时再加回来。 */
  const working = new Set<string>();
  /** 等座位的子代理，先来先走。 */
  const parked: ParkedSeat[] = [];
  /** `refToString(instance)` → 子代理键：ask 的终点只带实例，回不到子代理身上就无从腾座位。 */
  const instances = new Map<string, string>();

  const parkedIndexOf = (key: string): number => parked.findIndex((seat) => seat.key === key);

  /** 还有空位就按 FIFO 放行。放行是「进 working」而不是「发一张票」——座位就是 working 的名额。 */
  const unpark = (): void => {
    while (working.size < limit && parked.length > 0) {
      const seat = parked.shift()!;
      working.add(seat.key);
      seat.grant();
    }
  };

  /** 这次请求要不要过座位：只有「有在飞 ask 且手上没有工具在跑」的子代理才是一个 turn step。 */
  const needsSeat = (key: string, subagent: SeatGateSubagent): boolean =>
    working.has(key) && subagent.toolsInFlight() === 0;

  const acquireSeat = async (key: string, signal: AbortSignal | undefined): Promise<void> => {
    // 上界之内：原地通过，不动任何状态。没有 retune 压低过的 run 永远走这一支——快路径一字未变。
    if (working.size <= limit) return;
    if (signal?.aborted === true) throw signal.reason;
    working.delete(key);
    return await new Promise<void>((resolve, reject) => {
      const onAbort = (): void => {
        const index = parkedIndexOf(key);
        if (index >= 0) {
          parked.splice(index, 1);
          // **出队即回到 working**：这个闸门里「不在 parked」只有一种含义，就是在工作。abort 的
          // 是**这次请求**，不一定是这个 ask——driver 侧的瞬态重驱、流恢复、任何 per-request 信号
          // 都会在 ask 还活着的时候走到这里。把它留在两个集合之外，它此后每一次 turn 请求都因为
          // `needsSeat` 为假而无闸通过，而且再也不会被数进 working：上界会悄悄地往上漂。
          //
          // 加回去可能让 working 一时超过上界，那是合法的瞬态（与调低上界那一刻同形）：下一个
          // 提出 turn 请求的人照常停驻，计数随即收敛。abort 之后真的结算时，askSettled 会
          // `working.delete` 成功并调 unpark，而 unpark 的 `working.size < limit` 守卫挡住了
          // 「腾出一个它从未占过的座位」——停驻发生时 working ≥ limit，加回来就是 ≥ limit+1，
          // 删掉之后仍 ≥ limit，于是一个都不放。
          working.add(key);
        }
        reject(signal?.reason);
      };
      const seat: ParkedSeat = {
        key,
        grant: () => {
          signal?.removeEventListener("abort", onAbort);
          resolve();
        },
        refuse: (reason) => {
          signal?.removeEventListener("abort", onAbort);
          reject(reason);
        },
      };
      parked.push(seat);
      signal?.addEventListener("abort", onAbort, { once: true });
    });
  };

  return {
    setLimit: (next) => {
      limit = Math.max(1, Math.floor(next));
      unpark();
    },

    askStarted: (key, instance) => {
      instances.set(refToString(instance), key);
      // 停驻中的子代理不可能收到新 ask（它的 turn 正卡在 acquire 里，引擎那边 actor.current 还占着），
      // 但两个集合同时收下同一个键会把它数成两个人——宁可在这里挡一次。
      if (parkedIndexOf(key) >= 0) return;
      working.add(key);
    },

    askSettled: (instance) => {
      const ref = refToString(instance);
      const key = instances.get(ref);
      if (key === undefined) return;
      instances.delete(ref);
      const parkedIndex = parkedIndexOf(key);
      if (parkedIndex >= 0) {
        // 停驻中 ask 结束：**不腾座位**（它本就没有），否则这一位会被数两次。
        // 只可能是引擎主动取消（停驻中的 turn 卡在 acquire 里，报不出任何 turn 终局），而那条路
        // 先调 driver.cancelAsk（abort 会拒掉上面的等待）再记 node-settled，所以正常次序下这里
        // 已经找不到它了。反序到达时把等待一并拒掉，免得一个没人要的 ask 之后还被放行。
        const [seat] = parked.splice(parkedIndex, 1);
        seat?.refuse(new Error("workflow ask settled while waiting for a concurrency seat"));
        return;
      }
      if (!working.delete(key)) return;
      unpark();
    },

    wrap: (key, subagent, inner) => {
      if (inner === undefined) return undefined;
      return {
        // 快路径：这次请求要过座位、而座位已经满了 ⇒ 未命中。runner 因此发 `model_request_queued`，
        // driver 报 `askWaiting(slot)`——与共享 cap 造成的等待逐字相同的那一条，不需要新词汇。
        tryAcquire: (request) => {
          if (needsSeat(key, subagent) && working.size > limit) return undefined;
          return inner.tryAcquire?.(request);
        },
        // 顺序是载荷性的：**先**等座位，**再**过治理器。反过来就会让一个本该停驻的子代理先占住
        // 治理器的一张票，再在闸门这边睡——那张票对谁都没有用。
        acquire: async (request) => {
          if (needsSeat(key, subagent)) await acquireSeat(key, request.signal);
          return await inner.acquire(request);
        },
      };
    },

    stats: () => ({ limit, working: working.size, parked: parked.length }),
  };
}
