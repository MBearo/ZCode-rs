// Run through generate-zcode-cli-rust-fixtures.mjs. Builds a Node session
// database with Node's SqliteSessionStore and records what Node reads back for
// the session lists (sessions-index seed, session/list) and for a cold resume
// (model selection, execution state, permission grant, todos, goal, turn
// number and latest message ids). Spec rust-m11-node-storage §6.1, §6.2.
import crypto from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { syncBuiltinESMExports } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { SqliteSessionStore } from "../apps/zcode-cli/packages/adapters/src/storage/session-store/sqlite-session-store.ts";
import { readSessionModelSelection } from "../apps/zcode-cli/packages/bootstrap/src/app/session-store.ts";
import { listSessions } from "../apps/zcode-cli/packages/bootstrap/src/zcode-protocol/server-operations.ts";
import { activeSessionMessages } from "../apps/zcode-cli/packages/core/src/agent/session-history-hydrator.ts";
import { getLatestActiveSessionMessageId } from "../apps/zcode-cli/packages/core/src/runtime/helpers/conversation.ts";
import { permissionFullAccessReceiptSchema } from "../apps/zcode-cli/packages/contracts/src/interfaces/permission-full-access.ts";
import {
  executionStateSchema,
  resolveExecutionState,
} from "../packages/shared/src/execution-state.ts";
import { parseRemoteWorkspaceIdentity } from "../packages/shared/src/remote-workspace-identity.ts";
import { compacted } from "./zcode-cli-rust-node-cold-fixtures.mjs";
import { transcript } from "./zcode-cli-rust-node-cold-scenarios.mjs";

const TABLES = ["session", "message", "part", "session_entry", "session_target", "todo"];
const REMOTE = "remote:ssh:host:22:me:/srv/app";
const TASK_LIST_SESSION_TYPES = ["interactive", "fork", "workflow_parent"];

const receipt = (sessionId, interactionId = "perm_1") => ({
  interactionId,
  event: {
    id: "evt_1",
    sessionId,
    traceId: "trace_1",
    type: "session_mode_changed",
    timestamp: "2026-01-01T00:00:00.000Z",
    sequenceNumber: 3,
    payload: {
      mode: "yolo",
      planEnabled: false,
      previousMode: "build",
      previousPlanEnabled: false,
      source: "command",
      permissionGrant: { interactionId, queueItemIds: ["queue_1"] },
    },
  },
});

/** The rewound transcript under its own message ids (message ids are global). */
function rewoundCopy(t) {
  t.user("p1", [{ type: "text", text: "one" }]);
  t.assistant("pa1", "p1", [{ type: "text", text: "first" }]);
  t.user("p2", [{ type: "text", text: "two" }]);
  t.assistant("pa2", "p2", [{ type: "text", text: "second" }]);
  t.user("p3", [{ type: "text", text: "retry two" }]);
  t.assistant("pa3", "p3", [{ type: "text", text: "second again" }]);
  return {
    revert: {
      keptMessageIDs: ["p1", "pa1"],
      branchCutAfterMessageID: "pa2",
      branchGeneration: 1,
      messageID: "pa1",
      kind: "conversation_rewind",
      scope: "conversation",
      targetMessageID: "p2",
    },
  };
}

const SESSIONS = [
  {
    id: "sess_main",
    title: "Main",
    titleSource: "custom",
    permission: { mode: "plan" },
    time: 2_000,
    build: compacted,
    entries: {
      "runtime/model_selection": {
        providerId: "p",
        modelId: "m",
        options: { reasoningLevel: "high" },
      },
      "runtime/execution_state": { mode: "yolo", planEnabled: true },
      "runtime/permission_full_access": receipt("sess_main"),
    },
    todos: [
      { id: "t1", content: "Write tests", status: "in_progress", priority: "high" },
      { id: "t2", content: "Ship", status: "pending", priority: "medium" },
    ],
    target: { objective: "Ship it", tokenBudget: 500 },
  },
  {
    id: "sess_partial",
    title: "Partial",
    permission: { mode: "edit" },
    time: 2_100,
    build: rewoundCopy,
    entries: {
      "runtime/model_selection": { providerId: "p", modelId: "m", options: { reasoningLevel: 5 } },
      "runtime/execution_state": { mode: "weird", planEnabled: true },
      "runtime/permission_full_access": receipt("sess_other_owner"),
    },
  },
  {
    id: "sess_fork",
    title: "Fork",
    titleSource: "generated",
    taskType: "fork",
    parentID: "sess_main",
    time: 2_200,
  },
  {
    id: "sess_child",
    title: "Child",
    taskType: "subagent_child",
    parentID: "sess_main",
    time: 2_300,
  },
  { id: "sess_archived", title: "Archived", time: 2_400, archived: 2_500 },
  { id: "sess_other", title: "Other", directory: "/other", time: 2_600 },
  { id: "sess_remote", title: "Remote", directory: "/srv/app", workspaceID: REMOTE, time: 2_700 },
  { id: "sess_legacy_remote", title: "Legacy", directory: "/srv/app", time: 2_800 },
  { id: "sess_default", title: "", titleSource: "default", time: 2_900 },
];

async function populate(store) {
  for (const spec of SESSIONS) {
    const directory = spec.directory ?? "/w";
    await store.createSession({
      id: spec.id,
      projectID: "proj_w",
      ...(spec.workspaceID ? { workspaceID: spec.workspaceID } : {}),
      ...(spec.parentID ? { parentID: spec.parentID } : {}),
      ...(spec.taskType ? { taskType: spec.taskType } : {}),
      directory,
      path: directory,
      slug: spec.id,
      title: spec.title,
      ...(spec.titleSource ? { titleSource: spec.titleSource } : {}),
      version: "0.0.0",
      ...(spec.permission ? { permission: spec.permission } : {}),
      time: { created: spec.time, updated: spec.time },
    });
    if (spec.build) {
      const t = transcript(spec.id);
      const extra = spec.build(t) ?? {};
      for (const message of t.messages) {
        await store.saveMessage(message.info);
        for (const part of message.parts) await store.savePart(part);
      }
      if (extra.revert) await store.setRevert({ sessionID: spec.id, revert: extra.revert });
    }
    for (const [type, data] of Object.entries(spec.entries ?? {})) {
      await store.saveSessionEntry({
        id: `${spec.id}:${type}`,
        sessionID: spec.id,
        type,
        time: { created: spec.time, updated: spec.time },
        data,
      });
    }
    if (spec.todos) await store.updateTodos({ sessionID: spec.id, todos: spec.todos });
    if (spec.target)
      await store.setTarget({ sessionID: spec.id, status: "active", ...spec.target });
    if (spec.archived) {
      await store.updateSession({ id: spec.id, timeArchived: spec.archived });
    }
  }
}

/** Mirrors v4-bridge `loadStoredSessionSummaries` (the store query is Node's). */
async function storedSummaries(store, workspaceId) {
  const remote = parseRemoteWorkspaceIdentity(workspaceId);
  const stored = await store.listSessions({
    directory: remote?.workspacePath ?? workspaceId,
    includeArchived: false,
    limit: 200,
    taskTypes: TASK_LIST_SESSION_TYPES,
    workspaceID: remote ? workspaceId : null,
  });
  const titleSource = (source) =>
    source === "custom" ? "custom" : source === "default" ? "default" : "generated";
  return stored.map((session) => ({
    sessionId: String(session.id),
    workspaceId,
    ...(session.parentID ? { parentSessionId: String(session.parentID) } : {}),
    title: session.title ?? "",
    titleSource: titleSource(session.titleSource),
    phase: "completedSuccess",
    sessionEnded: true,
    hasBackgroundWork: false,
    lastActivityAt: session.time?.updated ?? 0,
    createdAt: session.time?.created ?? 0,
  }));
}

/** The persisted facts `resumeFromStore` reads (see core runtime/methods/resume.ts). */
async function resumeFacts(store, sessionId) {
  const session = await store.getSession(sessionId);
  if (!session || session.time.archived !== undefined) return null;
  const messages = await store.messages({ sessionID: sessionId });
  const branch = {
    branchCutAfterMessageId: session.revert?.branchCutAfterMessageID,
    rewindCreatedMessageId: session.revert?.createdMessageID,
    rewindKeptMessageIds: session.revert?.keptMessageIDs,
    rewindTargetMessageId: session.revert?.targetMessageID,
  };
  const active = activeSessionMessages(messages, branch);
  const timeline = activeSessionMessages(messages, {
    ...branch,
    includeCompactPreservedSegment: false,
  });
  const latestAssistant = [...timeline].reverse().find((m) => m.info.role === "assistant");
  const permissionMode = session.permission?.mode;
  const entries = (type) => store.sessionEntries({ sessionID: sessionId, type });
  const saved = executionStateSchema.safeParse(
    (await entries("runtime/execution_state")).at(-1)?.data,
  );
  const grant = (await entries("runtime/permission_full_access")).at(-1);
  const grantReceipt = grant ? permissionFullAccessReceiptSchema.safeParse(grant.data) : null;
  return {
    modelSelection: (await readSessionModelSelection(store, sessionId)) ?? null,
    execution: saved.success
      ? saved.data
      : permissionMode !== undefined
        ? resolveExecutionState({ mode: permissionMode })
        : null,
    permissionGrant:
      grantReceipt?.success && grantReceipt.data.event.sessionId === sessionId
        ? grantReceipt.data.interactionId
        : null,
    todos: await store.readTodos({ sessionID: sessionId }),
    target: await store.readTarget({ sessionID: sessionId }),
    turnNumber: active.filter((m) => m.info.role === "user" && !m.info.summary).length,
    latestConversationMessageId: getLatestActiveSessionMessageId(messages, branch) ?? null,
    latestAssistantMessageId: latestAssistant?.info.id ?? null,
    latestAssistantTurnId: latestAssistant?.info.anchor?.turnId ?? null,
    lastAssistantCompletedAtMs: latestAssistant?.info.time.completed ?? null,
  };
}

const LIST_PARAMS = {
  all: {},
  local: { workspace: { workspacePath: "/w", workspaceKey: "/w" } },
  archived: { workspace: { workspacePath: "/w", workspaceKey: "/w" }, includeArchived: true },
  remote: {
    workspace: { workspacePath: "/srv/app", workspaceKey: REMOTE, workspaceIdentity: REMOTE },
  },
  limited: { limit: 2 },
  byId: { sessionIds: ["sess_child", "sess_archived", "sess_remote", "missing"] },
  byIdArchived: { sessionIds: ["sess_archived"], includeArchived: true },
};

export async function nodeSessionFixtures() {
  const realNow = Date.now;
  const realUuid = crypto.randomUUID;
  Date.now = () => 9_000;
  crypto.randomUUID = () => "00000000-0000-4000-8000-000000000002";
  syncBuiltinESMExports();
  const dir = await mkdtemp(join(tmpdir(), "zcode-rust-sessions-"));
  const store = await SqliteSessionStore.openStartup({ dbPath: join(dir, "db.sqlite") });
  try {
    await populate(store);
    const context = { deps: { sessionStore: store }, sessions: new Map() };
    const lists = {};
    for (const [name, params] of Object.entries(LIST_PARAMS)) {
      lists[name] = { params, result: await listSessions(context, params) };
    }
    const summaries = {};
    for (const workspaceId of ["/w", "/srv/app", REMOTE, "/nope"]) {
      summaries[workspaceId] = await storedSummaries(store, workspaceId);
    }
    const resume = {};
    for (const id of ["sess_main", "sess_partial", "sess_fork", "sess_archived", "missing"]) {
      resume[id] = await resumeFacts(store, id);
    }
    const db = new DatabaseSync(store.getDatabasePath(), { readOnly: true });
    const tables = Object.fromEntries(
      TABLES.map((table) => [
        table,
        db
          .prepare(`select * from ${table} order by rowid`)
          .all()
          .map((row) => Object.values(row)),
      ]),
    );
    db.close();
    return { tables, lists, summaries, resume };
  } finally {
    store.close();
    await rm(dir, { recursive: true, force: true });
    Date.now = realNow;
    crypto.randomUUID = realUuid;
    syncBuiltinESMExports();
  }
}
