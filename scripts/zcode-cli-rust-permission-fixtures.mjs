// Permission parity cases for the Rust domain PolicyEngine, computed by the TS
// implementations. Imported by generate-zcode-cli-rust-fixtures.mjs.
import { readFileSync } from "node:fs";
import { builtInTools } from "../apps/zcode-cli/packages/core/src/tool/handlers/index.ts";
import { isWebFetchPreapprovedUrl } from "../apps/zcode-cli/packages/core/src/tool/webfetch-preapproved.ts";
import { PermissionService } from "../apps/zcode-cli/packages/core/src/permission/service.ts";
import { resolveRuntimePermissionCapability } from "../apps/zcode-cli/packages/core/src/tool/executor/permission-capability.ts";
import { buildDefaultPermissionUpdates } from "../apps/zcode-cli/packages/core/src/tool/executor/permission-suggestions.ts";
import {
  PERMISSION_DENIED_BY_USER_CONTENT,
  buildPermissionDeniedContent,
  buildProtocolPermissionOptions,
} from "../apps/zcode-cli/packages/bootstrap/src/permission-options.ts";

const PERMISSION_KEYS = [
  "permission",
  "riskLevel",
  "sideEffectScope",
  "needsApproval",
  "alwaysAsk",
  "askOptions",
];
const CAPABILITY_KEYS = [
  "allowedInPlanMode",
  "alwaysAsk",
  "readOnly",
  "destructive",
  "requiresUserInteraction",
  "sideEffectScope",
  "riskLevel",
  "needsApproval",
  "permissionCapabilityGroup",
];

function pick(source, keys) {
  return Object.fromEntries(
    keys.filter((key) => source?.[key] !== undefined).map((key) => [key, source[key]]),
  );
}

// 只保留权限相关字段；Rust 以同一形状表达 Node 的 PermissionToolCapability。
function capabilityOf(capability) {
  const trimmed = pick(capability, CAPABILITY_KEYS);
  if (capability?.permission) trimmed.permission = pick(capability.permission, PERMISSION_KEYS);
  return trimmed;
}

const ctx = { runtimeScope: "main", workingDirectory: "/w", workspaceRoot: "/w" };
const tools = Object.fromEntries(
  builtInTools.map((entry) => [
    entry.metadata.name,
    capabilityOf({ ...entry.metadata, permission: entry.permission }),
  ]),
);

const inputs = {
  Bash: [{ command: "ls -la" }, { command: "rm -rf build" }],
  Read: [{ file_path: "/etc/passwd" }],
  Write: [
    { file_path: "/w/src/a.ts", content: "x" },
    { file_path: "/w/.zcode/workflow-drafts/a.ts", content: "x" },
    { file_path: ".zcode/workflow-drafts/b.ts", content: "x" },
    { file_path: "/w/.zcode/workflow-drafts-other/a.ts", content: "x" },
  ],
  Edit: [{ file_path: "/w/src/a.ts", old_string: "a", new_string: "b" }],
  WebFetch: [{ url: "https://example.com/x", prompt: "p" }],
  AmendWorkflow: [{}, { predecessor: { owned_by_this_session: true, stop_reason: "completed" } }],
};
const states = [
  { mode: "build", planEnabled: false },
  { mode: "edit", planEnabled: false },
  { mode: "yolo", planEnabled: false },
  { mode: "auto", planEnabled: false },
  { mode: "build", planEnabled: true },
  { mode: "yolo", planEnabled: true },
  { mode: "edit", planEnabled: true },
];

function decide(entry, input, state, options = {}) {
  const config = {
    allowedTools: new Set(options.allowedTools ?? []),
    disallowedTools: new Set(options.disallowedTools ?? []),
    autoApproveHighRisk: options.autoApproveHighRisk ?? false,
    allowMediumRiskInAutoMode: false,
  };
  const service = new PermissionService(config);
  if (options.sessionRules) service.grantSessionPermission(options.sessionRules);
  const capability = resolveRuntimePermissionCapability(entry, input, ctx);
  const context = {
    toolName: entry.metadata.name,
    input,
    riskLevel: entry.metadata.riskLevel,
    workingDirectory: "/w",
    ...state,
  };
  const result = service.checkPermission(context, capability, options.projectRules ?? null);
  return {
    context,
    capability: capabilityOf(capability),
    config: {
      allowedTools: options.allowedTools ?? [],
      disallowedTools: options.disallowedTools ?? [],
      autoApproveHighRisk: config.autoApproveHighRisk,
    },
    projectRules: options.projectRules ?? null,
    sessionRules: options.sessionRules ?? [],
    expected: pick(result, [
      "decision",
      "ruleId",
      "reason",
      "riskLevel",
      "sideEffectScope",
      "alwaysAsk",
    ]),
  };
}

const entry = (name) => builtInTools.find((e) => e.metadata.name === name);
const decisions = [];
for (const tool of builtInTools) {
  for (const input of inputs[tool.metadata.name] ?? [{}]) {
    for (const state of states) decisions.push(decide(tool, input, state));
  }
}
const build = { mode: "build", planEnabled: false };
const plan = { mode: "build", planEnabled: true };
const yolo = { mode: "yolo", planEnabled: false };
const write = { file_path: "/w/src/a.ts", content: "x" };
const scenarios = [
  ["Write", write, build, { projectRules: { version: 1, deny: [{ toolName: "Write" }] } }],
  ["Write", write, yolo, { projectRules: { version: 1, deny: [{ toolName: "Write" }] } }],
  ["Write", write, build, { projectRules: { allow: [{ toolName: "Edit", ruleContent: "/w/src/*" }] } }],
  ["Write", write, plan, { projectRules: { allow: [{ toolName: "Write" }] } }],
  ["Write", write, build, { projectRules: { ask: [{ toolName: "Write", ruleContent: "/w/src/a.ts" }] } }],
  ["Read", { file_path: "/etc/passwd" }, build, { projectRules: { ask: [{ toolName: "Read", ruleContent: "/etc/*" }] } }],
  ["Read", { file_path: "/etc/pass\nwd" }, build, { projectRules: { ask: [{ toolName: "Read", ruleContent: "/etc/*" }] } }],
  ["Grep", { pattern: "foo", path: "/w" }, build, { projectRules: { deny: [{ toolName: "Grep", ruleContent: "foo:*" }] } }],
  ["Grep", { pattern: "foo bar", path: "/w" }, build, { projectRules: { deny: [{ toolName: "Grep", ruleContent: "foo:*" }] } }],
  ["Grep", { pattern: "foobar", path: "/w" }, build, { projectRules: { deny: [{ toolName: "Grep", ruleContent: "foo:*" }] } }],
  ["Grep", { pattern: "a.b", path: "/w" }, build, { projectRules: { deny: [{ toolName: "Grep", ruleContent: "a.*" }] } }],
  ["Grep", { pattern: "a(b", path: "/w" }, build, { projectRules: { deny: [{ toolName: "Grep", ruleContent: "a(*" }] } }],
  ["WebFetch", { url: "https://Example.COM./x", prompt: "p" }, build, { projectRules: { allow: [{ toolName: "WebFetch", ruleContent: "domain:example.com" }] } }],
  ["WebFetch", { url: "https://example.com/x", prompt: "p" }, build, { projectRules: { allow: [{ toolName: "WebFetch", ruleContent: "https://example.com/x" }] } }],
  ["WebFetch", { url: "https://a.example.com/x", prompt: "p" }, build, { projectRules: { allow: [{ toolName: "WebFetch", ruleContent: "domain:*.example.com" }] } }],
  ["Write", write, build, { disallowedTools: ["Write"] }],
  ["Write", write, yolo, { disallowedTools: ["Write"] }],
  ["Write", write, build, { allowedTools: ["Write"] }],
  ["Write", write, plan, { allowedTools: ["Write"] }],
  ["AskUserQuestion", { questions: [] }, yolo, { disallowedTools: ["AskUserQuestion"] }],
  ["CreateWorkflow", {}, build, { sessionRules: [{ behavior: "allow", type: "addRules", rules: [{ toolName: "CreateWorkflow" }] }] }],
  ["CreateWorkflow", {}, yolo, { disallowedTools: ["CreateWorkflow"] }],
  ["CreateWorkflow", {}, yolo, { projectRules: { deny: [{ toolName: "CreateWorkflow" }] } }],
  ["js", { code: "1" }, build, { autoApproveHighRisk: true }],
  ["Write", "raw string input", build, { projectRules: { allow: [{ toolName: "Write", ruleContent: "raw*" }] } }],
];
for (const [name, input, state, options] of scenarios) {
  decisions.push(decide(entry(name), input, state, options));
}

const optionCases = [
  { toolName: "Write", input: { file_path: "/w/a.ts" } },
  { toolName: "Bash", input: { command: "npm test" }, suggestedPermissionUpdates: [{ behavior: "allow", type: "addRules", rules: [{ toolName: "Bash", ruleContent: "npm test:*" }] }] },
  { toolName: "CreateWorkflow", input: {}, optionsPolicy: "session-always-allow" },
  { toolName: "SaveWorkflow", input: {}, optionsPolicy: "no-always-allow" },
  { toolName: "WebFetch", input: { url: " " } },
  { toolName: "Grep", input: "raw" },
];
const options = optionCases.map((source) => ({
  source,
  expected: buildProtocolPermissionOptions(source),
}));
const updateCases = [
  ["Write", { file_path: "/w/a.ts", content: "x" }],
  ["Grep", { pattern: " ", path: "/w" }],
  ["Grep", "  "],
  ["WebFetch", { url: "https://example.com/a" }],
  ["TodoWrite", { todos: [] }],
];
const defaultUpdates = updateCases.map(([toolName, input]) => ({
  toolName,
  input,
  expected: buildDefaultPermissionUpdates(toolName, input),
}));

// 预批准表在 Node 中不导出：从源码提取，再用 isWebFetchPreapprovedUrl 的结果校验提取正确。
const webfetchSource = readFileSync(
  new URL("../apps/zcode-cli/packages/core/src/tool/webfetch-preapproved.ts", import.meta.url),
  "utf8",
);
const hostBlock = /const PREAPPROVED_HOSTS = new Set\(\[([\s\S]*?)\]\);/.exec(webfetchSource)[1];
const prefixBlock = /const PREAPPROVED_PATH_PREFIXES = new Map\(\[([\s\S]*?)\]\);\n/.exec(
  webfetchSource,
)[1];
const webfetch = {
  hosts: [...hostBlock.matchAll(/"([^"]+)"/g)].map((m) => m[1]),
  pathPrefixes: Object.fromEntries(
    [...prefixBlock.matchAll(/\["([^"]+)", \[([^\]]*)\]\]/g)].map((m) => [
      m[1],
      [...m[2].matchAll(/"([^"]+)"/g)].map((p) => p[1]),
    ]),
  ),
};
const webfetchUrls = [
  ...webfetch.hosts.map((host) => `https://${host}/x`),
  "https://docs.python.org./x",
  "https://DOCS.PYTHON.ORG/3/",
  "https://wordpress.org/documentation",
  "https://wordpress.org/documentation/x",
  "https://wordpress.org/documentationx",
  "https://wordpress.org/documentation/%2e%2e/x",
  "https://wordpress.org/documentation/%252F",
  "https://huggingface.co/docs/x",
  "https://example.com/docs",
  "not a url",
];
const webfetchCases = webfetchUrls.map((url) => ({
  url,
  expected: isWebFetchPreapprovedUrl(url),
}));

/// Runtime data embedded in the Rust binary.
export function permissionData() {
  return {
    tools,
    webfetch,
    texts: { deniedByUser: PERMISSION_DENIED_BY_USER_CONTENT },
  };
}

export function permissionFixtures() {
  return {
    texts: {
      deniedWithFeedback: buildPermissionDeniedContent("  use pnpm  "),
    },
    decisions,
    options,
    defaultUpdates,
    webfetchCases,
  };
}
