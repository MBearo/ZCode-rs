// CreateWorkflow 的实时工具卡。
//
// 为什么不是 ToolTranscriptPart 上的几行 detailLines：工具行是**事件时刻的快照**，而
// CreateWorkflow 一把 run launch 出去就返回——工具行会在 run 还在飞的时候就变成 completed。
// 卡片因此必须在渲染时读镜像（join 按 toolCallId），状态取 run 的状态而不是工具行的状态。
//
// 视图是 props 的纯函数（TUI 测试按函数式调用组件，不起终端）。
import React from "react";
import type { TuiCopy } from "@zcode/i18n";
import type { WorkflowRunActor } from "@zcode/shared/zcode-protocol-v4";
import { palette } from "./app-model.js";
import { DEFAULT_TUI_COPY } from "./app-locale.js";
import { truncateDisplay } from "./app-terminal-width.js";
import type { TuiWorkflowCard } from "./app-workflow-mirror.js";

const CARD_DETAIL_INDENT = "  ";
const CARD_LOG_INDENT = "    ";
export const MAX_ACTOR_ROWS = 6;
const MAX_RESULT_PREVIEW_WIDTH = 200;

/** 六行位置按状态分桶：跑着的排前面。 */
const ACTOR_ROW_RANK: Record<WorkflowRunActor["status"], number> = {
  running: 0,
  waiting: 1,
  completed: 2,
};

/**
 * 卡片要显示的 actor 行。
 *
 * 为什么不是 `actors.slice(0, MAX_ACTOR_ROWS)`：`run.actors` 是协议顺序（出生序），一条宽 run
 * 里最先出生的六个往往都已结算，于是六行全是干完的人，正在跑的一个都看不见。改成按状态分桶
 * 挑选——running → waiting → completed，桶内仍按协议顺序（稳定，所以一个 actor 只在它自己换
 * 状态时才移动）。
 */
export function actorRowsForCard(
  actors: readonly WorkflowRunActor[],
  limit: number = MAX_ACTOR_ROWS,
): readonly WorkflowRunActor[] {
  return actors
    .map((actor, index) => ({ actor, index }))
    .sort(
      (left, right) =>
        ACTOR_ROW_RANK[left.actor.status] - ACTOR_ROW_RANK[right.actor.status] ||
        left.index - right.index,
    )
    .slice(0, limit)
    .map((entry) => entry.actor);
}

const h = React.createElement as (
  type: React.ElementType | string,
  props?: Record<string, unknown> | null,
  ...children: React.ReactNode[]
) => React.ReactElement;

export function WorkflowRunCardView({
  card,
  copy = DEFAULT_TUI_COPY,
  expanded = false,
  terminalWidth = 100,
}: {
  card: TuiWorkflowCard;
  copy?: TuiCopy;
  expanded?: boolean;
  terminalWidth?: number;
}): React.ReactElement {
  const workflowCopy = copy.transcript.workflow;
  const width = Math.max(20, terminalWidth - CARD_DETAIL_INDENT.length);
  const collapsedLine = workflowCopy.collapsed({
    // label 只有服务端知道（冷补种带回）；没有就退回 runId，绝不在这里造一个假名字。
    label: card.label ?? card.runId,
    status: workflowStatusLabel(card.status, workflowCopy, card.stopReason),
    nodesSettled: card.nodesSettled,
    nodesTotal: card.nodesTotal,
  });
  const hint = expanded ? workflowCopy.collapseHint : workflowCopy.expandHint;

  return h(
    "box",
    {
      style: {
        backgroundColor: "transparent",
        flexDirection: "column",
        marginTop: 1,
        width: "100%",
      },
    },
    h(
      "text",
      { style: { fg: colorForWorkflowStatus(card.status) } },
      truncateDisplay(`${collapsedLine}  [${hint}]`, terminalWidth),
    ),
    ...(expanded ? expandedDetailNodes(card, workflowCopy, width) : []),
  );
}

function expandedDetailNodes(
  card: TuiWorkflowCard,
  workflowCopy: TuiCopy["transcript"]["workflow"],
  width: number,
): React.ReactElement[] {
  const nodes: React.ReactElement[] = [];

  if (card.usage) {
    nodes.push(
      detailLine("usage", workflowCopy.usage({ spentTokens: card.usage.spentTokens }), width),
    );
  }

  if (card.actors.length > 0) {
    nodes.push(detailLine("actors-title", workflowCopy.actors, width));
    for (const [index, actor] of actorRowsForCard(card.actors).entries()) {
      nodes.push(
        h(
          "text",
          { key: `actor-${index}`, style: { fg: palette.muted } },
          truncateDisplay(
            `${CARD_LOG_INDENT}${workflowCopy.actorRow({
              name: actor.name ?? `${actor.siteId}#${actor.ordinal}`,
              status: actor.status,
            })}`,
            width,
          ),
        ),
      );
    }
  }

  if (card.logTail.length > 0) {
    nodes.push(detailLine("log-title", workflowCopy.log, width));
    for (const [index, line] of card.logTail.entries()) {
      nodes.push(
        h(
          "text",
          { key: `log-${index}`, style: { fg: palette.muted } },
          truncateDisplay(`${CARD_LOG_INDENT}${line}`, width),
        ),
      );
    }
  }

  if (card.resultPreview !== undefined) {
    nodes.push(
      detailLine(
        "result",
        workflowCopy.result(truncateDisplay(card.resultPreview, MAX_RESULT_PREVIEW_WIDTH)),
        width,
      ),
    );
  }

  if (card.error !== undefined) {
    nodes.push(
      h(
        "text",
        { key: "error", style: { fg: palette.danger } },
        truncateDisplay(`${CARD_DETAIL_INDENT}${workflowCopy.error(card.error)}`, width),
      ),
    );
  }

  if (card.truncated === true) {
    nodes.push(detailLine("truncated", workflowCopy.truncated, width));
  }

  return nodes;
}

function detailLine(key: string, text: string, width: number): React.ReactElement {
  return h(
    "text",
    { key, style: { fg: palette.muted } },
    truncateDisplay(`${CARD_DETAIL_INDENT}${text}`, width),
  );
}

function workflowStatusLabel(
  status: TuiWorkflowCard["status"],
  workflowCopy: TuiCopy["transcript"]["workflow"],
  stopReason?: TuiWorkflowCard["stopReason"],
): string {
  if (status === "running") return workflowCopy.status.running;
  if (status === "completed") return workflowCopy.status.completed;
  if (status === "errored") return workflowCopy.status.errored;
  // stopped 带原因词：`stopped (model error)`。
  if (status === "stopped") {
    return stopReason === undefined
      ? workflowCopy.status.stopped
      : `${workflowCopy.status.stopped} (${workflowCopy.stopReason[stopReason]})`;
  }
  return workflowCopy.status.pending;
}

function colorForWorkflowStatus(status: TuiWorkflowCard["status"]): string {
  if (status === "completed") return palette.success;
  // errored 是脚本故障（danger）；stopped 可恢复（muted）。
  if (status === "errored") return palette.danger;
  if (status === "running") return palette.accent;
  if (status === "stopped") return palette.muted;
  return palette.warning;
}
