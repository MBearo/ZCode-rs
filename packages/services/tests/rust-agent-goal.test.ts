import assert from "node:assert/strict";
import test from "node:test";
import { event, end, fixture, type Harness } from "./rust-agent-fixture.js";
import { setTimeout as delay } from "node:timers/promises";

function goal(h: Harness, sid: string, status: string, after = 0) {
  return h.wait(
    (m) =>
      m.params?.topic === `conversation/${sid}` &&
      m.params.frame?.payload?.deltas?.some((d: any) => d.patch?.goal?.status === status),
    after,
  );
}

test("Goal verifies hidden history without tools, continues the gap and persists a verified result", async () => {
  let verification = 0;
  const f = await fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const verify = request.messages
        .at(-1)
        .content.includes("Verify whether the active session goal");
      if (verify) {
        assert.equal(request.tools?.length ?? 0, 0);
        verification++;
        event(response, {
          content: JSON.stringify({
            passed: verification === 2,
            reason: verification === 2 ? "done" : "test missing",
            nextAction: verification === 2 ? "" : "Run the check",
          }),
        });
      } else event(response, { content: verification ? "checked" : "implemented" });
      end(response, "stop");
    },
  });
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    const ack = await h.command(h.envelope("sendGoalCommand", sid, { text: "实现并检查" }));
    assert.equal(ack.status, "accepted");
    await goal(h, sid, "verified");
    await h.completed(sid);
    assert.equal(f.requests.length, 4);
    assert.match(JSON.stringify(f.requests[2]!.messages), /Run the check/);
    const rows = (await h.rows(sid)).rows;
    assert.equal(rows.filter((r: any) => r.kind === "turnHeader").length, 2);
    assert.ok(!rows.some((r: any) => r.kind === "assistantText" && r.text.includes('"passed"')));
    assert.equal(rows.filter((r: any) => r.marker?.type === "goalVerify").length, 2);
    await h.close();
    const cold = f.start();
    await cold.subscribe(`conversation/${sid}`);
    const restored = (await cold.wait((m) => m.params?.frame?.payload?.kind === "snapshot")).params
      .frame.payload;
    assert.equal(restored.snapshot.goal?.status, "verified");
    assert.equal(restored.snapshot.goal?.verifications.length, 2);
    assert.deepEqual(cold.schemaErrors, []);
    await cold.close();
  } finally {
    await f.close();
  }
});

test("Goal queued in guide mode preserves its intent and replaces the target only at admission", async () => {
  let release: (() => void) | undefined;
  const f = await fixture({
    respond(request, response, attempt) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      if (attempt === 1) {
        release = () => {
          event(response, { content: "first done" });
          end(response, "stop");
        };
        return;
      }
      const verify = request.messages.at(-1).content.includes("Verify whether");
      event(response, {
        content: verify
          ? '{"passed":true,"reason":"new target done","nextAction":""}'
          : "goal done",
      });
      end(response, "stop");
    },
  });
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    await h.command(h.envelope("setFollowupMode", sid, { mode: "guide" }));
    await h.command(h.envelope("sendText", sid, { text: "first" }));
    const command = h.envelope("sendGoalCommand", sid, { text: "new target" });
    const ack = await h.command(command);
    assert.equal(ack.status, "accepted");
    assert.equal((ack.result as any).delivery, "queue");
    assert.equal((await h.command(command)).status, "duplicate");
    const queued = await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some((d: any) => d.patch?.queue?.items?.length),
    );
    const patch = queued.params.frame.payload.deltas.find(
      (d: any) => d.patch?.queue?.items?.length,
    ).patch;
    assert.equal(patch.queue.items[0].kind, "sendGoalCommand");
    assert.equal(patch.goal, null);
    while (!release) await delay(5);
    release();
    await goal(h, sid, "verified");
    assert.equal(f.requests.length, 3);
    assert.match(f.requests[1]!.messages.at(-1).content, /new target/);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    release?.();
    await f.close();
  }
});

test(
  "Goal waits for background completion, receives terminal facts and then verifies",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture({
      respond(request, response, attempt) {
        response.writeHead(200, { "Content-Type": "text/event-stream" });
        if (attempt === 1) {
          event(response, {
            tool_calls: [
              {
                index: 0,
                id: "background-goal",
                type: "function",
                function: {
                  name: "Bash",
                  arguments: JSON.stringify({
                    command: "sleep 0.35; echo checked",
                    run_in_background: true,
                  }),
                },
              },
            ],
          });
          end(response, "tool_calls");
          return;
        }
        const verify = request.messages.at(-1).content.includes("Verify whether");
        event(response, {
          content: verify
            ? '{"passed":true,"reason":"background checked","nextAction":""}'
            : "work done",
        });
        end(response, "stop");
      },
    });
    try {
      const h = f.start();
      const sid = await h.create();
      await h.subscribe(`conversation/${sid}`);
      assert.equal(
        (await h.command(h.envelope("sendGoalCommand", sid, { text: "check in background" })))
          .status,
        "accepted",
      );
      await h.completed(sid);
      assert.equal(f.requests.length, 2);
      await goal(h, sid, "verified");
      assert.equal(f.requests.length, 4);
      assert.match(f.requests[2]!.messages.at(-1).content, /task-notification/);
      assert.match(f.requests[2]!.messages.at(-1).content, /completed/);
      assert.deepEqual(h.schemaErrors, []);
      await h.close();
    } finally {
      await f.close();
    }
  },
);

test("Goal invalid verifier output retains a resumable goal and does not claim success", async () => {
  let malformed = true;
  const f = await fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const verify = request.messages
        .at(-1)
        .content.includes("Verify whether the active session goal");
      event(response, {
        content: verify
          ? malformed
            ? "not json"
            : '{"passed":true,"reason":"verified","nextAction":""}'
          : "work",
      });
      end(response, "stop");
    },
  });
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    assert.equal(
      (await h.command(h.envelope("sendGoalCommand", sid, { text: "do work" }))).status,
      "accepted",
    );
    await goal(h, sid, "failed");
    await h.completed(sid);
    assert.equal(f.requests.length, 2);
    malformed = false;
    assert.equal((await h.command(h.envelope("resumeGoal", sid))).status, "accepted");
    await goal(h, sid, "verified");
    assert.equal(f.requests.length, 4);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});

test("Goal pause during verification isolates late output, restart stays paused, resume explicitly runs", async () => {
  let held: (() => void) | undefined;
  let hold = true;
  const f = await fixture({
    respond(request, response) {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      if (request.messages.at(-1).content.includes("Verify whether the active session goal")) {
        const finish = () => {
          event(response, { content: '{"passed":true,"reason":"done","nextAction":""}' });
          end(response, "stop");
        };
        if (hold) {
          held = finish;
          return;
        }
        finish();
      } else {
        event(response, { content: "work" });
        end(response, "stop");
      }
    },
  });
  try {
    const h = f.start();
    const sid = await h.create();
    await h.subscribe(`conversation/${sid}`);
    assert.equal(
      (await h.command(h.envelope("sendGoalCommand", sid, { text: "goal" }))).status,
      "accepted",
    );
    await goal(h, sid, "verifying");
    assert.equal((await h.command(h.envelope("pauseGoal", sid))).status, "accepted");
    await goal(h, sid, "paused");
    held?.();
    await h.close();
    const cold = f.start();
    await cold.subscribe(`conversation/${sid}`);
    const restored = (await cold.wait((m) => m.params?.frame?.payload?.kind === "snapshot")).params
      .frame.payload;
    assert.equal(restored.snapshot.goal?.status, "paused");
    assert.ok(f.requests.length <= 2);
    hold = false;
    const after = cold.messages.length;
    assert.equal((await cold.command(cold.envelope("resumeGoal", sid))).status, "accepted");
    await goal(cold, sid, "verified", after);
    assert.deepEqual(cold.schemaErrors, []);
    await cold.close();
  } finally {
    held?.();
    await f.close();
  }
});
