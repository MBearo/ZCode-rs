// Legacy session/create, session/resume and setter params parity cases, computed by the
// shared zod schemas and the Node `parseParams` formatter (server-types.ts).
// Imported by generate-zcode-cli-rust-fixtures.mjs.
import {
  zcodeSessionCreateParamsSchema,
  zcodeSessionResumeParamsSchema,
  zcodeSessionSetModeParamsSchema,
  zcodeSessionSetModelParamsSchema,
  zcodeSessionSetThoughtLevelParamsSchema,
} from "../packages/shared/src/zcode-protocol/index.ts";

/** Node `summarizeParamsError` + `parseParams`. */
function protocolError(error) {
  const issues = error.issues;
  const parts = issues.slice(0, 5).map((issue) => {
    const path = issue.path?.length ? issue.path.join(".") : "(root)";
    return `${path}: ${issue.message ?? "invalid"}`;
  });
  const detail = parts.join("; ") + (issues.length > 5 ? ` (+${issues.length - 5} more)` : "");
  return {
    code: -32602,
    message: detail ? `Invalid params — ${detail}` : "Invalid params",
    data: JSON.parse(JSON.stringify(error)),
  };
}

const ws = { workspacePath: "/w", workspaceKey: "/w" };
const stdio = { name: "fs", command: "npx", args: ["-y", "x"], env: [{ name: "A", value: "1" }] };
const http = { name: "web", type: "http", url: "https://x", headers: [] };
const sha = "a".repeat(64);
const shared = {
  source: "sharedContext",
  title: "t",
  markdown: "m",
  provenance: {
    shareId: "s",
    projectionSha256: sha,
    artifactSetSha256: sha,
    formatterVersion: 1,
    markdownSha256: sha,
    installedArtifacts: [],
  },
};
const claude = { source: "claudeCode", messages: [{ role: "user", content: "hi" }] };

const CREATE = [
  undefined,
  null,
  [],
  1,
  "x",
  {},
  { workspace: ws },
  {
    workspace: { ...ws, workspacePath: "  /w  ", workspaceIdentity: " id ", remoteSessionId: "r" },
  },
  { workspace: ws, bogus: 1 },
  { workspace: ws, a: 1, b: 2 },
  { workspace: { ...ws, extra: 1 } },
  { workspace: { workspacePath: "/w" } },
  { workspace: { workspacePath: "  ", workspaceKey: "/w" } },
  { workspace: { workspacePath: 1, workspaceKey: "/w" } },
  { workspace: { workspacePath: "/w", workspaceKey: null } },
  { workspace: ws, sessionId: "" },
  { workspace: ws, sessionId: " s1 " },
  { workspace: ws, parentSessionId: 3 },
  ...["plan", "build", "edit", "yolo", "auto", "turbo", 1].map((mode) => ({ workspace: ws, mode })),
  ...["immediate", "deferred", "later"].map((persistence) => ({ workspace: ws, persistence })),
  { workspace: ws, model: { providerId: "p", modelId: "m" } },
  {
    workspace: ws,
    model: { providerId: " p ", modelId: " m ", options: { reasoningLevel: " low " } },
  },
  { workspace: ws, model: { providerId: "p" } },
  { workspace: ws, model: { providerId: "p", modelId: "m", x: 1 } },
  {
    workspace: ws,
    model: { providerId: "p", modelId: "m", options: { reasoningLevel: "low", y: 1 } },
  },
  { workspace: ws, model: { providerId: "p", modelId: "m", options: {} } },
  { workspace: ws, model: { providerId: "p", modelId: "m", options: { reasoningLevel: "" } } },
  { workspace: ws, model: "p/m" },
  { workspace: ws, thoughtLevel: " " },
  { workspace: ws, thoughtLevel: " high " },
  { workspace: ws, titleGenerationEnabled: "no" },
  { workspace: ws, titleGenerationEnabled: false },
  { workspace: ws, toolAllowlist: ["Read", ""] },
  { workspace: ws, toolAllowlist: [" Read "], toolDenylist: ["Bash"] },
  { workspace: ws, toolDenylist: "Bash" },
  { workspace: ws, mcpServers: [] },
  { workspace: ws, mcpServers: [stdio, http] },
  { workspace: ws, mcpServers: [{ name: "x" }] },
  { workspace: ws, mcpServers: [{ ...stdio, env: [{ name: "", value: "1" }] }] },
  { workspace: ws, mcpServers: [{ ...http, type: "ws" }] },
  { workspace: ws, mcpServers: [{ ...stdio, timeoutMs: 0 }] },
  {
    workspace: ws,
    mcpServers: [
      { ...http, oauth: { type: "client_credentials", clientId: "c", clientSecret: "s" } },
    ],
  },
  { workspace: ws, mcpServers: "x" },
  { workspace: ws, offPeakToolEnabled: true, dynamicWorkflowEnabled: false },
  { workspace: ws, offPeakToolEnabled: 1 },
  { workspace: ws, importedHistory: claude },
  {
    workspace: ws,
    sessionId: "s",
    importedHistory: { ...claude, title: " T ", createdAt: 1, updatedAt: 2 },
  },
  { workspace: ws, importedHistory: { source: "cursor", messages: [] } },
  { workspace: ws, importedHistory: { messages: [] } },
  { workspace: ws, importedHistory: { source: "claudeCode", messages: [] } },
  { workspace: ws, importedHistory: { source: "claudeCode" } },
  {
    workspace: ws,
    importedHistory: { source: "claudeCode", messages: [{ role: "system", content: "x" }] },
  },
  {
    workspace: ws,
    importedHistory: { source: "claudeCode", messages: [{ role: "user", content: "" }] },
  },
  {
    workspace: ws,
    importedHistory: {
      source: "claudeCode",
      messages: [{ role: "user", content: "x", timestamp: -1 }],
    },
  },
  {
    workspace: ws,
    importedHistory: {
      source: "claudeCode",
      messages: [{ role: "user", content: "x", timestamp: 1.5 }],
    },
  },
  {
    workspace: ws,
    importedHistory: { source: "claudeCode", messages: [{ role: "user", content: "x", id: "m1" }] },
  },
  { workspace: ws, importedHistory: { ...claude, createdAt: "1" } },
  { workspace: ws, importedHistory: shared },
  { workspace: ws, importedHistory: { ...shared, title: " " } },
  {
    workspace: ws,
    importedHistory: { ...shared, provenance: { ...shared.provenance, projectionSha256: "x" } },
  },
  {
    workspace: ws,
    importedHistory: { ...shared, provenance: { ...shared.provenance, shareUrl: "not a url" } },
  },
  {
    workspace: ws,
    importedHistory: { ...shared, provenance: { ...shared.provenance, shareUrl: "https://x/s" } },
  },
  {
    workspace: ws,
    importedHistory: { ...shared, provenance: { ...shared.provenance, formatterVersion: 2 } },
  },
  {
    workspace: ws,
    importedHistory: { ...claude, messages: [{ role: "user", content: "x", timestamp: -1.5 }] },
  },
  {
    workspace: ws,
    importedHistory: { ...claude, messages: [{ role: "user", content: "x", timestamp: 1e20 }] },
  },
  {
    workspace: ws,
    importedHistory: { ...claude, messages: [{ role: "user", content: "x", timestamp: 1.0 }] },
  },
  { workspace: ws, importedHistory: "x" },
  { workspace: ws, importedHistory: null },
  { workspace: ws, mcpServers: [{ name: " ", command: " ", args: [], env: [] }] },
  { workspace: ws, mcpServers: [{ ...stdio, timeoutMs: 1.5 }] },
  { workspace: ws, mcpServers: [1] },
  { workspace: ws, mcpServers: [{ ...http, oauth: { type: "x" } }] },
  {
    workspace: ws,
    mcpServers: [{ ...http, oauth: { type: "authorization_code", redirectPath: " " } }],
  },
  { workspace: ws, toolAllowlist: [1, "", 2] },
  { workspace: { workspacePath: "", workspaceKey: "", extra: 1 } },
  { workspace: ws, model: { providerId: "", modelId: "", options: null } },
  { workspace: ws, thoughtLevel: "\ufeff high\u2028" },
  { workspace: 1, mode: 1, model: 1, persistence: 1, thoughtLevel: 1, titleGenerationEnabled: 1 },
  {
    workspace: 1,
    mode: 1,
    model: 1,
    persistence: 1,
    thoughtLevel: 1,
    titleGenerationEnabled: 1,
    extra: 1,
  },
];

const RESUME = [
  undefined,
  null,
  {},
  { sessionId: "s" },
  {
    sessionId: " s ",
    workspace: ws,
    thoughtLevel: "high",
    mcpServers: [stdio],
    toolAllowlist: ["Read"],
    toolDenylist: [],
    offPeakToolEnabled: false,
    dynamicWorkflowEnabled: true,
  },
  { sessionId: "s", mode: "plan" },
  { sessionId: "s", persistence: "deferred" },
  { sessionId: "s", model: { providerId: "p", modelId: "m" } },
  { sessionId: "s", titleGenerationEnabled: false },
  { sessionId: "   " },
  { sessionId: 1 },
  { sessionId: "s", workspace: { workspacePath: "/w" } },
];

const SET_MODEL = [
  undefined,
  {},
  { sessionId: "s", model: { providerId: "p", modelId: "m" } },
  {
    sessionId: " s ",
    model: { providerId: " p ", modelId: " m ", options: { reasoningLevel: " high " } },
    expectedRevision: 0,
    persistAsWorkspaceLastUsed: false,
  },
  { sessionId: "s" },
  { sessionId: "s", model: { providerId: "p" } },
  { sessionId: "s", model: { providerId: "p", modelId: "m", x: 1 } },
  { sessionId: "s", model: { providerId: "p", modelId: "m" }, expectedRevision: -1 },
  { sessionId: "s", model: { providerId: "p", modelId: "m" }, expectedRevision: 1.5 },
  { sessionId: "s", model: { providerId: "p", modelId: "m" }, persistAsWorkspaceLastUsed: 1 },
  { sessionId: "s", model: "p/m", thoughtLevel: "high" },
];

const SET_THOUGHT_LEVEL = [
  undefined,
  { sessionId: "s" },
  { sessionId: " s ", thoughtLevel: " high ", expectedRevision: 3 },
  { sessionId: "s", thoughtLevel: "  " },
  { sessionId: "s", thoughtLevel: 1, expectedRevision: "1" },
  { sessionId: "", model: { providerId: "p", modelId: "m" } },
];

const SET_MODE = [
  undefined,
  ...["plan", "build", "edit", "yolo", "auto", "turbo", 1].map((mode) => ({
    sessionId: "s",
    mode,
  })),
  { sessionId: "s" },
  { sessionId: "s", mode: "auto", expectedRevision: 2 },
  { sessionId: "s", mode: "auto", persistAsWorkspaceLastUsed: true },
];

function run(schema, inputs) {
  return inputs.map((input) => {
    const result = schema.safeParse(input);
    return result.success
      ? {
          input: input ?? null,
          missing: input === undefined,
          ok: JSON.parse(JSON.stringify(result.data)),
        }
      : { input: input ?? null, missing: input === undefined, error: protocolError(result.error) };
  });
}

export function legacyParamsFixtures() {
  return {
    create: run(zcodeSessionCreateParamsSchema, CREATE),
    resume: run(zcodeSessionResumeParamsSchema, RESUME),
    setModel: run(zcodeSessionSetModelParamsSchema, SET_MODEL),
    setThoughtLevel: run(zcodeSessionSetThoughtLevelParamsSchema, SET_THOUGHT_LEVEL),
    setMode: run(zcodeSessionSetModeParamsSchema, SET_MODE),
  };
}
