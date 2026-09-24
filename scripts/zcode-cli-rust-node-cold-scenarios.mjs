// Run through generate-zcode-cli-rust-fixtures.mjs (via zcode-cli-rust-node-cold-fixtures.mjs).
// Node-shaped transcripts for the Rust cold load parity fixture, following
// Node's writer templates (message-persistence, turn-*, compact-persistence).
// Spec rust-m11-node-storage §6.

export const userSemantics = {
  origin: "real_user",
  kind: "user_prompt",
  uiVisibility: "visible",
  providerVisibility: "visible",
  transcriptVisibility: "visible",
};
const assistantSemantics = {
  origin: "agent_runtime",
  kind: "assistant_response",
  uiVisibility: "visible",
  providerVisibility: "visible",
  transcriptVisibility: "visible",
};
const tokens = (input, output) => ({
  input,
  output,
  reasoning: 0,
  cache: { read: 0, write: 0 },
});
const selection = { providerId: "p", modelId: "m", options: { reasoningLevel: "high" } };

export function transcript(sessionID) {
  let clock = 1_000;
  const messages = [];
  const tick = () => (clock += 10);
  const user = (id, parts, extra = {}) => {
    const created = tick();
    messages.push({
      info: {
        id,
        sessionID,
        role: "user",
        time: { created },
        agent: "zcode-agent",
        modelSelection: selection,
        semantics: userSemantics,
        anchor: { turnId: `turn_${id}`, origin: "realUser", sourceCommandId: `cmd_${id}` },
        tools: { Read: true },
        ...extra,
      },
      parts: parts.map((part, index) => ({
        id: `part_${id}_${index}`,
        sessionID,
        messageID: id,
        ...(part.type === "text" ? { time: { start: created, end: created } } : {}),
        ...part,
      })),
    });
    return id;
  };
  const notice = (id, source, text, extra = {}) => {
    const created = tick();
    const metadata = {
      runtimeMessage: { source: extra.runtimeSource ?? source },
      source,
      visibility: "model-only",
    };
    messages.push({
      info: {
        id,
        sessionID,
        role: "user",
        time: { created },
        agent: "zcode-agent",
        metadata,
        semantics: {
          origin: "agent_runtime",
          kind: "system_reminder",
          source,
          uiVisibility: "hidden",
          providerVisibility: "visible",
          transcriptVisibility: "hidden",
        },
        source,
        synthetic: true,
        visibility: "model-only",
        ...extra.info,
      },
      parts: [
        {
          id: `part_${id}`,
          sessionID,
          messageID: id,
          type: "text",
          text,
          synthetic: true,
          time: { start: created, end: created },
          metadata: extra.partMetadata ?? metadata,
        },
      ],
    });
  };
  const assistant = (id, parent, parts, extra = {}) => {
    const created = tick();
    const completed = extra.interrupted ? undefined : tick();
    messages.push({
      info: {
        id,
        sessionID,
        role: "assistant",
        time: { created, ...(completed ? { completed } : {}) },
        ...(extra.error ? { error: extra.error } : {}),
        parentID: parent,
        modelId: "m",
        providerId: "p",
        mode: "build",
        planEnabled: false,
        agent: "zcode-agent",
        path: { cwd: "/w", root: "/w" },
        cost: 0,
        tokens: extra.tokens ?? tokens(100, 5),
        ...(extra.finish ? { finish: extra.finish } : {}),
        semantics: extra.semantics ?? assistantSemantics,
        anchor: extra.anchor ?? { turnId: `turn_${parent}` },
        ...extra.info,
      },
      parts: parts.map((part, index) => ({
        id: `part_${id}_${index}`,
        sessionID,
        messageID: id,
        ...part,
      })),
    });
    return id;
  };
  const tool = (callID, name, index, state) => ({
    type: "tool",
    callID,
    declarationIndex: index,
    tool: name,
    state,
  });
  const done = (input, output, extra = {}) => ({
    status: "completed",
    input,
    output,
    title: extra.title ?? "Read",
    metadata: { schemaVersion: 1, ...extra.metadata },
    time: { start: clock, end: clock + 1 },
  });
  return { messages, user, notice, assistant, tool, done, tick, tokens };
}
