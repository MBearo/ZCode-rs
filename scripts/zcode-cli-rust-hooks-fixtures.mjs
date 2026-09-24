// Hook parity fixtures for the Rust `domain::hooks`, produced by running the TS
// hook modules on fixed inputs. Imported by generate-zcode-cli-rust-fixtures.mjs.
import { readFile } from "node:fs/promises";
import { CoreErrorType, createCoreError } from "../apps/zcode-cli/packages/contracts/src/index.ts";
import {
  createCompatibleHookStdin,
  createPluginEnvOverlay,
  expandPluginVariables,
} from "../apps/zcode-cli/packages/core/src/hooks/configured-runner-input.ts";
import { matchesAnyHookMatcher } from "../apps/zcode-cli/packages/core/src/hooks/runner-helpers.ts";
import {
  runPermissionRequestHooks,
  runPostToolUseFailureHooks,
  runPostToolUseHooks,
  runPreToolUseHooks,
} from "../apps/zcode-cli/packages/core/src/tool/executor/hook-flow.ts";
import {
  runSessionStartHooks,
  runStopHooks,
  runUserPromptSubmitHooks,
} from "../apps/zcode-cli/packages/core/src/runtime/methods/hooks.ts";
import { callbacks } from "./zcode-cli-rust-hook-callback-fixtures.mjs";
import {
  CALL,
  TRACE,
  capture,
  runtimeThis,
  toolDeps,
} from "./zcode-cli-rust-hook-fixture-helpers.mjs";
import { hookRunnerFixtures } from "./zcode-cli-rust-hook-runner-fixtures.mjs";

async function inputs() {
  const { seen, hookRunner } = capture();
  const deps = toolDeps(hookRunner);
  const entry = { metadata: { riskLevel: "low", sideEffectScope: "none" } };
  await runPreToolUseHooks(deps, CALL, { command: "ls" }, entry, "build", TRACE);
  await runPreToolUseHooks(
    deps,
    { id: "c2", name: "Agent" },
    {},
    { metadata: { riskLevel: "high" } },
    "yolo",
    { traceId: "t" },
  );
  const decision = { riskLevel: "high", sideEffectScope: "system" };
  await runPermissionRequestHooks(
    deps,
    CALL,
    { command: "rm x" },
    "perm_1",
    decision,
    "build",
    TRACE,
  );
  await runPermissionRequestHooks(
    deps,
    { id: "c3", name: "ApplyPatch" },
    {},
    "perm_2",
    { ...decision, reason: "Needs approval" },
    "edit",
    TRACE,
  );
  await runPostToolUseHooks(
    deps,
    CALL,
    { command: "ls" },
    { exitCode: 0, stdout: "a\n" },
    undefined,
    TRACE,
  );
  await runPostToolUseHooks(deps, CALL, {}, "x".repeat(4001), "/artifacts/out.txt", TRACE);
  await runPostToolUseFailureHooks(deps, CALL, { command: "ls" }, new Error("boom"), TRACE);
  await runPostToolUseFailureHooks(
    deps,
    CALL,
    {},
    createCoreError(CoreErrorType.ToolCancelled, "Cancelled"),
    TRACE,
  );
  const runtime = runtimeThis(hookRunner);
  await runSessionStartHooks.call(runtime, "startup", TRACE);
  await runSessionStartHooks.call(
    { ...runtimeThis(hookRunner), getSessionModelSelection: () => undefined },
    "resume",
    TRACE,
  );
  await runUserPromptSubmitHooks.call(runtime, "hello", undefined, TRACE);
  await runUserPromptSubmitHooks.call(
    runtime,
    "see files",
    [{ type: "file", path: "/a.txt" }, { type: "image", content: "abcd" }, { type: "file" }],
    TRACE,
  );
  await runStopHooks.call(runtime, "done", 2, TRACE);
  await runStopHooks.call(runtime, "y".repeat(4005), 0, TRACE, undefined, true);
  const cases = [];
  for (const { input, options } of seen) {
    const stdin = await createCompatibleHookStdin(input);
    const path = JSON.parse(stdin.value).transcript_path;
    const transcript = await readFile(path, "utf8");
    await stdin.cleanup();
    const values = [
      ...(options.matchValues ?? []),
      ...(options.matchValue ? [options.matchValue] : []),
    ];
    cases.push({
      input,
      values: [...new Set(values)],
      stdin: stdin.value.replaceAll(path, "<transcript>"),
      transcript,
    });
  }
  return cases;
}

const MATCHERS = [
  [null, ["Bash"]],
  ["", ["Bash"]],
  ["*", ["Bash"]],
  ["Bash", ["Bash"]],
  ["Edit|Write", ["Write"]],
  ["Edit|Write", ["ApplyPatch", "Write", "Edit"]],
  ["Edit|", ["Edit"]],
  ["Bash.*", ["Bash"]],
  ["ash", ["Bash"]],
  ["^Bash$", ["Bash"]],
  ["mcp__.*", ["mcp__fs__read"]],
  ["(", ["Bash"]],
  ["[", ["Bash"]],
  ["Bash", []],
  ["Bash", [""]],
  ["[a-z]+", ["BASH"]],
  ["\\bRead\\b", ["NotebookRead"]],
  ["Task", ["Agent", "Task"]],
  ["\\d+", ["Tool1"]],
  ["(?<n>Bash)", ["Bash"]],
  ["Bash(?=x)", ["Bashx"]],
  ["^(Read|Grep)$", ["Grep"]],
  ["bash", ["Bash"]],
  ["Write|Edit", ["Read"]],
  ["\\w+__\\w+", ["mcp__x"]],
  ["startup", ["startup"]],
];

function matchers() {
  return MATCHERS.map(([matcher, values]) => [
    matcher,
    values,
    matchesAnyHookMatcher({ matchValues: values }, matcher ?? undefined),
  ]);
}

function expansions() {
  const input = { sessionId: "s-1", cwd: "/work", hookEventName: "PreToolUse" };
  const plugin = { dataPath: "/data", id: "p", name: "Plug", rootPath: "/root" };
  const values = [
    "${CLAUDE_PLUGIN_ROOT}/run.sh ${ZCODE_SESSION_ID}",
    "cd ${CLAUDE_PROJECT_DIR} && ${ZCODE_PROJECT_DIR}",
    "${CLAUDE_SESSION_ID}${CLAUDE_CODE_SESSION_ID}${UNKNOWN} $ZCODE_SESSION_ID",
    "${ZCODE_PLUGIN_DATA}/${ZCODE_PLUGIN_ROOT}/${CLAUDE_PLUGIN_DATA}",
    "run ${ZCODE_SKILL_DIR}",
  ];
  const out = [];
  for (const withPlugin of [false, true]) {
    for (const value of values) {
      try {
        out.push({
          value,
          plugin: withPlugin,
          result: expandPluginVariables(value, withPlugin ? plugin : undefined, input, "/fallback"),
        });
      } catch (error) {
        out.push({ value, plugin: withPlugin, error: error.message });
      }
    }
  }
  out.push({
    value: "${ZCODE_PROJECT_DIR}",
    plugin: false,
    cwd: "",
    result: expandPluginVariables(
      "${ZCODE_PROJECT_DIR}",
      undefined,
      { ...input, cwd: "" },
      "/fallback",
    ),
  });
  return {
    cases: out,
    env: [
      createPluginEnvOverlay(undefined, input, "/fallback"),
      createPluginEnvOverlay(plugin, input, "/fallback"),
    ],
  };
}

export async function hookFixtures() {
  const inputCases = await inputs();
  return {
    inputs: inputCases,
    matchers: matchers(),
    expansions: expansions(),
    callbacks: await callbacks(inputCases),
    ...(await hookRunnerFixtures()),
  };
}
