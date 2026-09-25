import assert from "node:assert/strict";
import test from "node:test";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import type { ServerResponse } from "node:http";
import {
  v4ConversationUsageResultSchema,
  v4UsageStatsResultSchema,
} from "@zcode/shared/zcode-protocol-v4";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { ProjectId, SessionId, ToolCallId, TurnId, WorkspaceId } from "@zcode/contracts";
import { end, event, fixture } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
type Harness = ReturnType<Awaited<ReturnType<typeof fixture>>["start"]>;

function readCall(res: ServerResponse) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, {
    tool_calls: [
      {
        index: 0,
        id: "call-read",
        type: "function",
        function: { name: "Read", arguments: JSON.stringify({ file_path: "a.txt" }) },
      },
    ],
  });
  end(res, "tool_calls");
}

function answer(res: ServerResponse) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content: "done" });
  end(res, "stop");
}

async function rejection(promise: Promise<unknown>): Promise<Message> {
  try {
    await promise;
  } catch (error) {
    return error as Message;
  }
  throw new Error("expected a rejection");
}

const stats = (h: Harness, params: Message) =>
  h.client.request("v4/usage/stats", params, v4UsageStatsResultSchema);
const conversation = (
  h: Harness,
  method: "v4/conversation/usage" | "session/usage",
  sessionId: string,
) => h.client.request(method, { sessionId }, v4ConversationUsageResultSchema);

test("Rust records usage of a turn and answers the usage queries like Node", async () => {
  const f = await fixture({
    respond(_req, res) {
      if (f.requests.length === 1) readCall(res);
      else answer(res);
    },
  });
  try {
    await writeFile(join(f.cwd, "a.txt"), "hello\n");
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "read it" }));
    await h.completed(id, after);
    // 每次请求 usage 为 10 输入 / 4 输出：第二次的输入与主会话基线相同，只计增量。
    const expected = {
      sessionId: id,
      totalTokens: 18,
      inputTokens: 10,
      outputTokens: 8,
      reasoningTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      modelRequestCount: 2,
      modelErrorCount: 0,
      inputBaselineBySource: { main_turn: 10 },
    };
    assert.deepEqual(await conversation(h, "v4/conversation/usage", id), expected);
    assert.deepEqual(await conversation(h, "session/usage", ` ${id} `), expected);
    // v4 usage 状态同 Node onModelComplete：水位是最近一次主轮次请求的 provider 用量，
    // 阈值为 null，累计值是两次请求之和（spec rust-m9-usage-logs §4.2）。
    const usage = h.messages
      .slice(after)
      .filter((m) => m.params?.frame?.topic === `conversation/${id}`)
      .flatMap((m) => m.params.frame.payload?.deltas ?? [])
      .filter((d: Message) => d.patch?.usage)
      .map((d: Message) => d.patch.usage);
    assert.equal(usage.length, 2);
    const last = usage.at(-1);
    assert.equal(last.contextWindow.usedTokens, 14);
    assert.equal(last.contextWindow.autoCompactThresholdTokens, null);
    assert.ok(last.contextWindow.maxTokens > 0);
    const sources = last.contextWindow.breakdown.map((b: Message) => b.source);
    for (const source of ["system_prompt", "system_tool_schemas", "messages"])
      assert.ok(sources.includes(source), sources.join(","));
    assert.equal(last.contextWindow.cache.hitRateRequestCount, 2);
    assert.deepEqual(last.cumulative, {
      inputTokens: 20,
      outputTokens: 8,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
    });
    assert.equal((await conversation(h, "session/usage", "unknown")).modelRequestCount, 0);

    const week = await stats(h, { range: "7d", timeZone: "Asia/Shanghai" });
    assert.equal(week.range, "7d");
    assert.equal(week.timeZone, "Asia/Shanghai");
    assert.equal(week.source, "agent-db");
    const summary = week.summary;
    assert.deepEqual(
      [summary.totalTokens, summary.inputTokens, summary.outputTokens, summary.totalSessions],
      [28, 20, 8, 1],
    );
    assert.deepEqual(
      [summary.totalTurns, summary.toolCallCount, summary.toolErrorRate, summary.modelErrorRate],
      [1, 1, 0, 0],
    );
    assert.deepEqual([summary.activeDays, summary.currentStreakDays], [1, 1]);
    assert.equal(typeof summary.avgTimeToFirstTokenMs, "number");
    assert.equal(typeof summary.avgTurnDurationMs, "number");
    assert.deepEqual(week.models, [
      {
        modelId: "core-model",
        totalTokens: 28,
        inputTokens: 20,
        outputTokens: 8,
        requestCount: 2,
        share: 1,
      },
    ]);
    assert.deepEqual(summary.favoriteModel, { modelId: "core-model", totalTokens: 28, share: 1 });
    assert.equal(week.tools.length, 1);
    assert.deepEqual(
      { ...week.tools[0], avgDurationMs: typeof week.tools[0]!.avgDurationMs },
      { toolName: "Read", callCount: 1, errorCount: 0, errorRate: 0, avgDurationMs: "number" },
    );
    assert.equal(week.heatmap.weeks.length, 2, "8 local days split by 7");
    assert.equal(week.dailyModelUsage.length, 8);
    const today = week.dailyModelUsage.at(-1)!;
    assert.deepEqual(today.models, [{ modelId: "core-model", totalTokens: 28 }]);
    const cell = week.heatmap.weeks[1]!.days.find((d) => d?.date === today.date)!;
    assert.deepEqual(
      [cell.level, cell.totalTokens, cell.turnCount, cell.toolCallCount],
      [4, 28, 1, 1],
    );

    const all = await h.client.request("usage/stats", { range: "all" }, v4UsageStatsResultSchema);
    assert.equal(all.timeZone, "UTC");
    assert.equal(all.heatmap.startDate, all.heatmap.endDate);
    assert.equal(all.summary.totalTokens, 28);

    // 行的身份与工具元数据同 Node（spec rust-m9-usage-logs §2.3）。
    const db = new DatabaseSync(f.db, { readOnly: true });
    const steps = db
      .prepare("select * from model_usage where query_source = 'main_turn' order by started_at")
      .all() as Message[];
    const turnRow = db.prepare("select * from turn_usage").get() as Message;
    const tool = db.prepare("select * from tool_usage").get() as Message;
    db.close();
    for (const step of steps) {
      assert.equal(step.id, `usage_model_main_turn_${step.assistant_message_id}_0`);
      assert.equal(step.logical_request_id, step.assistant_message_id);
      assert.equal(step.parent_user_message_id, turnRow.user_message_id);
      assert.equal(String(step.span_id).length, 16);
    }
    assert.match(String(turnRow.user_message_id), /^msg_/);
    assert.deepEqual(
      [tool.side_effect_scope, tool.read_only, tool.destructive, tool.exit_code],
      ["none", 1, 0, 0],
    );
    assert.equal(tool.approval_status, "none");
    assert.equal(typeof tool.first_output_at, "number");
  } finally {
    await f.close();
  }
});

test("Rust records failed turns like Node createTurnFailureError", async () => {
  const f = await fixture({
    respond(_req, res) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: "rejected", type: "invalid_request_error" } }));
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "fail please" }));
    await h.wait(
      (m) =>
        m.params?.frame?.payload?.deltas?.some((d: Message) => d.patch?.control?.phase === "error"),
      after,
    );
    const db = new DatabaseSync(f.db, { readOnly: true });
    const turnRow = db.prepare("select * from turn_usage").get() as Message;
    const model = db.prepare("select * from model_usage").get() as Message;
    db.close();
    // 普通轮失败也记录（Node turn.ts catch 路径），错误是包装后的 UnknownError。
    assert.deepEqual(
      [turnRow.status, turnRow.error_type, turnRow.error_code, turnRow.cancelled_by_user],
      ["error", "unknown_error", "UNKNOWN_ERROR", 0],
    );
    // 模型行是适配器错误：reason 作类型，没有 code，message 是通用文案。
    assert.deepEqual(
      [model.status, model.error_type, model.error_code, model.error_message],
      ["error", "invalid_request", null, "Provider rejected the model request."],
    );
  } finally {
    await f.close();
  }
});

test("Rust usage queries reject invalid params like Node parseParams", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const range = await rejection(stats(h, { range: "1d" }));
    assert.equal(range.code, -32602);
    assert.match(range.message, /^Invalid params — range: /);
    assert.equal(range.data.name, "ZodError");
    const extra = await rejection(stats(h, { range: "7d", extra: 1 }));
    assert.equal(extra.message, 'Invalid params — (root): Unrecognized key: "extra"');
    const blank = await rejection(conversation(h, "v4/conversation/usage", "  "));
    assert.equal(blank.code, -32602);
    assert.match(blank.message, /^Invalid params — sessionId: /);
  } finally {
    await f.close();
  }
});

// 用量表与 Node 共用：Node `queryAppUsage` 不按工作区过滤，按会话的查询只看该会话。
test("Rust answers usage queries over the usage rows Node wrote", async () => {
  const f = await fixture();
  try {
    const store = createSqliteSessionStore({ dbPath: f.db });
    const now = Date.now();
    for (const [id, directory] of [
      ["mine", f.cwd],
      ["other", join(f.root, "elsewhere")],
    ] as const) {
      await store.createSession({
        id: id as SessionId,
        projectID: "fixture" as ProjectId,
        workspaceID: directory as WorkspaceId,
        directory,
        slug: id,
        title: id,
        version: "fixture",
      });
      await store.recordModelUsage({
        id: `usage-${id}`,
        logicalRequestId: `request-${id}`,
        sessionID: id as SessionId,
        querySource: "main_turn",
        providerId: "fixture",
        modelId: "ts-model",
        status: "completed",
        startedAt: now - 60_000,
        inputTokens: 100,
        outputTokens: 7,
      });
      await store.upsertTurnUsage({
        sessionID: id as SessionId,
        turnID: `turn-${id}` as TurnId,
        status: "completed",
        startedAt: now - 60_000,
        durationMs: 500,
      });
      await store.upsertToolUsage({
        id: `tool-${id}`,
        sessionID: id as SessionId,
        toolCallID: `call-${id}` as ToolCallId,
        toolName: "Grep",
        status: "error",
        startedAt: now - 60_000,
      });
    }
    store.close();
    const h = f.start();
    const all = await stats(h, { range: "all" });
    assert.deepEqual(
      [all.summary.totalTokens, all.summary.totalTurns, all.summary.toolCallCount],
      [214, 2, 2],
    );
    assert.deepEqual(all.tools, [
      { toolName: "Grep", callCount: 2, errorCount: 2, errorRate: 1, avgDurationMs: null },
    ]);
    assert.equal((await conversation(h, "v4/conversation/usage", "mine")).totalTokens, 107);
    assert.equal((await conversation(h, "v4/conversation/usage", "other")).totalTokens, 107);
  } finally {
    await f.close();
  }
});
