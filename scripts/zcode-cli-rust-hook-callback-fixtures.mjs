// Hook process results through Node's configured callback (exit codes,
// stdout parsing and schema validation). Imported by the hook fixtures.
import { createConfiguredHookCallback } from "../apps/zcode-cli/packages/core/src/hooks/configured-runner-callback.ts";

const EVENTS = [
  "SessionStart",
  "UserPromptSubmit",
  "PreToolUse",
  "PermissionRequest",
  "PostToolUse",
  "PostToolUseFailure",
  "Stop",
];
const LONG = "e".repeat(4100);
const RESULTS = [
  ["PreToolUse", 0, "", ""],
  ["PreToolUse", 0, "hello", ""],
  ["PreToolUse", 0, '{"continue":false,"reason":"r","extra":1}', ""],
  ["PreToolUse", 0, "{bad json", ""],
  ["PreToolUse", 0, '{"continue":"no"}', ""],
  ["PreToolUse", 0, "[1]", ""],
  [
    "PreToolUse",
    0,
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"x","updatedInput":{"a":1},"junk":2}}',
    "",
  ],
  ["PreToolUse", 0, ' \n {"decision":"approve"} \n', "warn "],
  ["PreToolUse", 0, '{"a":1}', ""],
  [
    "PreToolUse",
    0,
    '{"hookSpecificOutput":{"hookEventName":"PreToolUse","updatedInput":null}}',
    "",
  ],
  ["PreToolUse", 0, '{"hookSpecificOutput":{"hookEventName":"Nope"}}', ""],
  ["PreToolUse", 0, '{"decision":"maybe"}', ""],
  ["PreToolUse", 0, '{"reason":null}', ""],
  ["PreToolUse", 0, '{"hookSpecificOutput":null}', ""],
  [
    "PreToolUse",
    0,
    '{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"c","permissionDecision":"allow"}}',
    "",
  ],
  [
    "PermissionRequest",
    0,
    '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","updatedPermissions":[{"type":"addRules","behavior":"allow","rules":[{"toolName":"Bash","ruleContent":"ls","x":1}]}],"updatedInput":{"command":"ls"}}}}',
    "",
  ],
  [
    "PermissionRequest",
    0,
    '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","permissionUpdates":[{"type":"addRules","behavior":"allow","rules":[{"toolName":""}]}]}}}',
    "",
  ],
  [
    "PermissionRequest",
    0,
    '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"no","interrupt":true}}}',
    "",
  ],
  [
    "PermissionRequest",
    0,
    '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"maybe"}}}',
    "",
  ],
  [
    "Stop",
    0,
    '{"decision":"block","reason":"again","systemMessage":"sys","suppressOutput":true,"stopReason":"s"}',
    "",
  ],
  [
    "UserPromptSubmit",
    0,
    '{"additionalContext":"a","additional_context":"b","hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"c"}}',
    "",
  ],
  ...EVENTS.map((event) => [event, 2, "out", "blocked!\n"]),
  ["PreToolUse", 2, "", ""],
  ["PreToolUse", 2, "  only stdout ", ""],
  ["Stop", 2, "", LONG],
  ["PreToolUse", 1, "", "bad"],
  ["PreToolUse", 1, "out", ""],
  ["PreToolUse", 1, "", "", "Spawn failed"],
  ["PreToolUse", 0, "", "", "Command timed out after 1m", "timed_out"],
  ["PreToolUse", undefined, "", "", undefined, "cancelled"],
  ["PreToolUse", 2, "", "late", undefined, "timed_out"],
];

export async function callbacks(inputCases) {
  const byEvent = new Map();
  for (const { input } of inputCases)
    if (!byEvent.has(input.hookEventName)) byEvent.set(input.hookEventName, input);
  const out = [];
  for (const [event, exitCode, stdout, stderr, errorMessage, status] of RESULTS) {
    const result = {
      status: status ?? (exitCode === 0 ? "completed" : "failed"),
      ...(exitCode === undefined ? {} : { exitCode }),
      stdout: { text: stdout },
      stderr: { text: stderr },
      ...(errorMessage ? { error: { message: errorMessage } } : {}),
    };
    const options = {
      executionPort: { run: async () => result },
      getWorkingDirectory: () => "/work",
    };
    const callback = createConfiguredHookCallback(
      options,
      event,
      { type: "command", command: "x" },
      0,
      0,
      { maxOutputBytes: 64, timeoutMs: 1000 },
    );
    const record = {
      event,
      status: result.status,
      exitCode: exitCode ?? null,
      stdout,
      stderr,
      error: errorMessage ?? null,
    };
    try {
      record.output = (await callback(byEvent.get(event), { hookIndex: 0 })) ?? null;
    } catch (error) {
      record.failure = {
        message: error.message,
        cause: error.cause?.message ?? null,
        code: error.code,
      };
    }
    out.push(record);
  }
  return out;
}
