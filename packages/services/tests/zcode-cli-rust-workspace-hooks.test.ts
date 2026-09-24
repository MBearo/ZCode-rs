import assert from "node:assert/strict";
import test from "node:test";
import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import {
  buildWorkspaceHookBundleSnapshot,
  createWorkspaceHookSourceInput,
  resolveWorkspaceHookRuntimeRoot,
} from "@zcode/shared/workspace-hook-discovery";
import { end, event, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

const MARKER = 'touch "$ZCODE_PROJECT_DIR/ran-$ZCODE_SESSION_ID"';
const PROJECT = {
  hooks: {
    enabled: true,
    events: { PreToolUse: [{ matcher: "Read", hooks: [{ type: "command", command: MARKER }] }] },
  },
  mcp: { servers: {} },
};

/** `read` calls Read once; anything else answers with text. */
async function projectFixture(project: Message | null = PROJECT) {
  const f = await fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const messages = request.messages as Message[];
      const last = messages.findLast(
        (m) => m.role === "user" && !String(m.content).startsWith("<"),
      );
      const answered = messages.at(-1)?.role === "tool";
      if (!answered && last?.content === "read") {
        const call = { name: "Read", arguments: JSON.stringify({ file_path: "notes.txt" }) };
        event(response, {
          tool_calls: [{ index: 0, id: "call-1", type: "function", function: call }],
        });
        end(response, "tool_calls");
      } else {
        event(response, { content: "done" });
        end(response, "stop");
      }
    },
  });
  await writeFile(join(f.cwd, "notes.txt"), "hello\n");
  if (project) {
    await mkdir(join(f.cwd, ".zcode"), { recursive: true });
    await writeFile(join(f.cwd, ".zcode", "config.json"), JSON.stringify(project));
  }
  return f;
}

async function send(h: Harness, id: string, text: string) {
  const before = h.messages.length;
  await h.command(h.envelope("sendText", id, { text }));
  await h.completed(id, before);
}
async function snapshot(h: Harness, id: string, connection: string) {
  const after = h.messages.length;
  const sub = await h.subscribe(`conversation/${id}`, connection);
  const m = await h.wait(
    (m) =>
      m.params?.subscriptionId === sub.ack.subscriptionId &&
      m.params.frame?.payload?.kind === "snapshot",
    after,
  );
  return m.params.frame.payload.snapshot;
}
const ran = (cwd: string, id: string) =>
  access(join(cwd, `ran-${id}`)).then(
    () => true,
    () => false,
  );
function target(request: Message) {
  const keys = [
    "sessionId",
    "taskId",
    "runId",
    "workspaceIdentity",
    "bundleDigest",
    "reviewFlowId",
    "generation",
    "interactionId",
  ];
  return Object.fromEntries(keys.map((k) => [k, request[k]]));
}

test("Rust project hooks stay blocked until the review trusts them", async () => {
  const f = await projectFixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const phone = await h.subscribe(`conversation/${id}`, "phone", "web-remote-replayable");
    await send(h, id, "read");
    assert.equal(await ran(f.cwd, id), false);
    const blocked = ((await h.rows(id)).rows as Message[]).find((r) => r.kind === "hookInvocation");
    assert.deepEqual(
      blocked?.executions.map((e: Message) => [
        e.outcome,
        e.blockReason,
        e.didExecute,
        e.sourceKind,
      ]),
      [["blocked", "workspace_hooks_pending_trust", false, "project"]],
    );
    const first = await snapshot(h, id, "banner");
    const banner = first.workspaceHookAdmission;
    assert.equal(banner.pendingCount, 1);
    assert.equal(banner.workspaceIdentity, f.cwd);
    // 目标与快照不符时拒绝。
    const wrong = await h.command(
      h.envelope("requestWorkspaceHookReview", id, {
        sessionId: id,
        workspaceIdentity: f.cwd,
        bundleDigest: "0".repeat(64),
      }),
    );
    assert.equal(wrong.status, "failed");
    assert.equal((wrong as Message).reasonCode, "workspace_hooks_snapshot_mismatch");
    assert.equal(
      (wrong as Message).message,
      "Workspace Hook review command rejected: workspace_hooks_snapshot_mismatch",
    );
    const before = h.messages.length;
    const opened = await h.command(
      h.envelope("requestWorkspaceHookReview", id, {
        sessionId: id,
        workspaceIdentity: f.cwd,
        bundleDigest: banner.bundleDigest,
      }),
    );
    assert.equal(opened.status, "accepted");
    const review = (await snapshot(h, id, "review")).pendingInteractions.find(
      (p: Message) => p.kind === "workspaceHookReview",
    );
    const request = review.payload;
    assert.equal(request.generation, 1);
    assert.deepEqual(request.summary, { eventCount: 1, hookCount: 1, pendingCount: 1 });
    assert.equal(request.items[0].displayName, "PreToolUse · Read");
    assert.equal(request.items[0].trustState, "pending_trust");
    await h.wait(
      (m) =>
        m.params?.subscriptionId === phone.ack.subscriptionId &&
        m.params.frame?.payload?.deltas?.some((d: Message) =>
          d.patch?.pendingInteractions?.some(
            (p: Message) => p.interactionId === request.interactionId,
          ),
        ),
      before,
    );
    const trusted = await h.command(
      h.envelope("respondWorkspaceHookReview", id, {
        ...target(request),
        decision: { action: "trust_selected", reviewItemIds: [request.items[0].reviewItemId] },
      }),
    );
    assert.equal(trusted.status, "accepted");
    const after = await snapshot(h, id, "after-trust");
    assert.equal(after.workspaceHookAdmission, null);
    assert.equal(after.pendingInteractions.length, 0);
    const store = JSON.parse(
      await readFile(join(f.root, ".zcode", "security", "workspace-hook-trust-v1.json"), "utf8"),
    );
    assert.equal(store.records[0].workspaceIdentity, f.cwd);
    assert.equal(store.records[0].eventAtGrant, "PreToolUse");
    await send(h, id, "read");
    assert.equal(await ran(f.cwd, id), true);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

/** Node's own bundle for the fixture project (shared digest implementation). */
function nodeSnapshot(cwd: string) {
  const source = createWorkspaceHookSourceInput({
    path: join(cwd, ".zcode", "config.json"),
    workingDirectory: cwd,
    hooks: PROJECT.hooks as never,
    discoveryOrder: 0,
  });
  const runtimeRoot = resolveWorkspaceHookRuntimeRoot([
    { enabled: false, timeoutMs: 60000, maxOutputBytes: 32768 },
    PROJECT.hooks,
  ]);
  return buildWorkspaceHookBundleSnapshot({
    workspaceIdentity: cwd,
    workspacePath: cwd,
    sources: [source],
    runtimeRoot,
  })!;
}

test("Rust revoke reopens the review and toggles rewrite the project config", async () => {
  const f = await projectFixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "hi");
    const banner = (await snapshot(h, id, "banner")).workspaceHookAdmission;
    const expected = nodeSnapshot(f.cwd);
    // Rust 与 Node 对同一项目得出相同 bundle digest。
    assert.equal(banner.bundleDigest, expected.bundleDigest);
    const digest = expected.hooks[0]!.hookDeclarationDigest;
    // Settings 预先信任（无会话），本会话随即重载信任。
    const grant = (await h.client.request("workspace/hooks/trustGrant", {
      workspace: { workspacePath: f.cwd, workspaceKey: f.cwd },
      bundleDigest: banner.bundleDigest,
      hookDeclarationDigest: digest,
    })) as Message;
    assert.deepEqual(grant, { accepted: true });
    assert.equal((await snapshot(h, id, "granted")).workspaceHookAdmission, null);
    const changed = (await h.client.request("workspace/hooks/trustGrant", {
      workspace: { workspacePath: f.cwd, workspaceKey: f.cwd },
      bundleDigest: "0".repeat(64),
      hookDeclarationDigest: digest,
    })) as Message;
    assert.deepEqual(changed, { accepted: false, reasonCode: "workspace_hooks_bundle_changed" });
    const revoked = await h.command(
      h.envelope("revokeWorkspaceHookTrust", id, {
        sessionId: id,
        workspaceIdentity: f.cwd,
        bundleDigest: banner.bundleDigest,
        hookDeclarationDigests: [digest],
      }),
    );
    assert.equal(revoked.status, "accepted");
    const reopened = await snapshot(h, id, "revoked");
    assert.equal(reopened.workspaceHookAdmission.pendingCount, 1);
    const request = reopened.pendingInteractions[0].payload;
    assert.equal(request.items[0].trustState, "revoked");
    const item = request.items[0].reviewItemId;
    const toggled = await h.command(
      h.envelope("toggleWorkspaceHookReviewItem", id, {
        ...target(request),
        reviewItemId: item,
        enabled: false,
      }),
    );
    assert.equal(toggled.status, "accepted");
    const rewritten = await readFile(join(f.cwd, ".zcode", "config.json"), "utf8");
    // 保留原文件键序（hooks 在 mcp 之前），只改该声明的 enabled。
    assert.ok(rewritten.indexOf('"hooks"') < rewritten.indexOf('"mcp"'));
    assert.equal(JSON.parse(rewritten).hooks.events.PreToolUse[0].hooks[0].enabled, false);
    const after = await snapshot(h, id, "toggled");
    // 停用的声明不计入横幅；审查以新 generation 继续。
    assert.equal(after.workspaceHookAdmission, null);
    assert.ok(after.pendingInteractions[0].payload.generation > request.generation);
    const stale = await h.command(
      h.envelope("toggleWorkspaceHookReviewItem", id, {
        ...target(request),
        reviewItemId: item,
        enabled: true,
      }),
    );
    assert.equal((stale as Message).reasonCode, "workspace_hooks_review_superseded");
  } finally {
    await f.close();
  }
});

test("Rust review commands need project hooks", async () => {
  const f = await projectFixture(null);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "hi");
    const ack = await h.command(
      h.envelope("requestWorkspaceHookReview", id, {
        sessionId: id,
        workspaceIdentity: f.cwd,
        bundleDigest: "0".repeat(64),
      }),
    );
    assert.equal((ack as Message).reasonCode, "workspace_hooks_require_trust_capable_host");
    assert.equal((await snapshot(h, id, "none")).workspaceHookAdmission, null);
  } finally {
    await f.close();
  }
});
