// Run through generate-zcode-cli-rust-fixtures.mjs (via zcode-cli-rust-node-db-fixtures.mjs).
// The fixed operations on sessions, messages and parts that Node's repositories
// execute for the Rust node storage parity fixture. Spec rust-m11-node-storage.

export const uuid = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
export const selection = { providerId: "p", modelId: "m", options: { reasoningLevel: "max" } };
export const userSemantics = {
  origin: "real_user",
  kind: "user_prompt",
  uiVisibility: "visible",
  providerVisibility: "visible",
  transcriptVisibility: "visible",
};
const assistantSemantics = {
  ...userSemantics,
  origin: "agent_runtime",
  kind: "assistant_response",
};
const assistant = (id, created, extra = {}) => ({
  id,
  sessionID: "sess_a",
  role: "assistant",
  time: { created, ...(extra.completed ? { completed: extra.completed } : {}) },
  parentID: "msg_u1",
  modelId: "m",
  providerId: "p",
  mode: "build",
  planEnabled: false,
  agent: "zcode-agent",
  path: { cwd: "/w", root: "/w" },
  cost: 0,
  tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
  ...(extra.finish ? { finish: extra.finish } : {}),
  semantics: assistantSemantics,
  anchor: { turnId: "turn_1" },
});
export const part = (id, messageID, body) => ({ id, sessionID: "sess_a", messageID, ...body });
const tool = (state) =>
  part("part_tool", "msg_a1", {
    type: "tool",
    callID: "call_1",
    declarationIndex: 0,
    tool: "Read",
    ...(state.status === "pending" ? { metadata: {} } : {}),
    state,
  });
const toolInput = JSON.parse('{"file_path":"a","10":1,"2":2,"n":1.0,"big":1e21,"s":"\\u0001é"}');
export const intent = {
  sourceCommandId: "cmd-1",
  queueItemId: "queue_c1",
  clientId: "client",
  kind: "sendText",
  admissionSeq: 1,
  admittedAt: 1500,
  requestedDelivery: "startNow",
  admittedDelivery: "startNow",
  attachmentRefs: [],
};
export const conversationIntent = {
  sourceCommandId: "cmd-1",
  queueItemId: "queue_c1",
  clientId: "client",
  kind: "sendText",
  text: "hello",
  attachments: [],
  delivery: { requested: "startNow", admitted: "startNow" },
  order: { admissionSeq: 1 },
  steer: { state: "notRequested" },
  dispatch: { state: "admitted" },
  admittedAt: 1500,
};

export const MESSAGE_OPERATIONS = [
  {
    op: "createSession",
    now: 1000,
    input: {
      id: "sess_a",
      projectID: "proj_w",
      directory: "/w",
      path: "/w",
      slug: "sess_a",
      title: "First",
      titleSource: "first_input",
      version: "0.0.0",
      traceID: "trace-1",
      permission: { mode: "build" },
    },
  },
  {
    op: "createSession",
    now: 1001,
    input: {
      id: "sess_b",
      projectID: "proj_w",
      workspaceID: "remote:x",
      parentID: "sess_a",
      taskType: "subagent_child",
      directory: "/w",
      slug: "sess_b",
      title: "Child",
      version: "0.0.0",
    },
  },
  {
    op: "createSession",
    now: 1002,
    input: {
      id: "sess_a",
      projectID: "proj_w",
      directory: "/w",
      path: "/w",
      slug: "sess_a",
      title: "First",
      titleSource: "first_input",
      version: "0.0.1",
      traceID: "trace-2",
    },
  },
  {
    op: "saveMessage",
    now: 1100,
    info: {
      id: "msg_u1",
      sessionID: "sess_a",
      role: "user",
      time: { created: 1100 },
      agent: "zcode-agent",
      modelSelection: selection,
      semantics: userSemantics,
      anchor: { turnId: "turn_1", origin: "realUser", sourceCommandId: "cmd-0" },
      tools: { Read: true },
      metadata: { inputClientId: "client" },
    },
  },
  {
    op: "savePart",
    now: 1100,
    part: part("part_t1", "msg_u1", {
      type: "text",
      text: 'hi "there"\n',
      time: { start: 1100, end: 1100 },
    }),
  },
  { op: "saveMessage", now: 1200, info: assistant("msg_a1", 1200) },
  { op: "savePart", now: 1201, part: part("part_ss", "msg_a1", { type: "step-start" }) },
  {
    op: "savePart",
    now: 1210,
    part: tool({
      status: "pending",
      input: toolInput,
      raw: JSON.stringify({ tool: "Read", input: toolInput }),
    }),
  },
  {
    op: "savePart",
    now: 1220,
    part: tool({
      status: "running",
      input: toolInput,
      title: "Read",
      metadata: {},
      time: { start: 1220 },
    }),
  },
  {
    op: "savePart",
    now: 1250,
    part: part("part_r", "msg_a1", {
      type: "reasoning",
      text: "think",
      metadata: { anthropic: { signature: "sig" } },
      time: { start: 1200, end: 1250 },
    }),
  },
  {
    op: "savePart",
    now: 1250,
    part: part("part_x", "msg_a1", {
      type: "text",
      text: "done",
      time: { start: 1200, end: 1250 },
    }),
  },
  {
    op: "savePart",
    now: 1260,
    part: tool({
      status: "completed",
      input: toolInput,
      output: "x",
      title: "Read",
      metadata: { schemaVersion: 1 },
      time: { start: 1220, end: 1260 },
    }),
  },
  {
    op: "savePart",
    now: 1270,
    part: part("part_sf", "msg_a1", {
      type: "step-finish",
      reason: "tool-calls",
      cost: 0,
      tokens: { total: 7, input: 5, output: 2, reasoning: 0, cache: { read: 1, write: 0 } },
    }),
  },
  {
    op: "saveMessage",
    now: 1270,
    info: assistant("msg_a1", 1200, { completed: 1270, finish: "tool-calls" }),
  },
  {
    op: "exec",
    sql: `insert into message (id, session_id, time_created, time_updated, data, sequence) values ('msg_old', 'sess_a', 1280, 1280, '{"role":"assistant","providerID":"old","variant":"v","time":{"created":1280}}', null)`,
  },
  { op: "saveMessage", now: 1300, info: assistant("msg_old", 1280, { completed: 1300 }) },
  {
    op: "saveMessage",
    now: 1310,
    info: {
      id: "msg_u1",
      sessionID: "sess_a",
      role: "user",
      time: { created: 1100 },
      agent: "zcode-agent",
      modelSelection: { providerId: "q", modelId: "n" },
      semantics: userSemantics,
    },
  },
  {
    op: "saveMessage",
    now: 1320,
    info: {
      id: "msg_tl",
      sessionID: "sess_a",
      role: "assistant",
      time: { created: 1320, completed: 1320 },
      parentID: "msg_tl",
      mode: "build",
      planEnabled: false,
      agent: "zcode-agent",
      path: { cwd: "/w", root: "/w" },
      cost: 0,
      tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
      finish: "completed",
      semantics: {
        ...assistantSemantics,
        origin: "system",
        kind: "timeline_event",
        providerVisibility: "hidden",
      },
    },
  },
  {
    op: "savePart",
    now: 1320,
    part: part("part_mc", "msg_tl", {
      timelineType: "model_change",
      display: "separator",
      status: "completed",
      fromModel: { ...selection, label: "M" },
      toModel: { providerId: "q", modelId: "n", label: "N" },
      time: { start: 1320, end: 1320 },
      type: "timeline",
    }),
  },
  {
    op: "savePart",
    now: 1321,
    part: part("part_sub", "msg_tl", {
      type: "subtask",
      prompt: "p",
      description: "d",
      agent: "general",
      model: { providerId: "p", modelId: "m" },
    }),
  },
  {
    op: "saveMessage",
    now: 1330,
    info: {
      id: "msg_copy",
      sessionID: "sess_b",
      role: "user",
      time: { created: 1330 },
      agent: "zcode-agent",
      semantics: userSemantics,
    },
    copyFrom: { sessionID: "sess_a", id: "msg_u1" },
  },
  { op: "removeMessage", sessionID: "sess_a", messageID: "msg_old" },
];
