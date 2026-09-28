// ============================================================
// publisher 对 `workflowRun.*` 键级增量的两条纯规则：旧消费者编码、以及快路径的增长上界
// ============================================================
// 住在 publisher 之外的理由与它们是纯函数同样重要：两条规则都需要单独钉住，而 publisher 是
// 带订阅注册表和保留日志的运行时外壳，在它身上试这两件事要先搭半个传输层。
//
// ── 一、旧消费者编码 ──
// 保留日志里存的一律是原生 op（一份事实、一份日志），编码是**每订阅者**的事：握手时带了
// `workflowRunDeltas` 的连接直接收增量，没带的那一代客户端连 op 的判别式都不认——
// `conversationDeltaSchema` 是 discriminatedUnion，未知 `op` 让整条 union 解析失败、整个
// logical frame 被组装器丢掉，而重发的快照里同样没有这条 op，于是那条订阅从此静默。
// 所以这里不是「优化」，是那一代客户端能不能继续看见 run 的分界线。
//
// 改写规则只有一条：整批里所有 `workflowRun.*` 一律丢掉，在**最后一条**被丢掉的位置上补一条
// `state.updated{workflowRuns: 当前投影}`。为什么是最后一条的位置而不是第一条：整键替换蕴含
// 这批增量的全部效果，放在最后一条的位置上，它与批内其余 op 的相对顺序就与逐条投递一致
// （行 op 与状态键互不相干，本来也可交换）。为什么带的是**当前**投影而不是历史中间态：
// 恢复回放时日志里的那条增量早就不是最新事实了，而客户端要的是终态——中间态被跳过与
// coalesce 每天在做的事情是同一件。
//
// 裁到旧界（256）由 `clampWorkflowRunsForLegacy` 负责，理由见它的文件头。

import type { ConversationDelta, WorkflowRunsState } from "@zcode/shared/zcode-protocol-v4";
import { clampWorkflowRunsForLegacy, utf8JsonByteLength } from "@zcode/shared/zcode-protocol-v4";

function isWorkflowRunDelta(delta: ConversationDelta): boolean {
  return delta.op === "workflowRun.updated" || delta.op === "workflowRun.removed";
}

/**
 * 一批 delta 的旧消费者编码。批内没有 `workflowRun.*` 时**原样返回同一个数组**
 * （这条路径在每次 ingest 的每个订阅者上都会跑一遍，不该为不相干的批次造新数组）。
 *
 * `workflowRuns` 缺席时只丢不补：投影里还没有这个键，就没有能整键替换成的东西。
 * 产出侧不会走到（有增量就必有状态），这里只是不让它变成一条 `{workflowRuns: undefined}`。
 */
export function encodeConversationDeltasForLegacy(
  deltas: readonly ConversationDelta[],
  workflowRuns: WorkflowRunsState | undefined,
): readonly ConversationDelta[] {
  let lastIndex = -1;
  for (let index = deltas.length - 1; index >= 0; index -= 1) {
    if (isWorkflowRunDelta(deltas[index]!)) {
      lastIndex = index;
      break;
    }
  }
  if (lastIndex < 0) return deltas;

  const replacement: ConversationDelta | null =
    workflowRuns === undefined
      ? null
      : { op: "state.updated", patch: { workflowRuns: clampWorkflowRunsForLegacy(workflowRuns) } };
  const encoded: ConversationDelta[] = [];
  for (let index = 0; index < deltas.length; index += 1) {
    const delta = deltas[index]!;
    if (!isWorkflowRunDelta(delta)) {
      encoded.push(delta);
      continue;
    }
    if (index === lastIndex && replacement !== null) encoded.push(replacement);
  }
  return encoded;
}

// ── 二、ingest 快路径的增长上界 ──
//
// 今天每条 dwf 引擎事件都走精确路径：克隆投影 + 把整份 wire snapshot `JSON.stringify` 一遍来
// 对 16MiB 闸门。一条 run 上千条事件，这一项本身就是 MB 级的重复开销，与整键重发是同一笔账的
// 两半。键级增量让「这一步长了多少」有了廉价而**可靠**的上界，于是绝大多数事件不必再量整份。

/** JSON 外壳（`{"kind":"deltas","deltas":[…]}` 与帧信封）留的余量，与流式追加快路径同值。 */
const WORKFLOW_RUN_DELTA_ENVELOPE_SLACK_BYTES = 64;

/**
 * 一批 delta **只含**键级增量时，快照增长的字节上界；含任何其它 op 时返回 null（必须精确测量）。
 *
 * 上界成立的理由：`workflowRun.updated` 写进快照的全部内容都在这条 op 的载荷里（header 键整体
 * 替换、条目整条 upsert），所以快照最多长它自己那么多；`workflowRun.removed` 只会让快照变小。
 *
 * **空批次**同样满足这个判据（没有一条不是键级增量），而且它的增长论证更强：一条不产 delta 的
 * 事件对快照的全部改动就是 `seq` 那个数字——余量绰绰有余。这一支不是特例，它就是判据读到底的
 * 结果，而且正是它让「归约判幂等、一条 delta 都不产」的重放事件不再为一次整份序列化买单。
 *
 * null 覆盖的那一半同样是契约的一部分：diff 认不出结构变化时会退化成整键 `state.updated`，
 * 那条 op 的字节数**不是**增长的上界（它替换的是整个键，旧值的字节不在其中），必须落回精确
 * 路径。判据因此是「每一条都是键级增量」，而不是「有键级增量」。
 *
 * 余量是**每批**一份而不是每条一份：这一份既盖 JSON 外壳，也盖 `seq` 十进制位数的增长。
 */
export function workflowRunDeltaGrowthUpperBound(
  deltas: readonly ConversationDelta[],
): number | null {
  for (const delta of deltas) {
    if (delta.op !== "workflowRun.updated" && delta.op !== "workflowRun.removed") return null;
  }
  return utf8JsonByteLength(deltas) + WORKFLOW_RUN_DELTA_ENVELOPE_SLACK_BYTES;
}
