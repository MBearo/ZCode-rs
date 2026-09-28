// ============================================================
// amend-resume 的导入缓存
// ============================================================
// 从 engine/types.ts 拆出（该文件已到 oxlint max-lines 上限）：纯数据结构，run service 读前驱
// journal 构建，引擎只查表。

import type { AskStats, NodeKind, PersonaSpec } from "./types.js";

/**
 * 一条可导入的已完结 ask：前驱 run 里某具名 actor 第 `actorSeq` 次问答的结果。
 * 数组下标即 actorSeq（见 {@link ImportedActorCandidate.entries}），所以这里不存 seq。
 */
export interface ImportedAskEntry {
  /** 前驱记录的 inputHash（对指令正文）。运行期与本次 ask 的哈希逐条比对，一致才命中。 */
  inputHash: string;
  result: unknown;
  stats?: AskStats;
  /**
   * 该 ask 结算后源会话的消息数（{@link NodeRecord.messageBoundary}）。**必填**：
   * run service 的「无 marker 前驱整体拒绝」门保证每条可导条目都带边界，所以引擎这边
   * 不必有缺席分支——种子截断没有边界就无从谈起。
   */
  messageBoundary: number;
}

/**
 * 前驱停下时**还在飞**的那一条 ask（`actorSeq === entries.length`，紧接前缀之后的 running 行）。
 *
 * 它没有结果可导入，导入的是**它已经跑出来的那段转录**：修订若在同一位置重发同一条指令
 * （`inputHash` 相符），新会话就从这里接着跑，而不是把那半场对话扔掉重来。
 */
export interface ImportedInFlightAsk {
  /** 前驱记录的 inputHash（对指令正文）。与本次 ask 的哈希相符才谈得上续跑。 */
  inputHash: string;
  /**
   * 前驱**整个已结算会话**的消息数（不是某一条 ask 的记账边界——未完结的 ask 没有边界可记）。
   * 它既含前缀那些完整交换，也含这半场未完的问答，正是续跑要接上的位置。
   */
  messageBoundary: number;
}

/**
 * 前驱 run 里一个具名 actor 的可导入前缀。由 run service 从前驱 journal 构建（纯确定，可重建）。
 */
export interface ImportedActorCandidate {
  /** 前驱记录的规范化 persona——运行期 createActor 比对用（不一致即弃该候选）。 */
  persona: PersonaSpec;
  /**
   * 最长全 completed ask 前缀，按 actorSeq 0..n-1 索引。**可以为空**：带 {@link inFlight} 的候选
   * 常常一条都没做完（扇出第一轮在飞时被修订，正是这个形状）。
   */
  entries: ImportedAskEntry[];
  /**
   * 前驱停下时还在飞的那条 ask（若有）。只在转录源就是前驱自己那一行时导入——那半场对话只
   * 存在于前驱的会话里，从更早祖先解析出的源只有完整前缀。
   */
  inFlight?: ImportedInFlightAsk;
  /**
   * 经 `resumed_from` 链解析出的转录源会话 id。service 保证在场——链上没有任何祖先
   * 持有该 actor 会话的候选在 service 侧就已弃置（降级为全新 actor），所以这里不是可选。
   */
  transcriptSourceSessionId: string;
  /**
   * 前驱解析出的模型 pin，随种子带给 driver。**仅当真的导入了转录时生效**：一条也没命中的
   * actor 走全新解析、不带 pin（见 scheduler 的 ensureSession）。
   */
  resolvedModel?: string;
}

/** 一条可导入的世界节点（world-read / world-run），按内容 + 出现序匹配。 */
export interface ImportedWorldEntry {
  inputHash: string;
  kind: NodeKind;
  result: unknown;
}

/**
 * 注入引擎的导入缓存（amend-resume 的加速结构）。**纯数据**：引擎保持零 I/O，这张表由
 * run service 读前驱 journal 构建。
 * 它不是真相源——每次命中都落一行真 dwf_node，丢了可从 `resumed_from` 重建。
 */
export interface ImportedRunCache {
  /** 键 = 有效 actor 名（前驱内唯一且非空的那些）。 */
  actors: ReadonlyMap<string, ImportedActorCandidate>;
  /**
   * 键 = `inputHash({op, args})`；值 = 按前驱 listNodes 插入序排好的队列——同一个
   * `{op,args}` 的第 n 次出现对第 n 条记录，队列头即下一次命中。
   */
  world: ReadonlyMap<string, ImportedWorldEntry[]>;
}

/**
 * 会话种子：分歧 actor 首次 live 派发时交给 {@link WorkflowDriver.createActorSession}，
 * 让新会话以源会话的**全保真转录前缀**开场。
 */
export interface ActorSessionSeed {
  /** 转录来源会话（前驱或更早祖先的该名 actor 会话）。 */
  sourceSessionId: string;
  /**
   * 复制源会话前多少条消息 = 最后一条被消费导入 ask 的 {@link NodeRecord.messageBoundary}。
   * count offset 跨前缀复制不变，所以这个值在链上任何持会话祖先处都直接可用。
   */
  messageCount: number;
  /** 承袭的模型 pin（转录接续下静默换模型正是 pin 要防的身份突变）。 */
  resolvedModel?: string;
}
