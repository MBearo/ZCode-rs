import assert from "node:assert/strict";
import test from "node:test";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { end, event, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

const json = (value: unknown) => `printf '%s' '${JSON.stringify(value)}'`;
const hook = (command: string, matcher?: string) => [
  { ...(matcher ? { matcher } : {}), hooks: [{ type: "command", command }] },
];
const isReminder = (m: Message) => String(m.content).startsWith("<system-reminder>");

/** `write` / `read` call a tool once; anything else answers with text. */
function hooksFixture(events: Message, permissionMode = "yolo") {
  return fixture({
    permissionMode,
    userConfig: { hooks: { enabled: true, events } },
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const messages = request.messages as Message[];
      const index = messages.findLastIndex((m) => m.role === "user" && !isReminder(m));
      const answered = messages.slice(index).some((m) => m.role === "tool");
      const text = answered ? "" : messages[index]?.content;
      const call = (name: string, args: unknown) => {
        event(response, {
          tool_calls: [
            {
              index: 0,
              id: "call-1",
              type: "function",
              function: { name, arguments: JSON.stringify(args) },
            },
          ],
        });
        end(response, "tool_calls");
      };
      if (text === "write") call("Write", { file_path: "a.txt", content: "x" });
      else if (text === "read") call("Read", { file_path: "missing.txt" });
      else {
        event(response, { content: "done" });
        end(response, "stop");
      }
    },
  });
}

async function session(h: Harness) {
  const id = await h.create();
  await h.subscribe(`conversation/${id}`);
  return id;
}
async function send(h: Harness, id: string, text: string) {
  const before = h.messages.length;
  await h.command(h.envelope("sendText", id, { text }));
  await h.completed(id, before);
}
function toolResult(request: Message) {
  return request.messages.find((m: Message) => m.role === "tool")?.content as string;
}
async function hookRows(h: Harness, id: string) {
  return ((await h.rows(id)).rows as Message[]).filter((r) => r.kind === "hookInvocation");
}

test("Rust PreToolUse exit 2 blocks the tool and both subscriptions see the hook row", async () => {
  const f = await hooksFixture({ PreToolUse: hook("echo 'no writes' >&2; exit 2", "Write|Edit") });
  try {
    const h = f.start();
    const id = await session(h);
    const phone = await h.subscribe(`conversation/${id}`, "phone", "web-remote-replayable");
    const before = h.messages.length;
    await send(h, id, "write");
    assert.equal(toolResult(f.requests[1]!), "no writes");
    await assert.rejects(readFile(join(f.cwd, "a.txt")));
    const rows = (await h.rows(id)).rows as Message[];
    assert.equal(rows.find((r) => r.toolCallId === "call-1")?.status, "cancelled");
    const row = (await hookRows(h, id))[0]!;
    assert.equal(row.hookEventName, "PreToolUse");
    assert.equal(row.lane, "toolBefore");
    assert.equal(row.anchorToolCallId, "call-1");
    assert.equal(row.state, "completed");
    assert.deepEqual(
      row.executions.map((e: Message) => [e.outcome, e.blockReason, e.displayName, e.sourceKind]),
      [["blocked", "no writes", "echo", "user"]],
    );
    await h.wait(
      (m) =>
        m.params?.subscriptionId === phone.ack.subscriptionId &&
        m.params.frame?.payload?.deltas?.some((d: Message) => d.row?.kind === "hookInvocation"),
      before,
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust tool hooks rewrite the input and append their contexts", async () => {
  const pre = json({
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      additionalContext: "pre ctx",
      updatedInput: { file_path: "notes.txt" },
    },
  });
  const post = json({
    hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: "post ctx" },
  });
  const f = await hooksFixture({
    PreToolUse: hook(pre, "Read"),
    PostToolUse: [
      { matcher: "Read", hooks: [{ type: "command", command: post }] },
      { hooks: [{ type: "command", command: 'cat > "$ZCODE_PROJECT_DIR/post-stdin.json"' }] },
    ],
  });
  try {
    await writeFile(join(f.cwd, "notes.txt"), "hello notes\n");
    const h = f.start();
    const id = await session(h);
    await send(h, id, "read");
    const result = toolResult(f.requests[1]!);
    assert.match(result, /hello notes/);
    assert.ok(result.endsWith("\n\n[Hook additional context]\n#1\npre ctx\n#2\npost ctx"), result);
    const stdin = JSON.parse(await readFile(join(f.cwd, "post-stdin.json"), "utf8"));
    assert.equal(stdin.hook_event_name, "PostToolUse");
    assert.equal(stdin.tool_name, "Read");
    assert.equal(stdin.tool_use_id, "call-1");
    assert.equal(stdin.tool_input.file_path, "notes.txt");
    assert.equal(stdin.session_id, id);
    assert.equal(stdin.cwd, f.cwd);
    assert.equal(stdin.permission_mode, "yolo");
    assert.equal(
      (await hookRows(h, id)).map((r) => `${r.hookEventName}:${r.lane}:${r.hookCount}`).join(","),
      "PreToolUse:toolBefore:1,PostToolUse:toolAfter:2",
    );
  } finally {
    await f.close();
  }
});

test("Rust UserPromptSubmit blocks an input and lifecycle contexts reach the model", async () => {
  const block = json({ continue: false, stopReason: "blocked by policy" });
  const context = json({ additionalContext: "prompt ctx" });
  const f = await hooksFixture({
    SessionStart: hook(
      json({
        hookSpecificOutput: { hookEventName: "SessionStart", additionalContext: "session ctx" },
      }),
    ),
    UserPromptSubmit: hook(`if grep -q secret; then ${block}; else ${context}; fi`),
  });
  try {
    const h = f.start();
    const id = await session(h);
    await send(h, id, "secret plan");
    // 被阻止的输入不产生模型请求，错误横幅显示阻止原因。
    assert.equal(f.requests.length, 0);
    const after = h.messages.length;
    const sub = await h.subscribe(`conversation/${id}`, "after-block");
    const snapshot = await h.wait(
      (m) =>
        m.params?.subscriptionId === sub.ack.subscriptionId &&
        m.params.frame?.payload?.kind === "snapshot",
      after,
    );
    const lastError = snapshot.params.frame.payload.snapshot.control.lastError;
    assert.equal(lastError.code, "fault.runtime.hookBlocked");
    assert.equal(lastError.message, "hooks_prompt_block: blocked by policy");
    await send(h, id, "hello");
    const messages = f.requests[0]!.messages as Message[];
    assert.ok(!messages.some((m) => String(m.content).includes("secret plan")));
    const texts = messages.filter(isReminder).map((m) => m.content as string);
    assert.ok(
      texts.some((t) => t.includes("SessionStart hook additional context: \n#1\nsession ctx")),
    );
    assert.ok(
      texts.some((t) => t.includes("UserPromptSubmit hook additional context: \n#1\nprompt ctx")),
    );
    // 上下文插在本轮输入之前。
    const user = messages.findIndex((m) => m.content === "hello");
    const prompt = messages.findIndex((m) => String(m.content).includes("prompt ctx"));
    assert.ok(prompt >= 0 && prompt < user);
  } finally {
    await f.close();
  }
});

test("Rust Stop hooks continue a turn at most three times", async () => {
  const f = await hooksFixture({ Stop: hook(json({ decision: "block", reason: "keep going" })) });
  try {
    const h = f.start();
    const id = await session(h);
    await send(h, id, "plain");
    assert.equal(f.requests.length, 4);
    const reminders = (f.requests[3]!.messages as Message[]).filter((m) =>
      String(m.content).includes("Stop hook additional context: \n#1\nkeep going"),
    );
    assert.equal(reminders.length, 3);
    const rows = await hookRows(h, id);
    assert.equal(rows.length, 4);
    assert.ok(
      rows.every((r) => r.lane === "assistantWork" && r.executions[0].outcome === "blocked"),
    );
  } finally {
    await f.close();
  }
});

test("Rust PermissionRequest hooks answer a prompt before the user", async () => {
  const allow = json({
    hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "allow" } },
  });
  const f = await hooksFixture({ PermissionRequest: hook(allow, "Write") }, "build");
  try {
    const h = f.start();
    const id = await session(h);
    const phone = await h.subscribe(`conversation/${id}`, "phone", "web-remote-replayable");
    const before = h.messages.length;
    await send(h, id, "write");
    assert.equal(await readFile(join(f.cwd, "a.txt"), "utf8"), "x");
    const rows = (await h.rows(id)).rows as Message[];
    assert.equal(rows.find((r) => r.toolCallId === "call-1")?.status, "success");
    // 待决交互先出现再被 hook 收口；手机可重放流同样看到先出现、后清空。
    // 刷新窗口可能把"出现"与"清空"合进同一帧，按增量顺序判定。
    const pending = () =>
      h.messages
        .slice(before)
        .filter((m) => m.params?.subscriptionId === phone.ack.subscriptionId)
        .flatMap((m) => m.params.frame?.payload?.deltas ?? [])
        .filter((d: Message) => Array.isArray(d.patch?.pendingInteractions))
        .map((d: Message) => d.patch.pendingInteractions.length > 0);
    await h.wait(() => {
      const states = pending();
      return states.includes(true) && states.slice(states.indexOf(true)).includes(false);
    }, before);
    const row = (await hookRows(h, id))[0]!;
    assert.equal(row.hookEventName, "PermissionRequest");
    assert.equal(row.executions[0].outcome, "success");
  } finally {
    await f.close();
  }
});

test("Rust user answers win over a slow PermissionRequest hook", async () => {
  const allow = json({
    hookSpecificOutput: { hookEventName: "PermissionRequest", decision: { behavior: "allow" } },
  });
  const f = await hooksFixture({ PermissionRequest: hook(`sleep 5; ${allow}`, "Write") }, "build");
  try {
    const h = f.start();
    const id = await session(h);
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "write" }));
    const prompt = await h.permission(id);
    const started = Date.now();
    await h.command(
      h.envelope("resolveInteraction", id, {
        interactionId: prompt.interactionId,
        answer: { optionId: "deny" },
      }),
    );
    await h.completed(id, before);
    assert.ok(Date.now() - started < 4000);
    await assert.rejects(readFile(join(f.cwd, "a.txt")));
    const rows = (await h.rows(id)).rows as Message[];
    assert.equal(rows.find((r) => r.toolCallId === "call-1")?.status, "cancelled");
    await h.wait(
      (m) =>
        m.params?.topic === `conversation/${id}` &&
        m.params.frame?.payload?.deltas?.some(
          (d: Message) =>
            d.row?.kind === "hookInvocation" && d.row.executions[0]?.outcome === "cancelled",
        ),
      before,
    );
  } finally {
    await f.close();
  }
});
