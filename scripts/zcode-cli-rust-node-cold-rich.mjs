// Run through generate-zcode-cli-rust-fixtures.mjs (via zcode-cli-rust-node-cold-fixtures.mjs).
// Transcripts exercising every cold row rule: model changes, guided input,
// background and subagent turns, goal verification, forks, feedback, failures,
// cancellation and output continuation. Spec rust-m11-node-storage §6.

const intent = (id, text, extra = {}) => ({
  sourceCommandId: `cmd_${id}`,
  queueItemId: `queue_${id}`,
  clientId: "client-1",
  kind: "sendText",
  text,
  attachments: [],
  delivery: { requested: "startNow", admitted: "startNow" },
  order: { admissionSeq: 1 },
  steer: { state: "notRequested" },
  dispatch: { state: "drained" },
  admittedAt: 1,
  ...extra,
});

export function rich(t) {
  t.user("u1", [{ type: "text", text: "Plan it" }], {
    metadata: {
      conversationInputIntent: intent("u1", "Plan it", {
        attachments: [
          { ref: "zcode-artifact://s/a1", fileName: "a.png", mime: "image/png", bytes: 5 },
        ],
        provenance: { sourceCommandId: "root_cmd" },
      }),
      epilogueStart: 4,
    },
  });
  t.assistant("a1", "u1", [
    { type: "reasoning", text: "", time: { start: 1 } },
    { type: "text", text: "Spawning" },
    t.tool(
      "call_agent",
      "Agent",
      0,
      t.done(
        { description: "Look around", prompt: "Inspect", agent: "explore" },
        "Found it.\nagentId: agent_1 (internal)",
        {
          title: "Agent",
        },
      ),
    ),
    t.tool(
      "call_mcp",
      "mcp__x__y",
      1,
      t.done({}, "ok", {
        metadata: { display: { kind: "mcp_tool", serverName: "x", toolName: "y" } },
      }),
    ),
    // strict display 多出未知键时整块丢弃（Node parseCompletedToolPartMetadata）。
    t.tool(
      "call_bad",
      "mcp__x__z",
      2,
      t.done({}, "ok", { metadata: { display: { kind: "mcp_tool", server: "x", tool: "z" } } }),
    ),
    { type: "subtask", prompt: "p", description: "legacy subtask", agent: "general" },
    t.tool(
      "call_todo",
      "TodoWrite",
      3,
      t.done(
        {
          todos: [
            { content: "Write tests", status: "in_progress", activeForm: "Writing tests" },
            { content: "Ship", status: "pending", activeForm: "Shipping" },
          ],
        },
        "Todos updated",
      ),
    ),
    t.tool(
      "call_apps",
      "mcp__computer-use__list_apps",
      4,
      t.done(
        {},
        JSON.stringify({ apps: [{ pid: 42, name: "Finder", bundle_id: "com.apple.finder" }] }),
      ),
    ),
    t.tool(
      "call_click",
      "mcp__computer-use__click",
      5,
      t.done({ app_ref: { pid: 42 }, x: 1, y: 2 }, "clicked"),
    ),
  ]);
  t.user("g1", [{ type: "text", text: "also check tests" }], {
    metadata: {
      turnSteerDelivery: "guide",
      conversationInputIntent: intent("g1", "also check tests"),
    },
  });
  t.assistant("a2", "u1", [{ type: "text", text: "Checked." }], {
    info: { metadata: { assistantFeedback: "like" } },
    anchor: { turnId: "turn_u1", historyRoundCount: 3 },
  });
  t.assistant(
    "tl",
    "a2",
    [
      {
        timelineType: "goal_verification",
        display: "worklog",
        status: "completed",
        targetId: "target_1",
        verificationId: "verify_1",
        goalIteration: 1,
        verification: { passed: false, reason: "tests missing" },
        anchorMessageId: "a2",
        type: "timeline",
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
    },
  );
  t.assistant(
    "mc",
    "a2",
    [
      {
        timelineType: "model_change",
        display: "separator",
        status: "completed",
        fromModel: { providerId: "p", modelId: "m", label: "M" },
        toModel: { providerId: "q", modelId: "n", label: "N" },
        type: "timeline",
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
    },
  );
  t.user("u2", [{ type: "text", text: "Now write" }], {
    modelSelection: { providerId: "q", modelId: "n" },
  });
  t.assistant("a3", "u2", [{ type: "text", text: "Partial output" }], { finish: "length" });
  t.assistant("a4", "u2", [{ type: "text", text: " continues" }], { finish: "stop" });
  t.notice(
    "bg",
    "background_task",
    "<task-notification>\n<task-id>t1</task-id>\n<tool-use-id>call_agent</tool-use-id>\n<status>failed</status>\n<error>boom &amp; bust</error>\n</task-notification>",
    {
      info: {
        metadata: {
          originMeta: { backgroundSource: "subagent", workId: "agent_1", title: "Look around" },
        },
      },
    },
  );
  t.assistant("a5", "bg", [{ type: "text", text: "Noted the failure." }]);
}

export function failures(t) {
  t.user("u1", [{ type: "text", text: "go" }]);
  t.assistant("a1", "u1", [{ type: "text", text: "trying" }], {
    error: {
      name: "ProviderError",
      data: { message: "rate limited", retryable: true, attribution: { source: "provider" } },
    },
  });
  t.user("u2", [{ type: "text", text: "again" }]);
  t.assistant("a2", "u2", [{ type: "text", text: "partial" }], {
    error: { name: "AbortError", data: { message: "stopped" } },
  });
  t.user("u3", [{ type: "text", text: "retry" }]);
  t.assistant("a3", "u3", [{ type: "text", text: "discarded" }], {
    error: {
      name: "StreamRecoveryDiscarded",
      data: {
        message: "Partial assistant output was discarded before a streaming retry.",
        retryNumber: 1,
      },
    },
    finish: "stream_recovery_discarded",
  });
  t.assistant("a4", "u3", [{ type: "text", text: "recovered" }]);
  t.user("u4", [{ type: "text", text: "/goal ship" }], {
    metadata: { executionKind: "controlOnly" },
  });
  t.user(
    "f1",
    [
      {
        type: "text",
        text: "Forked from parent",
        synthetic: true,
        metadata: {
          forkContext: {
            kind: "session_fork",
            parentSessionId: "sess_parent",
            targetMessageId: "pm1",
          },
        },
      },
    ],
    {
      source: "fork",
      synthetic: true,
      semantics: {
        origin: "agent_runtime",
        kind: "fork_notice",
        uiVisibility: "visible",
        providerVisibility: "visible",
        transcriptVisibility: "visible",
      },
      anchor: undefined,
    },
  );
}

/** `target_completion_verification` entries (the legacy goal verify source). */
export const FAILURE_GOAL_ENTRIES = [
  {
    data: {
      eventId: "e1",
      payload: {
        targetId: "target_2",
        verificationId: "v2",
        status: "started",
        goalIteration: 1,
        anchorAssistantMessageId: "a4",
      },
      sequenceNumber: 5,
    },
    time: { created: 5 },
  },
  {
    data: {
      eventId: "e2",
      payload: {
        targetId: "target_2",
        verificationId: "v2",
        status: "completed",
        goalIteration: 1,
        verification: { passed: true, reason: "done" },
      },
      sequenceNumber: 6,
    },
    time: { created: 6 },
  },
  {
    data: {
      eventId: "e3",
      payload: { targetId: "target_3", verificationId: "v3", status: "failed_closed" },
      sequenceNumber: 7,
    },
    time: { created: 7 },
  },
];
