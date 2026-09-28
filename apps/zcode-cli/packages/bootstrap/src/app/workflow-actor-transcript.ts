// ============================================================
// actor 会话转录的两件事：边界计数与种子截断复制（amend-resume）
// ============================================================
//
// 这两件事必须同住一个模块，因为它们共享一条**载荷性**的不变式：
//
//   边界 N ⇒ 复制源会话前 N 条消息，恰好得到「到该 ask 为止」的完整转录。
//
// 而它成立的唯一理由是：数消息与复制消息走**同一个存取面**（{@link ActorTranscriptStore.messages}），
// 也正是 core 的 `resumeFromStore` 重水化时读的那一个（core/src/runtime/methods/resume.ts）。
// 三个读者只要有一个换成别的口径（比如只数 active branch、或按 part 数计），边界在两个读者眼里
// 就是两个长度，截断出来的转录会多一条或少一条——而那正是「模型看见的上文」与「journal 记的边界」
// 悄悄错位的样子。所以：**要改计数口径，三处一起改**。
//
// count offset（而不是消息 id 区间）的关键性质是**前缀复制下不变**：复制进新会话后 id 全变、
// count 不变，于是被复制 ask 的边界值在新会话里原样有效——链式修订（B 从 A 抄、C 再从 B 抄）
// 的根基就是这一条。

import {
  createMessageId,
  createPartId,
  type Logger,
  type MessageId,
  type MessageInfo,
  type MessagePart,
  type MessageWithParts,
  type PartId,
  type SessionId,
} from "@zcode/contracts";
import { cloneMessageForFork, clonePartForFork } from "@zcode/core";
import { WorkflowError, type ActorSessionSeed } from "@zcode/dynamic-workflow";

/**
 * driver 侧需要的会话转录存取面：读一个会话的全部消息、写一条消息 / 一个 part。
 *
 * 结构上是 {@link import("@zcode/contracts").SessionStorePort} 的真子集，所以生产直接把
 * session store 传进来即可。窄化的理由与 run service 的 journal / task-link 端口同款：driver
 * 只需要这三个方法，声明成整个 store 会让"driver 依赖会话存储的全部能力"变成一句真话。
 */
export interface ActorTranscriptStore {
  messages(input: { sessionID: SessionId }): Promise<MessageWithParts[]>;
  saveMessage(input: MessageInfo): Promise<void>;
  savePart(input: MessagePart): Promise<void>;
}

/**
 * 一个 actor 会话当前**已持久化**的消息条数（ask 边界记账的值）。
 *
 * 只数落库的那些：driver 在一次交换结束时问这个数，而 runtime 的消息持久化在 turn 内就已 await
 * 完成（core 的 persistMessage / persistPart），所以此刻的落库量就是这次交换的全部产出。
 */
export async function countActorTranscript(
  store: ActorTranscriptStore,
  sessionId: SessionId,
): Promise<number> {
  return (await store.messages({ sessionID: sessionId })).length;
}

/**
 * 把源会话的前 `seed.messageCount` 条消息（连同各自的 part）复制进目标会话。
 *
 * 三条性质：
 *
 * 1. **前驱只读**。复制出去的每条消息、每个 part 都铸新 id——`message.id` / `part.id` 是全库主键，
 *    而 `saveMessage` 的 upsert 在 id 冲突时会把 `session_id` 改成新值（adapters 的
 *    messages.ts）。原样搬 id 不是"复制"，是把前驱的转录**搬走**。id 重铸随之要求 `parentID`
 *    与 part 内嵌锚点跟着重映射，这正是 core 的 fork 克隆器 {@link cloneMessageForFork} /
 *    {@link clonePartForFork} 已经做对的事（fork 与本函数是同一个动作：按值复制一段转录到另一个
 *    会话，child 用本地 id 续写），所以这里复用它们而不是写第二份。
 * 2. **幂等，且绝不写进一个已经开跑的会话**。两条跳过规则，缺一不可：
 *
 *    - 目标已经有 ≥ messageCount 条消息即整段跳过：那是修订 run 崩溃后 resume 的情形——会话 id
 *      由 (runId, actorRef) 纯确定地铸出，重挂拿到的就是那个已经装着「复制的 + 新产的」内容的
 *      会话，再抄一遍等于把上文翻倍。
 *    - 目标里但凡有一条**不是**本会话种子 id 的消息（见 {@link seededMessageId}），同样跳过。
 *      只装着种子消息的会话是一截「复制到一半」的前缀，照旧补齐；装着别的东西的会话有自己的
 *      历史，一个字节都不许动。
 *
 *    第二条用于防止**边界增长后再次复制**：`inFlight.messageBoundary` 取自
 *    前驱会话此刻的消息条数，是导入缓存里唯一一个不是 journal 事实的数（见
 *    dynamic-workflow-import.ts 的文件头）。它在两次构建之间变大时（前驱被重新 resume 过又写了
 *    几轮，或者一条迟到的后台通知消息落了进去），修订 run 的一次普通「停止 → resume」就会带着
 *    更大的 M 再次调到这里；若此时目标里已有自己的 live 消息但总数仍 < M，老规则会去重抄
 *    0..M-1——前 N 条是对既有种子 id 的 upsert（无害），而 N..M-1 是**新 id**，于是前驱的消息被
 *    追加到本会话自己的历史**之后**（`message.sequence` 在 insert 时取 `max+1`，adapters 的
 *    messages.ts）。那是一段读起来前后颠倒、且不属于这个子代理的上文，而且悄无声息。
 *    「只有全是种子 id 才动它」把这件事在**复制点**一次性堵死，对将来任何一个非 journal 事实
 *    都成立，不必逐个去证明它们不会变大。
 * 3. **缺料即大声失败**。源会话不存在（读回空）或短于边界，说明 service 从 journal 事实构造出的
 *    种子这个 store 兑现不了——corruption 级，不是可降级情形（可降级的那一半在 service 侧的门里：
 *    链上缺会话的候选在那里就该被弃置）。
 *
 * @returns 实际复制的条数；`undefined` 表示跳过（两条规则都归到这一个返回值，因为调用方对
 *          两者的处理相同：不再水化第二次，见 workflow-driver-transcript.ts 的 seedActorSession）。
 */
export async function seedActorTranscript(input: {
  logger?: Logger;
  seed: ActorSessionSeed;
  store: ActorTranscriptStore;
  targetSessionId: SessionId;
}): Promise<number | undefined> {
  const { logger, seed, store, targetSessionId } = input;
  const existing = await store.messages({ sessionID: targetSessionId });
  if (existing.length >= seed.messageCount) return undefined;
  if (!holdsOnlySeedMessages(existing, targetSessionId)) {
    // 记一条：走到这里说明种子边界比上一次大了，而这是唯一能看见它的地方。
    logger?.warn?.("Dynamic workflow actor session already has its own history; seeding skipped", {
      event: "dynamic_workflow.actor.seed_skipped_live_session",
      existingMessageCount: existing.length,
      messageCount: seed.messageCount,
      module: "bootstrap.app",
      sessionId: targetSessionId,
    });
    return undefined;
  }

  const source = await store.messages({ sessionID: seed.sourceSessionId as SessionId });
  if (source.length < seed.messageCount) {
    throw new WorkflowError(
      "DriverError",
      `Cannot seed the subagent transcript: source session ${seed.sourceSessionId} has only ` +
        `${source.length} messages, but the boundary requires the first ${seed.messageCount}.`,
      { mismatch: { expected: String(seed.messageCount), got: String(source.length) } },
    );
  }

  // 老 id → 新 id：assistant 的 parentID 与 part 的内嵌锚点都按它重映射。前缀是连续的，
  // 所以每条消息引用到的更早消息必然已经在表里（下标严格递增）。
  const messageIds = new Map<MessageId, MessageId>();
  for (let index = 0; index < seed.messageCount; index++) {
    const message = source[index]!;
    const nextMessageId = seededMessageId(targetSessionId, index);
    const cloned = cloneMessageForFork(message.info, {
      forkedSessionId: targetSessionId,
      messageIdMap: messageIds,
      nextMessageId,
    });
    messageIds.set(message.info.id, nextMessageId);
    await store.saveMessage(cloned);
    for (const [partIndex, part] of message.parts.entries()) {
      await store.savePart(
        clonePartForFork(part, {
          forkedSessionId: targetSessionId,
          messageIdMap: messageIds,
          nextMessageId,
          nextPartId: seededPartId(targetSessionId, index, partIndex),
        }),
      );
    }
  }
  return seed.messageCount;
}

/**
 * 目标会话里是不是**只有**本会话的种子副本（第 i 条恰好是 {@link seededMessageId} 的第 i 个）。
 *
 * 判据用 id 而不是条数或时间：种子 id 按 (目标会话, 下标) 纯确定，所以「这条消息是不是我抄进来
 * 的」有一个不依赖任何时钟、也不依赖读取顺序之外任何东西的答案。空会话按真处理（还没抄过，
 * 当然可以抄）。`messages()` 按 `sequence` 升序返回，而种子是按下标顺序写的，所以下标就是位置。
 */
function holdsOnlySeedMessages(
  existing: readonly MessageWithParts[],
  targetSessionId: SessionId,
): boolean {
  return existing.every(
    (message, index) => message.info.id === seededMessageId(targetSessionId, index),
  );
}

/**
 * 种子副本的 id：按 (目标会话, 前缀下标) 纯确定。
 *
 * 确定性买到的是**半截复制的可修复性**：复制到一半崩溃，重挂时既有行会被同一个 id upsert 回去，
 * 而不是在旁边再长出一份。会话 id 已经含 runId 与 actorRef，所以两个不同目标会话的副本天然不撞。
 */
function seededMessageId(targetSessionId: SessionId, index: number): MessageId {
  return createMessageId(`${targetSessionId}-seed-${index}`);
}

function seededPartId(targetSessionId: SessionId, index: number, partIndex: number): PartId {
  return createPartId(`${targetSessionId}-seed-${index}-${partIndex}`);
}
