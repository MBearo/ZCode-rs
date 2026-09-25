// Conversation telemetry comparison for `zcode-cli-rust-usage-diff.mjs` (spec
// rust-m9-usage-logs §5): the `v4/telemetry/event` facts and
// `computer-use/operation-event` events of one scenario, ids and times replaced
// by placeholders in first-seen order, compared in emission order.

/** Members whose values are ids: replaced by `<member#n>` in first-seen order. */
const ID_MEMBERS = new Set([
  "eventId",
  "sessionId",
  "turnId",
  "sourceCommandId",
  "requestId",
  "queryId",
  "toolCallId",
  "childToolCallId",
  "parentToolCallId",
  "assistantMessageId",
  "partId",
  "childSessionId",
  "agentId",
  "operationId",
  "messageId",
  "summaryMessageId",
  "mcpInstanceId",
]);
const TIME_MEMBERS = new Set(["occurredAt", "timestamp", "startedAt", "endedAt", "sampledAt"]);
const DURATION_MEMBERS = new Set([
  "durationMs",
  "delayMs",
  "totalMs",
  "commandRunMs",
  "firstOutputMs",
  "noOutputMs",
  "permissionWaitMs",
  "readMs",
  "writeMs",
  "fsReadMs",
  "fsWriteMs",
  "matchMs",
  "patchMatchMs",
  "uptimeMs",
]);

/** The notifications of `methods` that belong to `sessions` (child sessions included). */
export function sessionNotifications(frames, methods, sessions) {
  return frames
    .filter((frame) => methods.includes(frame.method))
    .filter((frame) => sessions.has(frame.params?.sessionId))
    .map((frame) => ({ method: frame.method, ...frame.params }));
}

/** The session ids of a scenario: the session and the children its facts name. */
export function scenarioSessions(frames, id) {
  const sessions = new Set([id]);
  for (const frame of frames) {
    const child = frame.params?.childSessionId;
    if (frame.method === "v4/telemetry/event" && sessions.has(frame.params?.sessionId) && child)
      sessions.add(child);
  }
  return sessions;
}

/** Title requests: Node sends them without streaming (spec rust-m9-usage-logs §2.6). */
const TITLE_SOURCES = new Set(["session_title", "goal_summary_title"]);

/**
 * Node starts concurrency-safe tools while the model is still streaming, so their
 * scheduled/started facts can precede the request's completion; Rust starts tools
 * after it. Within such a window the completion and its usage come first.
 */
function canonicalOrder(facts) {
  const rank = (f) =>
    f.kind === "model.request.status" && f.status === "model_request_completed"
      ? 0
      : f.kind === "usage.delta"
        ? 1
        : ["tool-scheduled", "tool-started"].includes(f.kind) ||
            (f.kind === "tool.lifecycle" && ["scheduled", "started"].includes(f.phase))
          ? 2
          : -1;
  const out = [];
  let window = [];
  const flush = () => {
    out.push(...window.sort((a, b) => rank(a) - rank(b)));
    window = [];
  };
  for (const fact of facts) {
    if (rank(fact) < 0) {
      flush();
      out.push(fact);
    } else window.push(fact);
  }
  flush();
  return out;
}

/** Facts with ids, times and durations as placeholders (`eventSeq` keeps only its order). */
export function normalizeFacts(facts, roots = []) {
  const ids = new Map();
  const counters = {};
  const id = (member, value) => {
    const key = `${member}\0${value}`;
    if (!ids.has(key)) {
      counters[member] = (counters[member] ?? 0) + 1;
      ids.set(key, `<${member}#${counters[member]}>`);
    }
    return ids.get(key);
  };
  const lastSeq = new Map();
  const text = (value) => roots.reduce((out, root) => out.split(root).join("<root>"), value);
  // 序号按发出顺序比较，再调整 Node 提前启动工具的交错。
  const sequenced = facts.map((fact) => {
    const seq = fact.eventSeq ?? fact.sequenceNumber;
    if (typeof seq !== "number" || seq === 0) return { fact, seq };
    const stream = `${fact.method}\0${fact.sessionId}`;
    const previous = lastSeq.get(stream) ?? -1;
    lastSeq.set(stream, seq);
    return { fact, seq: seq > previous ? "<next>" : `<stale:${seq}>` };
  });
  const order = canonicalOrder(sequenced.map(({ fact }) => fact));
  const seqOf = new Map(sequenced.map(({ fact, seq }) => [fact, seq]));
  return order.map((fact) => {
    const out = {};
    for (const key of Object.keys(fact).sort()) {
      const value = fact[key];
      // Rust 不向 Host 请求会话运行偏好（spec §5.4），没有 memoryEnabled。
      if (key === "memoryEnabled") continue;
      if (ID_MEMBERS.has(key) && typeof value === "string") out[key] = id(key, value);
      else if (TIME_MEMBERS.has(key) && typeof value === "number") out[key] = "<time>";
      else if (DURATION_MEMBERS.has(key) && typeof value === "number") out[key] = "<ms>";
      else if (key === "performance" && value && typeof value === "object")
        out[key] = Object.fromEntries(
          Object.entries(value)
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([k, v]) => [k, DURATION_MEMBERS.has(k) ? "<ms>" : v]),
        );
      else if (key === "eventSeq" || key === "sequenceNumber") out[key] = seqOf.get(fact);
      else if (key === "transport" && TITLE_SOURCES.has(fact.querySource)) out[key] = "<title>";
      // 非内置 MCP 的 id 是按进程随机盐的 HMAC，只比较形态。
      else if (key === "mcpId" && /^(custom|plugin):[a-f0-9]{12}$/.test(value))
        out[key] = value.replace(/:.*/, ":<hmac>");
      else out[key] = typeof value === "string" ? text(value) : value;
    }
    return out;
  });
}

/** Differences between two normalized fact lists (`[]` when equal). */
export function diffFacts(node, rust) {
  const out = [];
  for (let i = 0; i < Math.max(node.length, rust.length); i++) {
    const n = node[i];
    const r = rust[i];
    if (JSON.stringify(n) === JSON.stringify(r)) continue;
    if (!n || !r) {
      out.push({ index: i, node: n ?? "(missing)", rust: r ?? "(missing)" });
      continue;
    }
    const fields = {};
    for (const key of new Set([...Object.keys(n), ...Object.keys(r)]))
      if (JSON.stringify(n[key]) !== JSON.stringify(r[key]))
        fields[key] = { node: n[key], rust: r[key] };
    out.push({ index: i, kind: n.kind ?? n.method, fields });
  }
  return out;
}

const TTFT_TIMES = new Set([
  "receivedAt",
  "admittedAt",
  "executionAt",
  "requestAt",
  "outputAt",
  "start",
  "end",
]);
const TTFT_IDS = new Set([
  "observationId",
  "instanceId",
  "commandId",
  "sessionId",
  "logicalCallId",
  "queryId",
  "requestId",
  "id",
]);

// MCP 进程遥测是独立通道：Node 在会话 runtime 创建时连接 MCP，Rust 在首个 step 准备工具时，
// 与会话事实的相对顺序不同，单独比较。
export const PROCESS = ["process/mcpTelemetry"];

/** Process events have no session: they belong to the scenario's frame range. */
export function processFacts(result, name) {
  const [from, to] = result.ranges[name];
  return normalizeFacts(
    result.telemetry
      .filter((f) => PROCESS.includes(f.method))
      .filter((f) => f.index >= from && f.index < to)
      .filter((f) => !f.params.sessionId || f.params.sessionId === result.sessions[name])
      .map((f) => ({ method: f.method, ...f.params })),
    [result.root],
  );
}

/**
 * The local TTFT of a scenario's frame range: the first checkpoint and the
 * last framed facts. Preparation details come from Node's internal pipeline
 * (context, hooks, persistence) and change its checkpoint count, so neither
 * is compared (spec rust-m9-usage-logs §7.3).
 */
export function localTtft(frames, [from, to]) {
  const inRange = frames.filter((f) => f.index >= from && f.index < to);
  const first = inRange.find((f) => f.method === "v4/telemetry/local-ttft")?.params;
  const framed = inRange.map((f) => f.params?.frame?.ttft).filter(Boolean);
  // 已知差异（spec §7.3）：Node 的 runtime turnId（`turn_…`）与产品 turnId（用户消息 id）是两个值，
  // Rust 的两者相同；只比较两者是否出现，不比较是否相等。
  const turn = (value, name) => (value === undefined ? undefined : name);
  const facts = [first, ...framed.slice(-1)]
    .filter(Boolean)
    .map(({ revision: _revision, ...rest }) => ({
      ...rest,
      turnId: turn(rest.turnId, "<turn>"),
      productTurnId: turn(rest.productTurnId, "<product-turn>"),
      details: rest.details.filter((d) => d.stage !== undefined && !d.id.startsWith("prepare:")),
    }));
  return normalizeTtft(facts);
}

/** Local TTFT checkpoints (spec rust-m9-usage-logs §7): times and ids as placeholders. */
function normalizeTtft(list) {
  const ids = new Map();
  const id = (value) => {
    // 明细 id 由前缀与请求 id 组成：只替换请求部分。
    const [prefix, rest] = value.includes(":") ? value.split(/:(.*)/s) : ["", value];
    if (!ids.has(rest)) ids.set(rest, `<id#${ids.size + 1}>`);
    return prefix ? `${prefix}:${ids.get(rest)}` : ids.get(rest);
  };
  const walk = (value, key) => {
    if (Array.isArray(value)) return value.map((item) => walk(item));
    if (value && typeof value === "object")
      return Object.fromEntries(
        Object.keys(value)
          .sort()
          .map((k) => [k, walk(value[k], k)]),
      );
    if (TTFT_TIMES.has(key) && typeof value === "number") return "<time>";
    if (TTFT_IDS.has(key) && typeof value === "string") return id(value);
    if (key === "cliVersion") return "<version>";
    return value;
  };
  return list.map((item) => walk(item));
}
