import assert from "node:assert/strict";
import test from "node:test";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
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
