// Shared call-site stand-ins for the hook fixture generators: they capture
// what Node's call sites hand to the hook runner.
export const TIMESTAMP = "2026-01-01T00:00:00.000Z";
export const TRACE = { traceId: "trace-1", turnId: "turn-1" };
export const CALL = { id: "call-1", name: "Bash" };

/** Captures what a call site hands to the hook runner. */
export function capture() {
  const seen = [];
  const hookRunner = {
    run: async (input, options) => {
      seen.push({ input: { ...input, timestamp: TIMESTAMP }, options });
      return next.shift() ?? { additionalContexts: [] };
    },
  };
  const next = [];
  return { seen, hookRunner, next };
}

export function toolDeps(hookRunner) {
  return {
    hookRunner,
    getMode: () => "edit",
    getWorkingDirectory: () => "/work",
    sessionId: "session-1",
    turnId: "turn-deps",
  };
}

export function runtimeThis(hookRunner) {
  return {
    config: { agentName: "main" },
    getMode: () => "build",
    getSessionModelSelection: () => ({ providerId: "zai", modelId: "glm-4.6" }),
    hookRunner,
    messageHistory: { addEntries() {} },
    sessionId: "session-1",
    sessionStartHookRan: false,
    workingDirectory: "/work",
  };
}
