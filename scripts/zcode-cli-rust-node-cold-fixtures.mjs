// Run through generate-zcode-cli-rust-fixtures.mjs. Stores Node-shaped
// transcripts with Node's repositories and records what Node rebuilds from
// them after a restart (the provider history entries of
// `hydrateMessageHistoryFromSession`); the Rust cold load must match.
// Spec rust-m11-node-storage §6.
import { DatabaseSync } from "node:sqlite";
import { runSqliteSessionMigrations } from "../apps/zcode-cli/packages/adapters/src/storage/session-store/migration-runner.ts";
import * as sessions from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/sessions.ts";
import * as messages from "../apps/zcode-cli/packages/adapters/src/storage/session-store/repositories/messages.ts";
import { hydrateMessageHistoryFromSession } from "../apps/zcode-cli/packages/core/src/agent/session-history-hydrator.ts";
import { createMessageHistory } from "../apps/zcode-cli/packages/core/src/agent/message-history.ts";
import {
  SYSTEM_REMINDER_SOURCES,
  getSystemReminderDescriptor,
  isMidConversationSystemSource,
  wrapSystemReminderForSource,
} from "../apps/zcode-cli/packages/core/src/system-reminder/source.ts";
import { transcript } from "./zcode-cli-rust-node-cold-scenarios.mjs";

const TABLES = ["session", "message", "part"];
const SUMMARY =
  "This session is being continued from a previous conversation.\n\nSummary: fixed the parser.";

function basic(t) {
  t.user("u1", [
    { type: "text", text: "Fix the bug" },
    {
      type: "file",
      mime: "text/plain",
      filename: "notes.txt",
      url: "file:///w/notes.txt",
      source: {
        type: "file",
        path: "/w/notes.txt",
        text: { value: "@notes.txt", start: 0, end: 10 },
      },
      metadata: {
        storageKind: "inline",
        recoverability: "provider_ready",
        preview: { text: "line one\nline two" },
      },
    },
    { type: "file", mime: "application/pdf", filename: "spec.pdf", url: "file:///w/spec.pdf" },
    { type: "agent", name: "reviewer" },
  ]);
  t.assistant("a1", "u1", [
    { type: "step-start" },
    {
      type: "reasoning",
      text: "Look first",
      metadata: { anthropic: { signature: "sig-1" } },
      time: { start: 1, end: 2 },
    },
    { type: "text", text: "Let me read." },
    t.tool("call_b", "Bash", 1, {
      status: "error",
      input: { command: "false" },
      error: "exit 1",
      metadata: { modelContent: "Command failed: exit 1" },
      time: { start: 3, end: 4 },
    }),
    t.tool("call_a", "Read", 0, t.done({ file_path: "a.ts" }, "1\tconst a = 1;")),
    { type: "step-finish", reason: "tool-calls", cost: 0, tokens: t.tokens(100, 5) },
  ]);
  t.assistant(
    "a2",
    "u1",
    [
      { type: "text", text: "Done." },
      { type: "text", text: "Second", ignored: true },
    ],
    {
      finish: "stop",
      anchor: {
        turnId: "turn_u1",
        historyRoundCount: 2,
        orderedMessageIds: ["u1", "a1", "a2"],
        boundaryMessageId: "a2",
      },
    },
  );
  t.notice("n1", "todo_reminder", "Todos:\n- a");
  t.notice("n2", "background_task", "<task-notification>done</task-notification>", {
    runtimeSource: "legacy_synthetic",
  });
  t.notice("n3", "subagent", "child finished");
  t.user("u2", [{ type: "text", text: "Continue" }], {
    metadata: { inputPresentation: "user_steer" },
  });
  t.assistant(
    "a3",
    "u2",
    [
      t.tool("call_c", "Read", 0, {
        status: "running",
        input: { file_path: "b" },
        title: "Read",
        metadata: {},
        time: { start: 5 },
      }),
    ],
    {
      interrupted: true,
    },
  );
  t.assistant("a4", "u2", [], { tokens: t.tokens(0, 0) });
  t.assistant("a5", "u2", [], {
    tokens: t.tokens(250, 0),
    info: { providerId: undefined, modelId: undefined },
  });
  t.user("u3", [{ type: "text", text: "shared" }], {
    source: "shared_context",
    metadata: { sharedContextStatus: "pending", contextId: "ctx" },
  });
}

function compacted(t) {
  t.user("u1", [{ type: "text", text: "one" }]);
  t.assistant("a1", "u1", [{ type: "text", text: "first" }]);
  t.user("u2", [{ type: "text", text: "two" }]);
  t.assistant("a2", "u2", [{ type: "text", text: "second" }], { tokens: t.tokens(900, 9) });
  t.assistant(
    "tl",
    "u2",
    [
      {
        timelineType: "context_compaction",
        display: "separator",
        status: "completed",
        operationId: "cmp_1",
        trigger: "auto",
        type: "timeline",
      },
      {
        type: "compaction",
        auto: true,
        trigger: "auto",
        operationId: "cmp_1",
        timelineStatus: "completed",
        timelineDisplay: "separator",
      },
    ],
    {
      semantics: {
        origin: "system",
        kind: "timeline_event",
        uiVisibility: "visible",
        providerVisibility: "hidden",
        transcriptVisibility: "visible",
      },
      anchor: null,
      finish: "completed",
    },
  );
  t.user(
    "s1",
    [
      { type: "text", text: SUMMARY, synthetic: true },
      {
        type: "compaction",
        auto: true,
        trigger: "auto",
        tail_start_id: "u2",
        compactBoundary: {
          boundaryId: "b1",
          trigger: "auto",
          preCompactTokenCount: 900,
          summarizedMessageCount: 2,
          summaryMessageIds: ["s1"],
          preservedSegment: { headMessageId: "u2", anchorMessageId: "s1", tailMessageId: "a2" },
          traceId: "trace",
        },
        operationId: "cmp_1",
      },
    ],
    {
      summary: { title: "Compact summary", body: SUMMARY, diffs: [] },
      semantics: {
        origin: "agent_runtime",
        kind: "compact_summary",
        uiVisibility: "hidden",
        providerVisibility: "visible",
        transcriptVisibility: "hidden",
      },
      anchor: undefined,
    },
  );
  t.user("u3", [{ type: "text", text: "three" }]);
  t.assistant("a3", "u3", [{ type: "text", text: "third" }]);
}

function rewound(t) {
  t.user("u1", [{ type: "text", text: "one" }]);
  t.assistant("a1", "u1", [{ type: "text", text: "first" }]);
  t.user("u2", [{ type: "text", text: "two" }]);
  t.assistant("a2", "u2", [{ type: "text", text: "second" }]);
  t.user("u3", [{ type: "text", text: "retry two" }]);
  t.assistant("a3", "u3", [{ type: "text", text: "second again" }]);
  return {
    keptMessageIDs: ["u1", "a1"],
    branchCutAfterMessageID: "a2",
    branchGeneration: 1,
    messageID: "a1",
    kind: "conversation_rewind",
    scope: "conversation",
    targetMessageID: "u2",
  };
}

const SCENARIOS = { basic, compacted, rewound };

async function scenario(name, build) {
  const sessionID = `sess_${name}`;
  const t = transcript(sessionID);
  const revert = build(t);
  const db = new DatabaseSync(":memory:");
  runSqliteSessionMigrations(db, ":memory:");
  sessions.createSession(db, {
    id: sessionID,
    projectID: "proj_w",
    directory: "/w",
    path: "/w",
    slug: sessionID,
    title: name,
    titleSource: "first_input",
    version: "0.0.0",
    permission: { mode: "build" },
    time: { created: 1_000, updated: 1_000 },
  });
  for (const message of t.messages) {
    await messages.saveMessage(db, message.info);
    for (const part of message.parts) await messages.savePart(db, part);
  }
  if (revert) await sessions.setRevert(db, { sessionID, revert });
  const stored = await messages.messages(db, { sessionID });
  const history = createMessageHistory();
  await hydrateMessageHistoryFromSession({
    history,
    messages: stored,
    branchCutAfterMessageId: revert?.branchCutAfterMessageID,
    rewindCreatedMessageId: revert?.createdMessageID,
    rewindKeptMessageIds: revert?.keptMessageIDs,
    rewindTargetMessageId: revert?.targetMessageID,
  });
  const tables = Object.fromEntries(
    TABLES.map((table) => [
      table,
      db
        .prepare(`select * from ${table} order by rowid`)
        .all()
        .map((row) => Object.values(row)),
    ]),
  );
  return { sessionID, tables, history: history.toRuntimeEntries() };
}

export async function nodeColdFixtures() {
  // 仓储层在写入时读取 Date.now()；固定时钟让夹具可重复生成。
  const realNow = Date.now;
  Date.now = () => 9_000;
  try {
    const out = {};
    for (const [name, build] of Object.entries(SCENARIOS)) out[name] = await scenario(name, build);
    return out;
  } finally {
    Date.now = realNow;
  }
}

/** Node's system reminder sources: descriptors and wrapping rules. */
export function systemReminderData() {
  return {
    sources: Object.fromEntries(
      SYSTEM_REMINDER_SOURCES.map((source) => {
        const { channel, lifecycle, isMeta, providerVisibility } =
          getSystemReminderDescriptor(source);
        return [
          source,
          {
            channel,
            lifecycle,
            isMeta,
            providerVisibility,
            midConversation: isMidConversationSystemSource(source),
            trailingNewline: wrapSystemReminderForSource(source, "x").endsWith("\n"),
          },
        ];
      }),
    ),
  };
}
