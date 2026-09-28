// Dynamic Workflow Run Port：工作区操作记录的读取类型。
// 包含 `files.*`、`git.*` 和 `world.run` 调用的清单行与正文，
// 由 dynamic-workflow-run.port.ts 统一再导出，调用方通过 `@zcode/contracts` 使用。

// 结构化失败的形状留在主文件上（本组的两个字段引用它），所以这里反向 import 一个类型：
// 纯类型、无运行时边，两个文件各自只描述自己那一组。
import type { DynamicWorkflowRunError } from "./dynamic-workflow-run.port.js";

/** 工作区节点的种类：journal `dwf_node.kind` 的两个 world 值。 */
export type DynamicWorkflowRunWorkspaceNodeKind = "world-read" | "world-run";

/** 节点行的状态，= journal 的 `NodeRecordStatus`（刻意在这里重申，理由同 lifecycle status）。 */
export type DynamicWorkflowRunWorkspaceNodeStatus = "running" | "completed" | "failed";

/**
 * 清单上一行的**摘要**：不把正文解出来就能报的那几个数。由存储层用 SQLite 的 JSON 函数在
 * 查询里算出（`resultBytes` / `resultCount` / `exitCode` / `stdoutBytes` / `stderrBytes`），
 * 端口原样透传。哪个字段在场取决于 op：数组正文（glob / grep / changedFiles）有 `resultCount`，
 * `world.run` 有 exitCode 与两路输出的字节数，字符串正文只有 `resultBytes`。
 */
export interface DynamicWorkflowRunWorkspaceNodeSummary {
  /** 正文序列化后的 UTF-8 字节数。 */
  resultBytes: number;
  resultCount?: number;
  exitCode?: number;
  stdoutBytes?: number;
  stderrBytes?: number;
}

/**
 * 工作区 transcript 的一行：一次 `files.*` / `git.*` / `world.run` 调用，**不带正文**。
 *
 * `op` / `args` 来自迁移 0030 加的 `input_json`（admission 时写下、≤ 4 KB）；升级前的历史行
 * 两者缺席，UI 退回静态图上的步标签。`inputTruncated` 表示 args 是逐项字符串预览而不是原值。
 */
export interface DynamicWorkflowRunWorkspaceNode {
  siteId: string;
  ordinal: number;
  kind: DynamicWorkflowRunWorkspaceNodeKind;
  op?: string;
  args?: readonly unknown[];
  inputTruncated?: true;
  status: DynamicWorkflowRunWorkspaceNodeStatus;
  /** failed 行的结构化失败（journal `error_json` 的 code + message；其余字段不出端口）。 */
  error?: DynamicWorkflowRunError;
  /** 结算成功的行才有。 */
  summary?: DynamicWorkflowRunWorkspaceNodeSummary;
  /** journal 行的建立 / 最近更新时刻（epoch 毫秒）；二者之差就是这一步的耗时。 */
  createdAt: number;
  updatedAt: number;
}

/** {@link import("./dynamic-workflow-run.port.js").DynamicWorkflowRunPort.readWorkspaceNodeResult} 的分页袋：正文的字节上限。 */
export interface DynamicWorkflowRunWorkspaceNodeResultQuery {
  /** 必填；端口按它**有界化**正文（截断而不是拒绝——这是审计面，不是脚本的取数面）。 */
  maxBytes: number;
}

/**
 * 一个工作区节点的正文：按形状有界化过的 `result`。
 *
 * 截断是**保形**的：字符串切尾、数组去尾、`world.run` 的 stdout / stderr 各自切尾，
 * `truncated` 说明发生过截断，`totalBytes` 是截断前的字节数。running 行没有正文；failed 行
 * 只有 `error`。
 */
export interface DynamicWorkflowRunWorkspaceNodeResult {
  status: DynamicWorkflowRunWorkspaceNodeStatus;
  result?: unknown;
  error?: DynamicWorkflowRunError;
  truncated: boolean;
  totalBytes: number;
}
