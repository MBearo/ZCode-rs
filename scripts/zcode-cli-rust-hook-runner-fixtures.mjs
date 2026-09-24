// Hook runner merging, lifecycle payloads, permission answers and texts from
// Node's hook modules. Imported by the hook fixtures.
import { CoreErrorType, createCoreError } from "../apps/zcode-cli/packages/contracts/src/index.ts";
import {
  createHookExecutionDescriptor,
  sanitizeHookDisplayText,
} from "../apps/zcode-cli/packages/core/src/hooks/display-metadata.ts";
import { InMemoryHookRunner } from "../apps/zcode-cli/packages/core/src/hooks/runner.ts";
import {
  applyPreToolPermissionDecision,
  formatHookAdditionalContexts,
  runPermissionRequestHooks,
} from "../apps/zcode-cli/packages/core/src/tool/executor/hook-flow.ts";
import { previewHookValue } from "../apps/zcode-cli/packages/core/src/tool/executor/utils.ts";
import {
  injectHookAdditionalContextIntoMessageHistory,
  shouldContinueAfterStopHooks,
} from "../apps/zcode-cli/packages/core/src/runtime/methods/hooks.ts";
import {
  CALL,
  TRACE,
  capture,
  runtimeThis,
  toolDeps,
} from "./zcode-cli-rust-hook-fixture-helpers.mjs";

/** Node errors for a hook spec's `failure` (`kind:message`). */
function failureError(failure) {
  const [kind, message] = failure.split(/:(.*)/su);
  if (kind === "timeout") return createCoreError(CoreErrorType.ToolTimeout, message);
  if (kind === "execution")
    return createCoreError(CoreErrorType.ToolExecutionFailed, "Hook process failed", {
      cause: new Error(message),
    });
  if (kind === "configuration") return createCoreError(CoreErrorType.ConfigurationError, message);
  return new Error(message);
}

/**
 * Runs specs through Node's runner. A spec is `{output?, diagnostics?, failure?,
 * admission?, async?}`; the Rust test replays the same specs.
 */
async function runnerCase(name, event, specs, input = {}) {
  const events = [];
  const runner = new InMemoryHookRunner({
    emitEvent: async (e) => {
      events.push({ type: e.type, payload: e.payload });
    },
  });
  specs.forEach((spec, index) => {
    runner.register({
      event,
      source: `config.${event}.0.${index}`,
      sourceKind: "user",
      async: spec.async === true,
      ...(spec.admission ? { admission: () => spec.admission } : {}),
      descriptor: {
        clientVisible: true,
        commandDisplay: "hook",
        executionMode: spec.async ? "background" : "foreground",
        executionType: "command",
        sourceKind: "user",
        timeoutMs: 1000,
      },
      callback: () => {
        if (spec.failure) throw failureError(spec.failure);
        return spec.diagnostics
          ? { kind: "hookCallbackResult", output: spec.output, diagnostics: spec.diagnostics }
          : spec.output;
      },
    });
  });
  const result = await runner.run(
    { hookEventName: event, sessionId: "s", traceId: "t", turnId: "turn", cwd: "/w", ...input },
    { matchValue: input.toolName },
  );
  await new Promise((resolve) => setTimeout(resolve, 5));
  const runs = new Map();
  const normalized = events.map(({ type, payload }) => {
    if (!runs.has(payload.hookRunId)) runs.set(payload.hookRunId, `run-${runs.size}`);
    const clean = {
      ...payload,
      hookInvocationId: "inv",
      hookRunId: runs.get(payload.hookRunId),
      startedAt: 0,
    };
    if ("durationMs" in clean) clean.durationMs = 0;
    return { type, payload: JSON.parse(JSON.stringify(clean)) };
  });
  return {
    name,
    event,
    input,
    specs,
    result: JSON.parse(JSON.stringify(result)),
    events: normalized,
  };
}

async function runners() {
  const pre = (behavior, reason) => ({
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: behavior,
      ...(reason ? { permissionDecisionReason: reason } : {}),
    },
  });
  const permission = (decision) => ({
    output: { hookSpecificOutput: { hookEventName: "PermissionRequest", decision } },
  });
  const pending = { allowed: false, reasonCode: "workspace_hooks_pending_trust" };
  return [
    await runnerCase(
      "pre-deny-then-allow",
      "PreToolUse",
      [{ output: pre("deny", "A") }, { output: pre("allow", "B") }],
      { toolName: "Bash" },
    ),
    await runnerCase(
      "pre-ask-updated-input",
      "PreToolUse",
      [
        {
          output: {
            hookSpecificOutput: {
              ...pre("ask").hookSpecificOutput,
              updatedInput: { x: 1 },
              additionalContext: "c1",
            },
          },
        },
        { output: { additionalContext: "c2" } },
      ],
      { toolName: "Bash" },
    ),
    await runnerCase(
      "permission-last-wins",
      "PermissionRequest",
      [permission({ behavior: "deny", message: "no" }), permission({ behavior: "allow" })],
      { toolName: "Bash", requestId: "perm_1", toolCallId: "call-1" },
    ),
    await runnerCase(
      "prompt-prevent-erases-reason",
      "UserPromptSubmit",
      [{ output: { continue: false, stopReason: "x" } }, { output: { continue: false } }],
      { agentName: "main" },
    ),
    await runnerCase("stop-block", "Stop", [
      { output: { decision: "block", reason: "r", systemMessage: "s" } },
    ]),
    await runnerCase("stop-continue", "Stop", [
      { output: { continue: true, additionalContext: "more" } },
      { output: { continue: false, reason: "ignored" } },
    ]),
    await runnerCase(
      "wrong-event",
      "PostToolUse",
      [
        { output: { hookSpecificOutput: { hookEventName: "Stop" } } },
        { output: { additionalContext: "kept" } },
      ],
      { toolName: "Read" },
    ),
    await runnerCase(
      "failures",
      "PreToolUse",
      [
        { failure: "timeout:Hook timed out after 1000ms" },
        { failure: "execution:exit 1" },
        { failure: "configuration:Hook variable requires a skill context: ZCODE_SKILL_DIR" },
        { failure: "plain:plain token=abc" },
      ],
      { toolName: "Bash" },
    ),
    await runnerCase("blocked-diagnostics", "UserPromptSubmit", [
      {
        output: { continue: false, reason: "token=abc123 blocked" },
        diagnostics: { errorMessage: " err ", stderrPreview: " err ", stdoutPreview: "out" },
      },
    ]),
    await runnerCase(
      "decision-approve",
      "PermissionRequest",
      [
        { output: { decision: "approve", additionalContext: "ignored?" } },
        { output: { decision: "block", systemMessage: "sys" } },
      ],
      { toolName: "Bash" },
    ),
    await runnerCase(
      "admission",
      "PreToolUse",
      [
        { admission: pending },
        { admission: { ...pending, skipLifecycle: true } },
        { output: { additionalContext: "ran" } },
      ],
      { toolName: "Bash" },
    ),
    await runnerCase(
      "async",
      "PostToolUse",
      [
        { output: { additionalContext: "never" }, async: true },
        { output: { additionalContext: "sync" } },
      ],
      { toolName: "Read" },
    ),
  ];
}

async function permissionResults() {
  const out = [];
  const results = [
    { additionalContexts: [], preventContinuation: true, stopReason: "stop" },
    { additionalContexts: [], preventContinuation: true },
    { additionalContexts: [], permissionBehavior: "deny", stopReason: "why" },
    { additionalContexts: [], permissionBehavior: "deny" },
    { additionalContexts: [], permissionBehavior: "allow" },
    { additionalContexts: [], permissionBehavior: "ask" },
    { additionalContexts: [] },
    { additionalContexts: [], permissionRequestResult: { behavior: "deny" } },
    { additionalContexts: [], permissionRequestResult: { behavior: "deny", message: "m" } },
    {
      additionalContexts: [],
      permissionRequestResult: {
        behavior: "allow",
        updatedPermissions: [
          { type: "addRules", behavior: "allow", rules: [{ toolName: "Bash" }] },
        ],
      },
    },
    {
      additionalContexts: [],
      permissionRequestResult: {
        behavior: "allow",
        permissionUpdates: [],
        updatedPermissions: [{ type: "addRules", behavior: "deny", rules: [] }],
        updatedInput: { command: "pwd" },
      },
    },
    {
      additionalContexts: [],
      permissionBehavior: "deny",
      permissionRequestResult: { behavior: "allow" },
    },
  ];
  for (const result of results) {
    const { hookRunner, next } = capture();
    next.push(result);
    const answer = await runPermissionRequestHooks(
      toolDeps(hookRunner),
      CALL,
      {},
      "perm",
      {},
      "build",
      TRACE,
    );
    out.push({ result, answer: answer === undefined ? null : JSON.parse(JSON.stringify(answer)) });
  }
  return out;
}

function preDecisions() {
  const base = { allowed: true, decision: "allow", reason: "base", ruleId: "r", mode: "build" };
  const ask = { ...base, allowed: false, decision: "ask" };
  const cases = [
    [ask, "allow", "why"],
    [ask, "allow"],
    [{ ...ask, alwaysAsk: true }, "allow"],
    [base, "ask"],
    [base, "ask", "h"],
    [base, "deny"],
    [{ ...base, decision: "deny", allowed: false }, "allow"],
    [ask, "ask"],
    [base, undefined],
  ];
  return cases.map(([decision, behavior, reason]) => {
    const result = applyPreToolPermissionDecision(
      decision,
      {
        additionalContexts: [],
        permissionBehavior: behavior,
        hookPermissionDecisionReason: reason,
      },
      "edit",
    );
    return {
      decision,
      behavior: behavior ?? null,
      reason: reason ?? null,
      result: { decision: result.decision, reason: result.reason, ruleId: result.ruleId },
    };
  });
}

function texts() {
  const lifecycle = [];
  for (const [event, contexts] of [
    ["Stop", ["a", "b"]],
    ["SessionStart", ["only"]],
    ["UserPromptSubmit", ["z".repeat(24_010)]],
    ["Stop", []],
  ]) {
    const entry = injectHookAdditionalContextIntoMessageHistory.call(
      runtimeThis(),
      event,
      contexts,
    );
    lifecycle.push({
      event,
      contexts: contexts.map((c) => (c.length > 100 ? `z*${c.length}` : c)),
      body: entry?.content ?? null,
    });
  }
  const sanitize = [
    "curl https://user:pass@host/x",
    "Authorization: Bearer abc.def",
    "authorization: basic 'q w'",
    "https://x?a=1&api_key=SECRET&b=2",
    'AUTHORIZATION = "tok en"',
    '{"apiToken": "abc", "name": "x"}',
    "export GITHUB_TOKEN=ghp_123 other",
    "--password hunter2 --user me",
    "my_secret_value: xyz",
    "tokens are fine",
    "private-key 'k k'",
    "x\nclient_secret=abc\n",
    "plain text",
    "",
  ].map((text) => [text, sanitizeHookDisplayText(text)]);
  const descriptors = [
    [
      {
        type: "command",
        command: "echo ${ZCODE_SESSION_ID} TOKEN=abc",
        source: { kind: "user", path: "/u/config.json" },
        statusMessage: "Checking",
      },
      5000,
    ],
    [{ type: "command", command: "bg.sh", async: true, source: { kind: "user" } }, 60000],
    [
      {
        type: "process",
        command: "node",
        args: ["hook script.js", "--flag=1", 'a"b'],
        source: { kind: "project" },
      },
      1000,
    ],
    [
      {
        type: "process",
        command: "/bin/check",
        plugin: {
          id: "p1",
          name: "Plugin",
          rootPath: "/r",
          dataPath: "/d",
          sourcePath: "/r/hooks.json",
        },
      },
      2000,
    ],
  ].map(([hook, timeout]) => [
    hook,
    timeout,
    createHookExecutionDescriptor(hook, timeout, (v) => v.replace("${ZCODE_SESSION_ID}", "s-1")),
  ]);
  return {
    toolContexts: formatHookAdditionalContexts(["one", "two\nlines"]),
    lifecycle,
    previews: [["short"], [{ a: 1, b: [true, null] }], ["q".repeat(4001)], ["é".repeat(4001)]].map(
      ([v]) => [
        typeof v === "string" && v.length > 100 ? `${v[0]}*${v.length}` : v,
        previewHookValue(v),
      ],
    ),
    stopContinue: [
      [{ additionalContexts: ["x"], stopShouldContinue: true }, 0],
      [{ additionalContexts: ["x"], stopShouldContinue: true }, 3],
      [{ additionalContexts: [], stopShouldContinue: true }, 0],
      [{ additionalContexts: ["x"], stopShouldContinue: false }, 0],
    ].map(([result, count]) => [result, count, shouldContinueAfterStopHooks(result, count)]),
    sanitize,
    descriptors,
  };
}

export async function hookRunnerFixtures() {
  return {
    runners: await runners(),
    permission: await permissionResults(),
    preDecisions: preDecisions(),
    texts: texts(),
  };
}
