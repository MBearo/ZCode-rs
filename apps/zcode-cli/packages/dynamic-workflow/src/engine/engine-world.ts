/**
 * engine.ts 顶到 oxlint max-lines 上限（400 行），把 world 节点（world-read / world-run）
 * 的准入-结算路径与 amend-resume 导入缓存的关门 / 恢复接缝拆到本文件；公开面仍从 engine.ts 导出。
 *
 * 自由函数经 {@link EngineState} 接缝读写引擎状态；WorkflowEngine.worldRead 只是薄委托，
 * 关门由调度器经 SchedulerHost 触发，恢复在引擎构造函数的 resume 分支里调用。
 */

import { canonicalJson, inputHash } from "./hash.js";
import { heldResolution } from "./replay-order.js";
import { boundWorldReadInput } from "./world-read-input.js";
import { hashMismatch } from "./scheduler.js";
import type { EngineState } from "./engine-state.js";
import type {
  InstanceRef,
  JournalStorePort,
  NodeKind,
  WorkflowErrorJson,
  WorldReadInput,
  WorldReadOp,
} from "./types.js";
import { refToString, WorkflowError } from "./types.js";

export function readWorld(
  state: EngineState,
  siteId: string,
  op: WorldReadOp,
  args: unknown[],
): Promise<unknown> {
  if (state.isRunSettled()) return Promise.reject(state.runError());
  const ordinal = state.nextOrdinal(siteId);
  const instance: InstanceRef = { siteId, ordinal };
  // inputHash 覆盖 `{op, args}`（而非单实参的 `{op, arg}`）。这是**有意的载荷形变**：
  // 旧 journal（多参 world-read 之前写下的）里，每个 world-read 命中都会 inputHash 不符而让 run 以
  // InputHashMismatch 大声失败。可接受——v1 的 resume 本就要求脚本文本逐字节相同
  // （script_hash），facade 表面早于多参 world-read 的脚本不会是要被 resume 的那个脚本。
  // 写在这里是因为故障点在 resume，离这一行很远。
  const hash = inputHash({ op, args });
  // 有界的 `{op, args}`：与 hash 同一次准入写下，
  // 并在结算的 upsert 里原样带过去——putNode 是整条替换，漏一处就把它抹回 NULL。
  const input = boundWorldReadInput(op, args);

  const recorded = state.journal.getNode(state.runId, siteId, ordinal);
  if (recorded !== undefined) {
    if (recorded.inputHash !== hash) {
      const err = hashMismatch(instance, recorded.inputHash, hash);
      state.failRun(err);
      return Promise.reject(err);
    }
    // 完结命中短路（journal 化世界读取使 resume 免疫于 run 与 resume 之间的磁盘变化）。
    // 释放点过 replay 次序闸：一条扇出分支里的 world 读取同样是别人的续体在等的东西，
    // 按准入顺序放会把 join 之后的序号错位。
    if (recorded.status === "completed") {
      return heldResolution(state.holdForReplay, instance, () => {
        state.record({ type: "node-settled", instance, outcome: "ok", cached: true });
        return recorded.result;
      });
    }
    if (recorded.status === "failed") {
      return heldResolution(state.holdForReplay, instance, () => {
        state.record({
          type: "node-settled",
          instance,
          outcome: "failed",
          cached: true,
          error: recorded.error,
        });
        throw WorkflowError.fromJSON(recorded.error!);
      });
    }
    // status === "running"：崩溃于执行中，落到下面重新 live 执行。
  }

  // op "run" 是效应而不是读，journal 单列一种（world-run）：机制同构，审计面诚实。其余 op 保持 world-read。
  const kind: NodeKind = op === "run" ? "world-run" : "world-read";

  // amend-resume：本 run 的 journal 里没有这一行，才轮到导入缓存（journal replay 永远优先——
  // 修订 run 自己崩溃后 resume 时，已消费的命中早已是真行，重放它们不能再动游标）。
  // 命中即写一行与 settleWorldRead 同形的 completed 记录 + 发 node-settled(cached)，
  // **不调 driver**：world-run 的效应绝不静默重放。
  // 缓存关闭后不再问表（也不推进游标）：命令文本没变，但它读到的世界可能已被某个 live 子代理
  // 改写。
  const imported = state.importClosed() ? undefined : state.importedWorld.take(hash);
  if (imported !== undefined) {
    state.journal.putNode({
      runId: state.runId,
      siteId,
      ordinal,
      kind,
      inputHash: hash,
      input,
      status: "completed",
      result: imported.result,
    });
    state.record({ type: "node-settled", instance, outcome: "ok", cached: true });
    return Promise.resolve(imported.result);
  }

  // live world-read：不受 actor FIFO / 并发上限约束（harness IO）。
  if (state.isRunSettled()) return Promise.reject(state.runError());
  // live 的 world.run 是一笔写入（效应，不是读）：它一旦执行，工作区就与前驱留下的不同，所以在
  // 派发**之前**关导入缓存——与子代理的改写工具同一条规则、同一个时刻（动手前）。world-read 不关：
  // 读不改变世界。
  if (kind === "world-run") closeImportCache(state, instance, "world-run");
  // 准入即落 running：崩溃于执行中的世界读取 resume 可据此重新执行。
  state.journal.putNode({
    runId: state.runId,
    siteId,
    ordinal,
    kind,
    inputHash: hash,
    input,
    status: "running",
  });
  state.record({ type: "node-queued", instance, kind });
  state.record({ type: "node-dispatched", instance });
  return state.driver.executeWorldRead(op, args).then(
    (value) =>
      settleWorldRead(state, instance, kind, hash, input, { status: "completed", result: value }),
    (cause: unknown) => {
      const err =
        cause instanceof WorkflowError
          ? cause
          : new WorkflowError("DriverError", `World read failed: ${op} ${canonicalJson(args)}.`, {
              cause,
            });
      return settleWorldRead(state, instance, kind, hash, input, {
        status: "failed",
        error: err.toJSON(),
      });
    },
  );
}

function settleWorldRead(
  state: EngineState,
  instance: InstanceRef,
  // putNode 是 upsert（整条替换），settle 若写死 world-read 会把准入时的 world-run
  // 悄悄改回去——kind 必须与准入同源。
  kind: NodeKind,
  hash: string,
  // 同一条理由：准入写下的有界输入必须随结算的整条替换回来。
  input: WorldReadInput,
  outcome:
    | { status: "completed"; result: unknown }
    | { status: "failed"; error: WorkflowErrorJson },
): unknown {
  if (state.isRunSettled()) throw state.runError();
  state.journal.putNode({
    runId: state.runId,
    siteId: instance.siteId,
    ordinal: instance.ordinal,
    kind,
    inputHash: hash,
    input,
    status: outcome.status,
    ...(outcome.status === "completed" ? { result: outcome.result } : { error: outcome.error }),
  });
  if (outcome.status === "completed") {
    state.record({ type: "node-settled", instance, outcome: "ok" });
    return outcome.result;
  }
  state.record({ type: "node-settled", instance, outcome: "failed", error: outcome.error });
  throw WorkflowError.fromJSON(outcome.error);
}

/**
 * 关闭导入缓存（幂等）。两个触发点，都是**第一笔写入之前**：
 * driver 上报某个 live 子代理即将执行改写工具（`mutating-tool`），或一条 `world.run` 要 live 执行
 * （`world-run`）。只在本次真是修订 run 时发 `import-cache-closed` 事件——它是 resume 时恢复
 * 「门已关」的唯一事实来源；非修订 run 没有表可关，标志位无害但不发事件。
 */
export function closeImportCache(
  state: EngineState,
  instance: InstanceRef,
  cause: "mutating-tool" | "world-run",
  actorName?: string,
): void {
  if (state.importClosed()) return;
  state.closeImport();
  if (state.importedCache === undefined) return;
  state.record({
    type: "import-cache-closed",
    instance,
    cause,
    ...(actorName === undefined || actorName === "" ? {} : { actorName }),
  });
}

/**
 * resume 时恢复关门判定、「曾 live 的 ask 实例」集合，以及「关门之前就已准入的 ask 实例」集合。
 *
 * 事实来源是本 run 自己的事件：live 节点在准入时发 `node-queued`（ask 的带 actor ref），缓存
 * 命中只发 `node-settled cached:true`，所以「哪些 ask 曾 live」是精确集合。门是否已关则由
 * `import-cache-closed` 事件决定——ask 转 live 不再意味着关门（它可能一个文件都没碰），只有
 * 写入才关，而写入这件事只有这条事件记着。零 schema 变更。
 *
 * `queuedBeforeClose` 同样零 schema 变更，靠的是**事件次序**：续跑在飞 ask 的判定与该 ask 的
 * `node-queued` 落在准入的同一个同步片里（见 scheduler 的 admitAsk → tryImportedSettle →
 * admitLive），所以「准入那一刻门开着」⇔「这条 node-queued 早于第一条 import-cache-closed」。
 * 没有关门事件时全体都算在内。
 */
export function recoverImportClosure(
  journal: JournalStorePort,
  runId: string,
): { live: ReadonlySet<string>; queuedBeforeClose: ReadonlySet<string>; closed: boolean } {
  const live = new Set<string>();
  const queuedBeforeClose = new Set<string>();
  let closed = false;
  for (const { event } of journal.listEvents(runId)) {
    if (event.type === "import-cache-closed") closed = true;
    if (event.type !== "node-queued" || event.kind !== "ask") continue;
    const key = refToString(event.instance);
    live.add(key);
    if (!closed) queuedBeforeClose.add(key);
  }
  return { live, queuedBeforeClose, closed };
}
