import assert from "node:assert/strict";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { z } from "zod";
import { zcodeWorkspaceUpdateInteractionPreferencesResultSchema } from "@zcode/shared";
import { askUserQuestionToolEntry } from "../../../apps/zcode-cli/packages/core/src/tool/handlers/ask-user-question.js";
import { AskUserQuestionInputSchema } from "../../../apps/zcode-cli/packages/contracts/src/tools/ask-user-question.js";
import { fixture, event, end, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
const questions = [
  {
    question: "Which layout?",
    header: "Layout",
    options: [
      { label: "List", description: "Compact", preview: "<div>List</div>" },
      { label: "Grid", description: "Visual" },
    ],
    multiSelect: false,
  },
  {
    question: "Which features?",
    header: "Features",
    options: [
      { label: "Search", description: "Find" },
      { label: "Export", description: "Save" },
    ],
    multiSelect: true,
  },
];
async function pending(h: Harness, id: string, after = 0) {
  const m = await h.wait(
    (m) =>
      m.params?.topic === `conversation/${id}` &&
      m.params.frame?.payload?.deltas?.some((d: Message) =>
        d.patch?.pendingInteractions?.some((p: Message) => p.kind === "userInput"),
      ),
    after,
  );
  return m.params.frame.payload.deltas
    .flatMap((d: Message) => d.patch?.pendingInteractions ?? [])
    .find((p: Message) => p.kind === "userInput");
}
function questionFixture(input: Message = { questions }, count = 1, env?: Record<string, string>) {
  return fixture({
    env,
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      if (req.messages.at(-1).role === "user" && req.messages.at(-1).content === "ask") {
        event(res, {
          tool_calls: Array.from({ length: count }, (_, index) => ({
            index,
            id: `ask${index}`,
            type: "function",
            function: { name: "AskUserQuestion", arguments: JSON.stringify(input) },
          })),
        });
        end(res, "tool_calls");
      } else {
        event(res, { content: "continued" });
        end(res, "stop");
      }
    },
  });
}
test("Rust AskUserQuestion waits for App answers, normalizes multiple selections and preserves preview notes", async () => {
  const f = await questionFixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "ask" }));
    const p = await pending(h, id);
    assert.equal(f.requests.length, 1);
    assert.equal(p.payload.questions[0].options[0].preview, "<div>List</div>");
    const answer = h.envelope("resolveInteraction", id, {
      interactionId: p.interactionId,
      answer: {
        action: "accept",
        content: {
          answers: { "Which layout?": " List ", "Which features?": [" Search ", "Export"] },
          annotations: {
            "Which layout?": { preview: "<div>List</div>", notes: "Keep it compact", unknown: 1 },
          },
        },
      },
    });
    assert.equal((await h.command(answer)).status, "accepted");
    assert.equal((await h.command(answer)).status, "duplicate");
    await h.completed(id);
    assert.equal(f.requests.length, 2);
    const output = f.requests[1]!.messages.at(-1).content;
    assert.match(output, /"Which layout\?"="List"/);
    assert.match(output, /"Which features\?"="Search, Export"/);
    assert.match(output, /selected preview:\n<div>List<\/div>/);
    assert.match(output, /user notes: Keep it compact/);
    const row = (await h.rows(id)).rows.find(
      (r) => r.kind === "toolCall" && r.toolName === "AskUserQuestion",
    );
    assert(row?.kind === "toolCall");
    assert.equal(row.status, "success");
    assert.deepEqual(JSON.parse(row.inputText as string).answers, {
      "Which layout?": "List",
      "Which features?": "Search, Export",
    });
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

async function snapshot(h: Harness, id: string, connection = "inspect") {
  const after = h.messages.length;
  const sub = await h.subscribe(`conversation/${id}`, connection, "web-remote-replayable");
  const m = await h.wait(
    (m) =>
      m.params?.subscriptionId === sub.ack.subscriptionId &&
      m.params?.frame?.payload?.kind === "snapshot",
    after,
  );
  return m.params.frame.payload.snapshot;
}
async function preference(h: Harness, enabled: boolean) {
  return h.client.request(
    "workspace/updateInteractionPreferences",
    {
      workspace: { workspacePath: h.workspace, workspaceKey: h.workspace },
      preferences: { askUserQuestionAutoResolutionEnabled: enabled },
    },
    zcodeWorkspaceUpdateInteractionPreferencesResultSchema,
  );
}
const clockEnv = { ZCODE_ENV: "test", ZCODE_E2E_ASK_USER_QUESTION_CLOCK_SCALE: "100" };

test("Rust AskUserQuestion matches TS normalization/formatting for partial, skipped, legacy and declined answers", async () => {
  const cases = [
    {
      answer: { action: "accept", content: { answers: { "Which layout?": "Grid" } } },
      expected: { "Which layout?": "Grid" },
    },
    { answer: { action: "accept", content: { answers: {} } }, expected: {} },
    {
      answer: { action: "accept", content: { answer_0: " Grid ", answer_1: ["Search", "Export"] } },
      expected: { "Which layout?": "Grid", "Which features?": "Search, Export" },
    },
    {
      answer: { freeText: " My custom layout " },
      single: true,
      expected: { "Which layout?": "My custom layout" },
    },
    { answer: { action: "decline" }, error: "declined" },
    { answer: { action: "cancel" }, error: "cancelled" },
  ];
  for (const c of cases) {
    const input = { questions: c.single ? [questions[0]] : questions };
    const f = await questionFixture(input);
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "ask" }));
      const p = await pending(h, id);
      await h.command(
        h.envelope("resolveInteraction", id, { interactionId: p.interactionId, answer: c.answer }),
      );
      await h.completed(id);
      const output = f.requests[1]!.messages.at(-1).content;
      if (c.error) assert.match(output, new RegExp(c.error));
      else
        assert.equal(
          output,
          askUserQuestionToolEntry.formatModelContent!({
            questions: input.questions,
            answers: c.expected,
          }),
        );
      const current = await snapshot(h, id);
      assert.deepEqual(current.pendingInteractions, []);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  }
});

test("Rust question validation agrees with TS and never announces invalid questions", async () => {
  const invalid = [
    { questions, answers: null },
    { questions, metadata: { source: null } },
    { questions, annotations: { "Which layout?": { notes: null } } },
    { questions: [] },
    { questions: Array(5).fill(questions[0]) },
    { questions: [{ ...questions[0], extra: true }] },
    { questions: [{ ...questions[0], options: [questions[0]!.options[0]] }] },
    {
      questions: [
        { ...questions[0], options: [questions[0]!.options[0], questions[0]!.options[0]] },
      ],
    },
    {
      questions: [
        {
          ...questions[0],
          options: [{ label: " Other ", description: "bad" }, questions[0]!.options[0]],
        },
      ],
    },
    ...["<html><div>x</div></html>", "<script>alert(1)</script>", "<!-- comment -->"].map(
      (preview) => ({
        questions: [
          {
            ...questions[0],
            options: [{ ...questions[0]!.options[0], preview }, questions[0]!.options[1]],
          },
        ],
      }),
    ),
  ];
  for (const input of invalid) {
    assert.equal(AskUserQuestionInputSchema.safeParse(input).success, false);
    const f = await questionFixture(input);
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "ask" }));
      await h.completed(id);
      assert.match(f.requests[1]!.messages.at(-1).content, /Tool failed/);
      assert(
        !h.messages.some((m) =>
          m.params?.frame?.payload?.deltas?.some(
            (d: Message) => d.patch?.pendingInteractions?.length,
          ),
        ),
      );
    } finally {
      await f.close();
    }
  }
});

test("Rust defaults multiSelect, rejects fabricated answers and retains questions after a foreign-session reply", async () => {
  const { multiSelect: _, ...single } = questions[0]!;
  const f = await questionFixture({
    questions: [single],
    answers: { "Which layout?": "MODEL FABRICATED" },
  });
  try {
    const h = f.start();
    const id = await h.create();
    const other = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "ask" }));
    const p = await pending(h, id);
    assert.equal(p.payload.questions[0].multiSelect, false);
    assert.equal(p.payload.input.answers, undefined);
    const reply = {
      interactionId: p.interactionId,
      answer: { action: "accept", content: { answers: { "Which layout?": "HUMAN" } } },
    };
    assert.equal((await h.command(h.envelope("resolveInteraction", other, reply))).status, "noop");
    assert.equal(f.requests.length, 1);
    await h.command(h.envelope("resolveInteraction", id, reply));
    await h.completed(id);
    assert.match(f.requests[1]!.messages.at(-1).content, /HUMAN/);
    assert(!f.requests[1]!.messages.at(-1).content.includes("MODEL FABRICATED"));
  } finally {
    await f.close();
  }
});

test("Rust question timers start on promotion and production ignores the test clock scale", async () => {
  for (const production of [false, true]) {
    const f = await questionFixture(
      { questions },
      2,
      production ? { ...clockEnv, ZCODE_ENV: "production" } : clockEnv,
    );
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "ask" }));
      await h.wait((m) =>
        m.params?.frame?.payload?.deltas?.some(
          (d: Message) => d.patch?.pendingInteractions?.length === 2,
        ),
      );
      const [first, second] = (await snapshot(h, id)).pendingInteractions;
      assert.equal(
        first.autoResolution.deadlineAt - first.autoResolution.startedAt,
        production ? 300000 : 3000,
      );
      assert.equal(second.autoResolution, undefined);
      const began = Date.now();
      await h.command(
        h.envelope("resolveInteraction", id, {
          interactionId: first.interactionId,
          answer: { action: "accept", content: { answers: {} } },
        }),
      );
      const next = (await snapshot(h, id)).pendingInteractions[0];
      assert.equal(next.interactionId, second.interactionId);
      assert(next.autoResolution.startedAt >= began);
      await h.command(h.envelope("stop", id));
    } finally {
      await f.close();
    }
  }
});

test("Rust concurrent questions keep call-order history and only the head timer while two App connections race", async () => {
  const f = await questionFixture({ questions }, 2);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.subscribe(`sessions-index/${h.workspace}`);
    await h.command(h.envelope("sendText", id, { text: "ask" }));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: Message) => d.patch?.pendingInteractions?.length === 2,
      ),
    );
    const s = await snapshot(h, id);
    const [first, second] = s.pendingInteractions;
    await h.client.request(
      "v4/connection/flow",
      { connectionId: "inspect", state: "closed" },
      z.object({}),
    );
    assert.equal(first.autoResolution.state, "hiddenGrace");
    assert.equal(second.autoResolution, undefined);
    const summaries = h.messages
      .flatMap((m) => m.params?.frame?.payload?.deltas ?? [])
      .map((d) => d.session)
      .filter(Boolean);
    assert(
      summaries.some(
        (s) =>
          s.pendingInteractionSummary.userInputCount === 2 &&
          s.pendingInteraction.toolName === "AskUserQuestion",
      ),
    );
    const cmd = h.envelope("resolveInteraction", id, {
      interactionId: second.interactionId,
      answer: { action: "accept", content: { answers: { "Which layout?": "Second" } } },
    });
    assert.equal((await h.command(cmd)).status, "accepted");
    const later = { ...cmd, commandId: "late-second", clientId: "phone" };
    assert.equal((await h.command(later)).status, "noop");
    assert.equal(f.requests.length, 1);
    await h.command(
      h.envelope("resolveInteraction", id, {
        interactionId: first.interactionId,
        answer: { action: "accept", content: { answers: { "Which layout?": "First" } } },
      }),
    );
    await h.completed(id);
    const results = f.requests[1]!.messages.filter((m: Message) => m.role === "tool");
    assert.deepEqual(
      results.map((m: Message) => m.tool_call_id),
      ["ask0", "ask1"],
    );
    assert.match(results[0].content, /First/);
    assert.match(results[1].content, /Second/);
    await h.close();
    const cold = f.start();
    const saved = await snapshot(cold, id);
    assert.equal(saved.pendingInteractions.length, 0);
    assert.equal(
      saved.rows.window.filter((r: Message) => r.kind === "toolCall" && r.status === "success")
        .length,
      2,
    );
    assert.equal((await cold.command(later)).status, "noop");
    assert.deepEqual(h.schemaErrors, []);
    assert.deepEqual(cold.schemaErrors, []);
  } finally {
    await f.close();
  }
});

for (const action of ["stop", "startNow", "EOF", "deleteSession"] as const)
  test(`Rust ${action} cancels waiting questions without auto-answer or replay`, async () => {
    const f = await questionFixture();
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "ask" }));
      const p = await pending(h, id);
      const after = h.messages.length;
      if (action === "EOF") await h.close();
      else {
        await h.command(
          h.envelope(
            action === "startNow" ? "sendText" : action,
            id,
            action === "startNow" ? { text: "continue", requestedDelivery: "startNow" } : {},
          ),
        );
        if (action === "startNow") await h.completed(id, after);
        if (action === "stop")
          await h.wait(
            (m) =>
              m.params?.frame?.payload?.deltas?.some(
                (d: Message) => d.patch?.control?.phase === "completedInterrupted",
              ),
            after,
          );
      }
      const active = action === "EOF" ? f.start() : h;
      const late = await active.command(
        active.envelope("resolveInteraction", id, {
          interactionId: p.interactionId,
          answer: { action: "accept", content: { answers: { "Which layout?": "LATE" } } },
        }),
      );
      assert.equal(late.status, "noop");
      const restored = await snapshot(active, id);
      assert.deepEqual(restored.pendingInteractions, []);
      assert.equal(f.requests.length, action === "startNow" ? 2 : 1);
      assert(!JSON.stringify(f.requests).includes("LATE"));
      assert.deepEqual(active.schemaErrors, []);
    } finally {
      await f.close();
    }
  });

test("Rust question countdown becomes visible then explicitly skips with no invented preference", async () => {
  const f = await questionFixture({ questions }, 1, clockEnv);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "ask" }));
    const p = await pending(h, id);
    assert.equal(p.autoResolution.deadlineAt - p.autoResolution.startedAt, 3000);
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: Message) =>
          d.patch?.pendingInteractions?.[0]?.autoResolution?.state === "visibleCountdown",
      ),
    );
    await h.completed(id);
    assert.equal(
      f.requests[1]!.messages.at(-1).content,
      askUserQuestionToolEntry.formatModelContent!({ questions, answers: {} }),
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust snooze and preference disable are permanent for registered questions, reenable affects new questions only", async () => {
  const f = await questionFixture({ questions }, 2, clockEnv);
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "ask" }));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: Message) => d.patch?.pendingInteractions?.length === 2,
      ),
    );
    const s = await snapshot(h, id);
    const [first, second] = s.pendingInteractions;
    const snooze = h.envelope("snoozeInteractionAutoResolution", id, {
      interactionId: first.interactionId,
    });
    assert.equal((await h.command(snooze)).status, "accepted");
    assert.equal((await h.command({ ...snooze, commandId: "again" })).status, "noop");
    await preference(h, false);
    await preference(h, true);
    await delay(3200);
    assert.equal(f.requests.length, 1);
    assert.equal((await snapshot(h, id)).pendingInteractions[0].autoResolution.state, "snoozed");
    await h.command(
      h.envelope("resolveInteraction", id, {
        interactionId: first.interactionId,
        answer: { action: "accept", content: { answers: {} } },
      }),
    );
    assert.equal((await snapshot(h, id)).pendingInteractions[0].autoResolution, undefined);
    await h.command(
      h.envelope("resolveInteraction", id, {
        interactionId: second.interactionId,
        answer: { action: "accept", content: { answers: {} } },
      }),
    );
    await h.completed(id);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "ask" }));
    const newer = await pending(h, id, after);
    assert.equal(newer.autoResolution.state, "hiddenGrace");
    const disabled = await preference(h, false);
    assert.equal(disabled.snoozedInteractionCount, 1);
    assert.equal((await snapshot(h, id)).pendingInteractions[0].autoResolution.state, "snoozed");
    await h.command(h.envelope("stop", id));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
