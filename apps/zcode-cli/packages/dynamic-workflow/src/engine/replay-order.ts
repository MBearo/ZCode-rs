/**
 * Replay 的**结算次序闸**。
 *
 * 站点序号是调用到达时的计数器，所以一条分支在 await **之后**做的每一次 journal 调用，
 * 编号依的是扇出完成的顺序，而不是脚本发起的顺序。那个顺序是墙钟的，节点行里没有任何东西
 * 能复现它：按准入顺序释放缓存结算，重放的 `Promise.all` 会按数组顺序跑续体，join 之后的第一条
 * `report` 于是拿到 journal 给「最先跑完的那条分支」的序号，run 死在自己的防御性校验里
 * （`InputHashMismatch`），即使脚本本身是确定性的，也可能因重放完成顺序不同而失败。
 *
 * 所以 resume 重放的是**调度**，不只是答案：本闸持有首生的结算次序，命中缓存的结算在释放点
 * 挂起，直到它前面的每一条都已释放。次序表之外的实例直接放行——闸门只约束它有证据的那些，
 * 因此对旧 journal（事件早于本规则）退化成原行为，也不可能把一个它一无所知的 run 锁死。
 */

import { isArtifactPresetOp } from "../facade/registry.js";
import type { InstanceRef, JournalStorePort, NodeRecord, RunEvent } from "./types.js";
import { refToString } from "./types.js";

export class ReplaySettleOrder {
  /** 实例键 → 它在首生结算次序里的位置。 */
  private readonly position: ReadonlyMap<string, number>;
  /** 已释放到哪一位（次序表的游标）。 */
  private cursor = 0;
  /** 已到达、正在等自己那一位的释放动作。 */
  private readonly parked = new Map<string, () => void>();
  /** 已轮到、排在微任务里等着投递的释放动作（FIFO，见 {@link enqueue}）。 */
  private queue: Array<() => void> = [];
  private flushing = false;
  /** run 结算后闸门永久打开（见 {@link open}）。 */
  private opened = false;

  constructor(private readonly order: readonly string[] = []) {
    const position = new Map<string, number>();
    order.forEach((key, index) => position.set(key, index));
    this.position = position;
  }

  static empty(): ReplaySettleOrder {
    return new ReplaySettleOrder([]);
  }

  /** 首生的结算次序（用于诊断）。 */
  recorded(): readonly string[] {
    return this.order;
  }

  /**
   * 受次序约束地释放一次 replay 命中。表里没有这个实例（全新调用、旧 journal）、闸门已打开、
   * 或它的位次已被越过时立即执行；否则挂起，等轮到它。
   */
  hold(instance: InstanceRef, release: () => void): void {
    const key = refToString(instance);
    const at = this.position.get(key);
    if (this.opened || at === undefined || at < this.cursor) {
      release();
      return;
    }
    this.parked.set(key, release);
    this.pump();
  }

  /**
   * 投递永远晚于**产生这个 promise 的那次调用**：释放动作先进队列，一个微任务之后才执行。
   *
   * 少了这一跳，次序就是反的：`Promise.all` 里每条分支都在同一个同步片里发起自己的调用，
   * 队首那条的释放会**在它自己的 `ask()` 里**同步发生——那时它的 `await` 还没挂上去，于是
   * 它的续体排在「早先已挂上、刚被这次 pump 兑现」的那条后面。微任务这一跳让所有 `await`
   * 先挂稳，投递次序因此就是释放次序。这不是时钟：跳的是确定的一步，队列是 FIFO。
   */
  private enqueue(release: () => void): void {
    this.queue.push(release);
    if (this.flushing) return;
    this.flushing = true;
    queueMicrotask(() => this.flush());
  }

  /** 把排好的投递按 FIFO 执行完（队列空即空跑，故重复调用安全）。 */
  private flush(): void {
    this.flushing = false;
    const batch = this.queue;
    this.queue = [];
    for (const release of batch) release();
  }

  /**
   * run 结算：闸门永久打开，挂起的按记录次序放完。
   *
   * 不放的后果是脚本那侧的 promise 永远不兑现——在 harness 里沙箱马上就被关掉，但在同进程
   * 跑脚本的装配（如 `EvalWorkflowSnippet`）里那就是一次挂死。结算之后释放是安全的：
   * 引擎已 markSettled，每条 host 路径都以 `isRunSettled()` 开头。
   */
  open(): void {
    if (this.opened) return;
    this.opened = true;
    // 已排队的先按次序放完：结算路径在 `run-settled` **之前**调用本方法，同步放完这一批，
    // 命中事件因此不会落到那条终态事件后面。此刻每个挂起项的 await 早就挂稳了，不需要再跳。
    this.flush();
    for (let i = this.cursor; i < this.order.length; i++) {
      const key = this.order[i];
      if (key === undefined) continue;
      const release = this.parked.get(key);
      if (release === undefined) continue;
      this.parked.delete(key);
      release();
    }
    this.cursor = this.order.length;
    // 次序表之外的键从不入 parked，这一轮理论上是空的；兜底清空，绝不留下没人兑现的 promise。
    const leftovers = [...this.parked.values()];
    this.parked.clear();
    for (const release of leftovers) release();
  }

  /** 游标能往前走多少就走多少：队首已到达即释放，然后看下一位。 */
  private pump(): void {
    while (this.cursor < this.order.length) {
      const key = this.order[this.cursor];
      if (key === undefined) break;
      const release = this.parked.get(key);
      if (release === undefined) break;
      this.parked.delete(key);
      this.cursor++;
      this.enqueue(release);
    }
  }
}

/**
 * 从 run 自己的事件日志恢复首生的结算次序。
 *
 * 事实来源是事件而不是新列：结算次序**本来就**逐条记在 `dwf_event` 里，而它与 journal 行同一次
 * 写入落库；读回来既不用迁移，也让本规则之前跑出来的 journal 立刻可 resume（本地库里 258 个 run
 * 有 54 个正卡在这个形状上）。同一条路 `recoverImportClosure` 已经走过（零 schema 变更地从事件
 * 次序恢复关门判定）。
 *
 * 只收**每个实例首次**结算的那一条：重放的一世会按本闸释放的次序再发一遍 cached 事件，取首次
 * 因而跨世稳定，第三世恢复出的仍是同一张表。
 *
 * 表里只留「重放时会兑现一个 promise 的终态行」：`report` 是 void、预置产物声明是同步的，两者
 * 永远不会来认领，留在表里就会把游标堵死；仍是 `running` 的行按重新 live 执行处理，也不认领。
 */
export function recoverSettleOrder(
  journal: JournalStorePort,
  runId: string,
  /** 已读好的节点行（引擎的 resume 分支本来就要读一次，传进来省掉第二次全表读）。 */
  nodes: readonly NodeRecord[] = journal.listNodes(runId),
): ReplaySettleOrder {
  const claimable = new Set<string>();
  for (const node of nodes) {
    if (node.status !== "completed" && node.status !== "failed") continue;
    if (!claimsOnReplay(node)) continue;
    claimable.add(refToString(node));
  }
  if (claimable.size === 0) return ReplaySettleOrder.empty();

  const order: string[] = [];
  const seen = new Set<string>();
  for (const { event } of journal.listEvents(runId)) {
    const key = settledInstanceKey(event);
    if (key === undefined || seen.has(key) || !claimable.has(key)) continue;
    seen.add(key);
    order.push(key);
  }
  return new ReplaySettleOrder(order);
}

/** 结算事件 → 它结算的实例键；不是结算事件则 undefined。 */
function settledInstanceKey(event: RunEvent): string | undefined {
  if (
    event.type === "node-settled" ||
    event.type === "artifact-published" ||
    event.type === "artifact-failed"
  ) {
    return refToString(event.instance);
  }
  return undefined;
}

/**
 * 这一行在 replay 时会不会兑现一个脚本能等的 promise。
 * ask / world 节点会；`report` 不会（void）；产物行要看成员族——内容成员是效应（async），
 * 预置声明是同步的，只有前者会来认领闸门。失败的产物行只可能来自内容成员（声明没有拒绝通道）。
 */
function claimsOnReplay(node: NodeRecord): boolean {
  if (node.kind === "report") return false;
  if (node.kind !== "artifact") return true;
  if (node.status === "failed") return true;
  const kind = (node.result as { kind?: unknown } | undefined)?.kind;
  return typeof kind === "string" && !isArtifactPresetOp(kind);
}

/**
 * 受次序闸约束地兑现一次 replay 命中：轮到它时才执行 `settle` 并按其结果兑现 / 拒绝。
 * 给的是 promise 路径（world 节点与内容产物）；ask 的释放本就是回调，直接用 {@link ReplaySettleOrder.hold}。
 */
export function heldResolution<T>(
  hold: (instance: InstanceRef, release: () => void) => void,
  instance: InstanceRef,
  settle: () => T,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    hold(instance, () => {
      try {
        resolve(settle());
      } catch (cause) {
        reject(cause);
      }
    });
  });
}
