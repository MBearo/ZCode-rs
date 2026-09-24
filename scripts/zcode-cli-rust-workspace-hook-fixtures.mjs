// Workspace hook trust fixtures for the Rust `domain::hooks::{workspace,trust}`,
// produced by the TS implementations. Imported by generate-zcode-cli-rust-fixtures.mjs.
import { createWorkspaceHookBundleSnapshot } from "../apps/zcode-cli/packages/contracts/src/hooks/workspace-hook-trust.ts";
import { evaluateWorkspaceHookEntry } from "../apps/zcode-cli/packages/core/src/hooks/workspace-hook-trust-evaluation.ts";
import { createWorkspaceHookTrustRecords } from "../apps/zcode-cli/packages/core/src/hooks/workspace-hook-trust-records.ts";
import { buildWorkspaceHookReviewRequest } from "../apps/zcode-cli/packages/bootstrap/src/app/workspace-hook-review-request.ts";
import {
  createWorkspaceHookSourceInput,
  resolveWorkspaceHookRuntimeRoot,
} from "../packages/shared/src/workspace-hook-config.ts";
import { buildWorkspaceHookBundleSnapshot } from "../packages/shared/src/workspace-hook-digest.ts";
import { parseWorkspaceHookTrustStoreContent } from "../packages/shared/src/workspace-hook-trust-store-file.ts";

const WORKSPACE = "/w";
const command = (value, extra = {}) => ({ type: "command", command: value, ...extra });
const CASES = [
  {
    name: "research",
    roots: [{ enabled: true }],
    sources: [
      {
        path: "/w/.zcode/config.json",
        order: 0,
        hooks: {
          enabled: true,
          events: {
            PreToolUse: [{ matcher: "Bash", hooks: [command("echo hi", { timeout: 5 })] }],
            Stop: [
              { hooks: [{ type: "process", command: "node", args: ["stop.js"], enabled: false }] },
            ],
          },
        },
      },
    ],
  },
  {
    name: "shapes",
    roots: [
      { enabled: false, timeoutMs: 1500.5 },
      { enabled: true, maxOutputBytes: 4096.4 },
    ],
    sources: [
      {
        path: "/w/zcode.json",
        order: 1,
        hooks: {
          timeoutMs: 2.5,
          maxOutputBytes: 1e21,
          events: {
            UserPromptSubmit: [
              {
                hooks: [
                  command("  ./check.sh  ", {
                    shell: "/bin/bash",
                    async: true,
                    statusMessage: " Checking ",
                  }),
                  command("x", { shell: true, timeoutMs: 1e-7 }),
                ],
              },
            ],
            PostToolUse: [
              { matcher: "Edit|Write", hooks: [{ type: "process", command: "fmt", args: [] }] },
              {
                hooks: [
                  {
                    type: "process",
                    command: "lint",
                    args: ["--fix", "a b"],
                    timeoutMs: 123456.789,
                  },
                ],
              },
            ],
          },
        },
      },
      {
        path: "/other/.zcode/config.json",
        order: 3,
        hooks: {
          enabled: false,
          events: { SessionStart: [{ matcher: "startup", hooks: [command("boot")] }] },
        },
      },
    ],
  },
];

function build(testCase) {
  const sources = testCase.sources.map((s) =>
    createWorkspaceHookSourceInput({
      path: s.path,
      workingDirectory: WORKSPACE,
      hooks: s.hooks,
      discoveryOrder: s.order,
    }),
  );
  const runtimeRoot = resolveWorkspaceHookRuntimeRoot([
    { enabled: false, timeoutMs: 60000, maxOutputBytes: 32768 },
    ...testCase.roots,
  ]);
  const data = buildWorkspaceHookBundleSnapshot({
    workspaceIdentity: " id-1 ",
    workspacePath: WORKSPACE,
    sources,
    runtimeRoot,
    discoveredAt: "2026-01-01T00:00:00.000Z",
  });
  return { runtimeRoot, snapshot: createWorkspaceHookBundleSnapshot(data) };
}

const key = (identity, digest) => `${identity}\u0000${digest}`;

function evaluations(snapshot) {
  const [first, second] = snapshot.hooks;
  const record = (entry, extra = {}) => ({
    workspaceIdentity: snapshot.workspaceIdentity,
    hookDeclarationDigest: entry.hookDeclarationDigest,
    digestAlgorithm: "sha256",
    decision: "trusted",
    grantedAt: "2026-01-01T00:00:00.000Z",
    eventAtGrant: entry.event,
    displayCommandAtGrant: entry.command,
    sourcePathAtGrant: entry.sourceRelativePath,
    sourceDiscoveryOrderAtGrant: snapshot.sourceFiles[entry.sourceFileIndex].discoveryOrder,
    matcherAtGrant: entry.matcher,
    matcherIndexAtGrant: entry.matcherIndex,
    hookIndexAtGrant: entry.hookIndex,
    ...extra,
  });
  const stale = record(first, { hookDeclarationDigest: "f".repeat(64) });
  const scenarios = [
    { name: "pending", policy: "user_decides", records: [], revoked: [], corrupt: false },
    {
      name: "trusted",
      policy: "user_decides",
      records: [record(first), record(second)],
      revoked: [],
      corrupt: false,
    },
    {
      name: "revoked",
      policy: "user_decides",
      records: [],
      revoked: [first.hookDeclarationDigest],
      corrupt: false,
    },
    { name: "stale", policy: "user_decides", records: [stale], revoked: [], corrupt: false },
    {
      name: "corrupt",
      policy: "user_decides",
      records: [record(first)],
      revoked: [],
      corrupt: true,
    },
    { name: "deny", policy: "deny", records: [record(first)], revoked: [], corrupt: false },
    {
      name: "trusted-only",
      policy: "allow_trusted_only",
      records: [record(first)],
      revoked: [],
      corrupt: false,
    },
  ];
  return scenarios.map((scenario) => {
    const persistentRecords = new Map(
      scenario.records.map((r) => [key(r.workspaceIdentity, r.hookDeclarationDigest), r]),
    );
    const revokedKeys = new Set(scenario.revoked.map((d) => key(snapshot.workspaceIdentity, d)));
    const policy = { mode: scenario.policy, policyRevision: "p" };
    const items = snapshot.hooks.map((entry) =>
      evaluateWorkspaceHookEntry({
        entry,
        snapshot,
        policy,
        persistentRecords,
        revokedKeys,
        storeStatus: scenario.corrupt ? "corrupt" : "ok",
      }),
    );
    return {
      ...scenario,
      items: items.map((i) => [i.reviewItemId, i.trustState, i.effectiveRunnable]),
    };
  });
}

const STORES = [
  "",
  "not json",
  '{"schemaVersion":1,"records":[]}',
  '{"schemaVersion":2,"records":[]}',
  '{"schemaVersion":1,"records":[],"extra":1}',
];

function stores(snapshot) {
  const [entry] = snapshot.hooks;
  const base = {
    workspaceIdentity: "  id-1 ",
    hookDeclarationDigest: entry.hookDeclarationDigest,
    digestAlgorithm: "sha256",
    decision: "trusted",
    grantedAt: "2026-02-28T23:59:59.5Z",
    eventAtGrant: "PreToolUse",
    displayCommandAtGrant: " echo hi ",
    sourcePathAtGrant: ".zcode/config.json",
    matcherAtGrant: null,
  };
  const variants = [
    base,
    {
      ...base,
      lastUsedAt: "2024-02-29T00:00:00Z",
      matcherIndexAtGrant: 0,
      appVersionAtGrant: "1.0",
    },
    { ...base, grantedAt: "2026-02-29T00:00:00Z" },
    { ...base, grantedAt: "2026-01-01T00:00:00+08:00" },
    { ...base, hookDeclarationDigest: "A".repeat(64) },
    { ...base, unknown: true },
    { ...base, displayCommandAtGrant: "   " },
    { ...base, matcherIndexAtGrant: 1.5 },
    { ...base, matcherIndexAtGrant: 2.0, sourceDiscoveryOrderAtGrant: -1 },
    { ...base, eventAtGrant: "Nope" },
  ];
  const contents = [
    ...STORES,
    ...variants.map((record) => JSON.stringify({ schemaVersion: 1, records: [record] })),
    JSON.stringify({ schemaVersion: 1, records: [base, { ...base, workspaceIdentity: "id-1" }] }),
  ];
  return contents.map((content) => {
    const parsed = parseWorkspaceHookTrustStoreContent(content);
    return { content, records: parsed.status === "ok" ? parsed.file.records : null };
  });
}

export function workspaceHookFixtures() {
  const built = CASES.map((testCase) => ({ testCase, ...build(testCase) }));
  const [research] = built;
  const snapshot = research.snapshot;
  const records = createWorkspaceHookTrustRecords({
    snapshot,
    reviewItemIds: [
      snapshot.hooks[1].reviewItemId,
      snapshot.hooks[0].reviewItemId,
      snapshot.hooks[1].reviewItemId,
    ],
    grantedAt: "2026-01-01T00:00:00.000Z",
    appVersion: "9.9.9",
  });
  const evaluation = {
    items: evaluations(snapshot)[0].items.map(([reviewItemId, trustState]) => ({
      reviewItemId,
      trustState,
    })),
  };
  const request = buildWorkspaceHookReviewRequest({
    snapshot: built[1].snapshot,
    evaluation: {
      items: built[1].snapshot.hooks.map((e, i) => ({
        reviewItemId: e.reviewItemId,
        trustState: i === 0 ? "trusted_persistent" : "pending_trust",
      })),
    },
    reviewFlowId: "workspace-hook-review:flow",
    generation: 2,
    sessionId: "s-1",
    host: {
      taskId: "s-1",
      runId: "workspace-hook-run:s-1:r",
      workspaceLabel: "w",
      remoteSessionId: "remote-1",
    },
    now: () => 1000,
    createId: () => "i",
  });
  return {
    numbers: [
      0,
      1,
      -1,
      1.5,
      60000,
      1e21,
      1e-7,
      0.000001,
      123.456,
      2 ** 53,
      1 / 3,
      1e300,
      5e-324,
      100,
      1e20,
    ].map((n) => [n, String(n)]),
    snapshots: built.map(({ testCase, runtimeRoot, snapshot: s }) => ({
      ...testCase,
      runtimeRoot,
      snapshot: s,
    })),
    evaluations: evaluations(snapshot),
    records,
    reviewRequest: request,
    reviewEvaluation: evaluation,
    stores: stores(snapshot),
    storeContent: `${JSON.stringify({ schemaVersion: 1, records }, null, 2)}\n`,
  };
}
