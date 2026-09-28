---
name: dynamic-workflows
description: "Use when writing, debugging, or resubmitting a dynamic-workflow script for the CreateWorkflow tool: choosing subagent topology, typing subagent results, fanning out over files or git, gating loops on world.run commands, testing pieces with EvalWorkflowSnippet, planner-reviewer loops, confirming findings before reporting them, report() salvage, the report shape a run returns, publishing artifacts the user opens, and handling a backgrounded run."
when_to_use: "Only for CreateWorkflow scripts. A single delegation or a few independent lookups belong to the Agent tool instead."
---

# Writing dynamic workflows

This skill is the whole authoring contract for `CreateWorkflow`. The tool descriptions are
short on purpose; the facade the compiler checks against, the rules a script must satisfy and
the fields each tool takes are in §16, and `CreateWorkflow`, `AmendWorkflow`, `SaveWorkflow`
and `EvalWorkflowSnippet` refuse to run until this skill has been loaded in the session. The
rest is the judgment layer: how many subagents a request deserves, which of them should share
a context, what each one should hand back, and how to keep the run's work from dying with the
run.

Two names collide in this codebase. `CreateWorkflow` is the dynamic-workflow tool and the
only one this skill is about. The legacy `Workflow` tool and `/expert` are a different,
older feature that happens to share the word.

## The bar: first-class, expert-level work

A workflow exists to produce first-class, expert-level work — the deliverable a senior
practitioner would hand over after doing the job properly, not a faster draft of what one
reply could have said. A user who asks for a workflow is paying for that depth, and every
choice in this skill serves it: enough subagents to actually cover the ground, fresh eyes
on every plan and every draft (§3), an independent confirmation of every finding,
deterministic checks wherever a command can decide, and a report that says what was
verified, what was not, and what it all means.
Depth is measured by what is at stake, not by how many subagents re-read the same work:
verification is spent where a wrong claim would cost the user something, and each piece of
work gets the one check that decides it, not every check this skill knows.
When a shortcut and the expert's way disagree, take the expert's way. Trim the work only when
the task is genuinely small, never because thoroughness is inconvenient; trim the checking
when a wrong claim costs little or a command has already decided it.

## 1. Is this the right tool, and how big should it be?

| The request | The tool |
| --- | --- |
| One thing delegated to one agent | `Agent` |
| A few independent lookups, nobody reading anybody's answer | `Agent`, in parallel |
| Anything else the user did not name a workflow for — however many steps or subagents it needs | `Agent`, or do it yourself |
| The user said "use a workflow" / "使用 workflow" / "用工作流" — any phrasing naming workflow as the means | `CreateWorkflow`, mandatory, even if `Agent` would have done or one reply could answer |

**Only an explicit request starts a workflow.** The request is the whole routing rule: a
workflow begins with `/workflow` or with the user naming workflow/工作流 as the means, never
with your own judgement that a task looks orchestration-shaped. Results feeding later steps,
a loop with a stopping condition, control flow branching on a typed result — none of these
are a reason to start one. Delegate with `Agent` or do the work yourself.

**An explicit request is binding.** When the user names workflow/工作流 as the way to do the
task, the routing question is closed: not `Agent`, not doing it inline, not "this is too small
for a workflow". The user picked the tool; what remains for you is only how big the script
should be, and the smallest task still gets a small workflow rather than something else.

Once a workflow has been requested, these are the shapes it is good at, and what the script
should reach for: results feeding later steps (pipelines, fan-out then fan-in), a loop with a
stopping condition, and control flow branching on a typed result. They describe how to build
the workflow the user asked for — they never decide that there should be one.

An explicit workflow request is a request for depth, and the bar above is the measure.
Default to the thorough shape — a fresh subagent per unit of work, a deterministic gate
wherever a command can decide, an independent confirmation of every finding the user will
act on as fact (§10) — and size the checking to the stakes: a bug the user will fix on your
word gets a confirmer; a poem, a brainstorm or a survey gets a reader at most. Cost still
counts: every task is a full model session with its own context, so put the sessions where
they raise confidence, not where they repeat each other. "Review the changed files" wants
one reviewer per changed file plus one confirmer per finding; a three-line question wants
neither; "write me twenty poems" wants twenty poets and no confirmers.

## 2. Subagent topology

This is the decision that separates a good script from a bad one, and there are four parts
to it.

**Fresh subagent per item, or one shared subagent?** Every `agent()` call creates a new context.
Sharing a context means sharing the *variable*:

```ts
// Independent: each judge sees only its own item. Runs concurrently.
items.map((i) => agent(`judge-${i.id}`).ask<Verdict>(`Judge ${i.id}`));

// Calibrated: one context sees every item in sequence, so its later verdicts are
// consistent with its earlier ones. Asks on one subagent queue FIFO — this serializes.
const judge = agent("judge", "You rank consistently across a whole batch.");
items.map((i) => judge.ask<Verdict>(`Judge ${i.id}`));
```

Both are correct for different jobs. Independent is right when the items are unrelated and
you want parallelism; shared is right when the answers must be consistent with each other,
and you are paying serialization for that consistency. Choosing shared by accident — by
hoisting the `agent()` call out of a loop for tidiness — turns a parallel fan-out into a
queue. Nothing rejects that script; the run simply takes as long as every item in a row.

**A name is an identity, and it has to be unique.** Naming is optional, but no two subagents
in one run may share a non-empty name — a duplicate fails the whole run. Note what that
means for the first snippet above: one fixed name inside a fan-out is a duplicate once per
item, so it is written `` agent(`judge-${i.id}`) ``, per item. The compiler catches the
literal form (`agent("judge")` inside a `.map`/`for...of`) before you submit; a name built
at run time it cannot see, so that one costs you the run. Anonymous is always safe: drop
the name when you have nothing per-item to build one from.

Names are worth giving anyway, because they are how a **revised** re-run finds its cache
(§13): resubmitting a fixed script with `AmendWorkflow` imports, per named subagent, every ask
whose instructions did not change — at zero tokens, up to the first ask that has to run
live. Stable, meaningful names are what makes that hit. Anonymous subagents are legal
and simply always start with an empty context.

**Persona, frozen at creation.** A persona stacks on top of the harness's own workflow
subagent contract — citing `path:line`, never reporting a check it did not run, saying
plainly what it could not do, escalating instead of faking — so write the role and the
standard of judgement, and leave those rules out; they are already there. Every subagent
has the same tools — reading, searching, editing, running commands — and runs on the
session's model; there is no per-subagent tool profile or model choice. What a subagent may
do with its tools is said in the ask: a reviewer that must not touch the code is told "do not
edit any file", a judge of a string is handed the string and told to judge it as given. A
subagent asked to run a check runs it; one told not to edit does not. None of this can
change after `agent()` — there is no per-task override.

**Loops.** Bound every loop with a round cap, and carry the feedback forward:

```ts
let feedback = "none";
for (let round = 0; round < 5; round++) {
  const plan = await planner.ask<Plan>(`Plan the fix. Previous critique: ${feedback}`);
  const review = await reviewer.ask<Review>(`Critique this plan: ${JSON.stringify(plan)}`);
  if (review.approved) return plan;
  feedback = review.feedback;
}
```

Reuse the same two subagents across rounds — that is the point. A subagent asked more than once
keeps its accumulated context and gets long-lived caching, so round five is cheap. Creating
a fresh planner each round throws away everything it learned in round four. The one thing
reuse costs you is freshness: by round three the reviewer is anchored on its own earlier
critique. Keep the persistent reviewer for continuity and add an independent one at the end (§3).

**No nesting.** Subagents cannot call `CreateWorkflow`. If a task is big enough to want its own
workflow, model it as more subagents here.

## 3. Fresh eyes

The author of a plan or a draft cannot see its gaps, because they filled them while
writing. A reader with a separate context has no such fill, so the gaps show. That is the
whole method, and it applies at every stage *inside* the workflow where something is
drafted — a plan, a fix, a finding, the final report. It pays most on plans, which are
cheap to critique and expensive to execute wrongly.

Four rules make the eyes actually fresh:

- **Separate context, not a second opinion from the same head.** An independent reader is a new
  `agent()` that has seen nothing but the draft. Every planner–reviewer loop keeps a
  persistent reviewer for continuity (§2), and then puts the final draft in front of a
  reviewer that has never seen an earlier round.
- **Ask for failures, not approval.** "Is this good?" gets a yes. "What would break this?
  What is missing? Restate the plan in your own words" gets findings. Write the ask so
  that approving takes evidence and objecting is the easy move.
- **Give the eyes the same evidence.** A reviewer told to judge a plan about code from the
  plan's text alone can only judge coherence. To judge correctness it must read the code:
  tell plan reviewers to read the files the plan touches, and not to edit them.
- **Give a prose deliverable an independent read.** When the deliverable is text nothing else
  checked — a guide, an analysis, a write-up — hand it, before the script returns, to a
  reader-proxy subagent asked what is unclear, what the text itself fails to support, and
  what the user will ask next — then fix those, not the prose. Say in the ask that it judges
  the report as a reader would, from the text alone, so it does not wander off to verify it
  against the repository. If you want claims checked, that is a confirmer that reads the
  code, not an independent read. A report whose findings were each confirmed (§10) has
  already had its eyes; do not add a reader on top of the confirmers.

Three cautions. Correlated reviewers add little — same model, same prompt, same blind
spots — so give each a distinct lens or a different task. Bound it: one deliverable gets one
mechanism — a plan gets a reviewer who reads the code, findings get a confirmer, prose gets a
reader. A draft that came out of a planner–reviewer loop and then passed a fresh final
reviewer has had its fresh eyes; a reader-proxy pass over the same text is a third session
re-reading work two have already read. Two mechanisms on one deliverable is the ceiling, and
it needs a reason you can say. And the rules above are about the subagents *inside* the
script, not a gate on submitting it: reading your own script once against the request is
normally enough, and the run confirmation already puts the plan in front of the user. An
independent read of the script by a fresh `Agent` is an option for a genuinely hard one — many
phases, subtle gates — not a routine step.

## 4. Typed results

Pass a type argument to `ask` whenever your control flow will branch on the answer; leave it
off when you just want prose back. The type must be an interface or type alias you write in
the script.

**JSDoc on a property becomes the field description the subagent actually reads.** This is the
cheapest quality lever in the whole surface and the easiest to skip:

```ts
interface Finding {
  /** Workspace-relative path the problem is in. */
  path: string;
  /** One sentence: what is wrong, not how to fix it. */
  problem: string;
  /** How much it matters. Reserve "high" for data loss or a crash. */
  severity: "low" | "medium" | "high";
}
```

Without the comments you get whatever the subagent guessed `severity` meant. With them you get
a calibrated field, for the price of three lines.

Keep results narrow. A result crosses a schema boundary and gets interpolated into the
next prompt, so a wide one costs tokens twice. **Pass paths, not file contents** — a subagent
has its own file tools and can read what it needs; handing it a stringified 4000-line file
spends tokens to tell it something it could have fetched itself.

## 5. Getting the world in

`files.glob` and `git.changedFiles` are how a script learns what to fan out over. Both are
executed by the harness, recorded, and replayed from the record on resume.

Caps **reject rather than truncate**: an over-wide `files.grep` fails that call instead of
handing you a silently partial view of the workspace to fan out over. Narrow the pattern or
add a glob filter. Reads are catchable, so a script that wants a coarser fallback writes one
— which is exactly what you need for `git`, since every `git` call rejects outside a
repository:

```ts
let paths: string[];
try {
  paths = await git.changedFiles("origin/main");
} catch {
  paths = await files.glob("src/**/*.ts");
}
```

Reach for `files.read` or `files.grep` only when the *script itself* must shard or branch on
content. Deciding which files to hand out is script work; reading them is subagent work.

**`world.run` is the deterministic gate.** When a check is fixed and machine-checkable — the
build passes, the proof checks, the tests are green — run it as code and branch on the exit
code, instead of asking a subagent to run it and trusting the claim:

```ts
const check = await world.run("lake", ["build"], { timeoutMs: 1_800_000 });
if (check.exitCode !== 0) feedback = check.stderr;
```

A nonzero exit code is a **value**, not an exception: the gating loop's normal case reads
`exitCode` and carries `stderr` forward as the next round's feedback, with no `catch` in
sight. Rejections are reserved for the world failing to answer — spawn failure, timeout
(default 300s, override per call, no cap), or output over the per-stream cap. The command
name must be a compile-time string literal, because the user approves the script's command
set at confirmation: interpolate paths, flags and round numbers into the **args array**,
never into the command. And keep the division of labor: open-ended editing belongs to a
subagent with tools; `world.run` is for checks whose outcome your control flow branches on.

**Choose the gate before you write it.** A gate decides exactly as much as the check it
runs, and the check the task needs is rarely the first one you would type from habit.
Before the first `world.run`, find out what checks the repository actually has — the
scripts in `package.json`, the targets in a `Makefile`, the CI config, the README's "run
this to verify" line — and rank them by how much they decide: a unit suite over fixtures
decides less than an integration or end-to-end suite, which decides less than the
acceptance command the README names, and none of them decides a performance request the
way the bench does. Then build the gate in two tiers. The fast tier may drive the rounds of
a loop. The **strongest** tier the request implies decides the exit, and it runs for real at
least once before the final `return` — even when the fast tier already said yes, because
the fast tier cannot see what it does not test. When the strong tier is slow, give it the
`timeoutMs` it needs; a faster substitute is a different check, not a cheaper version of
the same one.

```ts
phase("Fix until the unit tests pass");
for (let round = 1; round <= 5; round++) {
  const unit = await world.run("npm", ["test"]);
  if (unit.exitCode === 0) break;
  await fixer.ask(`The unit tests failed:\n${unit.stderr}\nFix them.`);
}

phase("Run the end-to-end suite once before handing over");
const e2e = await world.run("npm", ["run", "e2e"], { timeoutMs: 1_800_000 });
const verified = e2e.exitCode === 0;
```

A check that exists in the repository and did not run is not "not covered"; it is
unverified work (§10).

## 6. Test the pieces before you commit

A workflow script is final at submit. The fixed logic inside it does not have to be: run it
through the `EvalWorkflowSnippet` tool first, which compiles and executes a snippet against
the same compiler, sandbox and world-read path a real run uses, synchronously and without
persisting anything. What passed there pastes into the workflow verbatim. Send the snippet
as `code`, or as `path` to a file holding it when you have already written it out — the
second try then costs an `Edit` rather than the whole snippet again.

Snippet work is exactly the non-subagent part: what a glob really returns (workspace-relative,
sorted), whether a grep pattern overruns its cap, how `world.run("lean", ...)`'s stderr
parses, whether a gate predicate does what you meant. A snippet has `files.*`, `git.*`,
`world.run` and `log`, but no `agent()` — orchestration cannot be rehearsed, only the pieces
can. Testing a parse function by burning a full run is the expensive way to find a typo.
Once you know which check decides the result (§5), run it once here as well: that tells you
what its output looks like, whether it exits nonzero the way you assumed, and how long it
takes — which is the `timeoutMs` you write, not a guess.

## 7. Keep the script analyzable, and join only where the data needs everyone

The harness recovers the dependency graph by reading your code before it runs, so write code
whose data flow is visible. Node results flowing through plain variables, template
interpolation, destructuring and small local helpers are all traced exactly. Stashing a
result into a container and fishing it out later, or routing it through a clever indirection,
earns an analyzability diagnostic instead.

`Promise.all` is a join, and a join is a barrier: nothing after it starts until the slowest
item before it has landed. Parallelism comes from *not awaiting yet*:

```ts
// Concurrent: nothing is awaited until the join.
const all = await Promise.all(
  paths.map((p) => agent(`reviewer-${p}`).ask<Review>(`Review ${p}`)),
);

// Serial: each await blocks the next call. Only write this when you mean it.
for (const p of paths) results.push(await agent(`reviewer-${p}`).ask<Review>(`Review ${p}`));
```

**Join where the next step needs every item, and nowhere else.** When two stages map one to
one — a reviewer per file and a confirmer per finding, a migrator per file and a checker per
file — a barrier between them makes every confirmer wait for the slowest reviewer while the
concurrency slots sit idle. Chain the stages per item inside the fan-out instead, and join
once at the end:

```ts
// ✗ Two barriers: no finding is confirmed until every file has been reviewed.
const reviews = await Promise.all(paths.map((p) => agent(`reviewer-${p}`).ask<Review>(`Review ${p}`)));
const confirmed = await Promise.all(
  reviews.flatMap((r) => r.findings).map((f, i) => agent(`confirmer-${i}`).ask<Confirmation>(`Reproduce: ${JSON.stringify(f)}`)),
);

// ✓ One join: each file's findings go to their confirmers the moment its review lands.
const confirmed = (
  await Promise.all(
    paths.map(async (p) => {
      const review = await agent(`reviewer-${p}`).ask<Review>(`Review ${p}`);
      return Promise.all(
        review.findings.map((f, i) => agent(`confirmer-${p}-${i}`).ask<Confirmation>(`Reproduce: ${JSON.stringify(f)}`)),
      );
    }),
  )
).flat();
```

The stages that genuinely need everyone — a triage that ranks on one scale, a synthesis that
deduplicates across findings — are the legitimate barriers, and they are few. A shared,
calibrated subagent is not one of them: asks on it queue FIFO, so feeding it per item as
results land keeps it consistent *and* keeps the pipeline moving (§11).

One rejection inside `Promise.all` rejects the whole join and the sibling results with it.
When one bad item should cost one item, catch inside the callback (examples #3) or use
`Promise.allSettled` and read each outcome.

## 8. Group the run into named phases

Phases are required, not a nice-to-have. The user experiences a workflow through its
phase graph: the confirmation dialog they approve is drawn one node per phase, and a
dialog with thirty step cards in it is a dialog nobody reads. Cover the whole script —
every stage gets a `phase("...")` marker at its head, and the arrows between the phases
are inferred from the same analysis:

<!-- compile -->
```ts
interface Attempt {
  /** One sentence: the optimization this round is trying. */
  approach: string;
}

/** Total nanoseconds the bench reports, or Infinity when it printed none. */
function benchNanos(output: string): number {
  const match = /time:\s*([\d.]+) ns/.exec(output);
  return match?.[1] === undefined ? Number.POSITIVE_INFINITY : Number(match[1]);
}

phase("Measure today's baseline");
const baseline = await world.run("cargo", ["bench"], { timeoutMs: 1_800_000 });
const target = benchNanos(baseline.stdout);
const optimizer = agent("optimizer");

let winner = "none";
for (let round = 1; round <= 3; round++) {
  phase("Make one improvement");
  const plan = await optimizer.ask<Attempt>(
    `Round ${round}. Baseline timings:\n${baseline.stdout}\nMake one improvement.`,
  );

  phase("Check that the tests still pass");
  const check = await world.run("cargo", ["test"]);
  if (check.exitCode !== 0) continue;

  phase("Measure whether it is actually faster");
  const bench = await world.run("cargo", ["bench"], { timeoutMs: 1_800_000 });
  if (bench.exitCode === 0 && benchNanos(bench.stdout) < target) {
    winner = plan.approach;
    break;
  }
}

phase("Collect the final numbers");
const head = await world.run("git", ["rev-parse", "HEAD"]);
return { approach: winner, commit: head.stdout.trim() };
```

Five nodes — "Measure today's baseline" → "Make one improvement" → "Check that the tests
still pass" → "Measure whether it is actually faster" (and back to "Make one improvement")
→ "Collect the final numbers" — and they stay five when the real version of this script
grows to thirty steps. That is the entire payoff: the user sees the story you had in your
head instead of a wall of cards.

Notice the two checks. The unit tests are the fast tier: they decide whether a round is
worth measuring. The bench is the check the request was about, and it runs every time a
round survives the tests — not once at the start to print a baseline and never again. A
loop that gates on `cargo test` alone would crown the first round that compiles (§5).

**Write the names for the user, not for the graph.** A phase name is a short natural
phrase saying what this stage accomplishes for the user, written in the language the
user is speaking in this session — "Research each changed file in parallel",
"汇总并产出最终报告". Orchestration vocabulary the user never chose ("fan-out",
"gate", "aggregate") names the machinery, not the work. Names must be compile-time
string literals, so you cannot number rounds by interpolation — and you should not
want to: two markers with the same name are one node, which is how a loop body stays
one box across all its rounds.

**Put the marker where the steps are.** A marker claims the rest of the block it sits
in, nested blocks and inlined helper calls included, so a marker inside an `if` covers
that branch and stops at its closing brace. Mark the block where the `ask` and
`world.run` calls actually live.

**Not inside a concurrent fan-out callback.** The run has one current phase, and every step
is stamped with it at birth. Twenty `map(async …)` callbacks each re-entering
`phase("Review")` and `phase("Confirm")` out of order would stamp each other's steps and
count every re-entry as a round. A per-item pipeline (§7) is one phase, named for what it
does to each item — "Review each changed file and confirm its findings as they land" — and
the markers stay at the top level around it.

**Every phase must contain at least one `ask` or one `world.run`.** A phase is a stage
the user watches progress through. Plain script logic — reading `args`, shaping a
prompt, sorting results, building the final `return` — runs in a flash and shows no
progress, so it is not a stage; a phase made only of it sits on the timeline as a step
that never does anything. Fold that logic into the phase before or after it. In
particular, do not open a phase for the setup at the top of the script or for the
`return` at the bottom; the first phase starts at the first ask, and the last phase is
the last stage that asks or runs something. The same goes for a branch that only
narrates: an `if` whose body is nothing but `report(...)` and `log(...)` does not get
its own marker.

## 9. Write for the user

Six things you write are read by the user, not by the script: phase names, subagent names,
`log` lines, artifact titles and the markdown you publish, the four report fields, and any
question a subagent escalates. Write each one the way you would tell a colleague across the
desk what is happening: say what the work is, in the words the user would use, in the
language the user is speaking in this session. The machinery — fan-out, gate, node, stage,
pipeline — is yours, not theirs, and so is the numbering.

| Read by the user | Write | Not |
| --- | --- | --- |
| Phase name | Review each changed file / 逐个检查改动的文件 | Fan-out review stage / 文件评审阶段 |
| Phase name | Check that the tests still pass / 确认测试仍然通过 | Gate: test verification / 执行测试验证任务 |
| Subagent name | Code reviewer / 代码评审员 | reviewer_2 / 节点3 |
| Subagent name | Benchmark runner / 跑基准测试的人 | runner / 子代理A |
| Phase name | Independent review of the final plan / 请没看过方案的人再审一遍 | Cold read of the plan / 冷读方案 |
| Subagent name | Independent reviewer / 独立评审员 | cold-reviewer / 冷眼评审 |
| `log` line | Reviewing 12 changed files / 正在检查 12 个改动的文件 | Fan-out sized: n=12 / 扇出阶段初始化完成 |
| Report conclusion | Two of the twelve files have real bugs, both in the parser. / 12 个文件里有 2 个有真问题，都在解析器里。 | The workflow executed successfully and produced findings. / 工作流已成功执行并生成结果。 |
| Artifact title | Review report / 评审报告 | report-artifact-v1 / 产物输出 |
| Escalation question | Should the threshold be 94 rather than 96? / 阈值应该是 94 而不是 96 吗？ | Requesting clarification regarding gate configuration parameters / 请求对门控配置参数进行澄清 |

The method names in this skill — fresh eyes, independent read — are English idioms for you,
not words for the user. Carry the idea across, not the words: in Chinese that is 独立复核 or
换人复审, never a word-for-word rendering such as 冷读 or 冷眼, which are not Chinese.

The language travels through your asks. Findings, summaries and anything else the user will
read are written by subagents answering the ask you wrote, so write those asks in the user's
language, or say in the ask which language to answer in. Otherwise a Chinese report arrives
wrapping English findings.

## 10. Verify, report, deliver

`log(...)` is narration for the human watching the run. Use it at the points where a reader
would otherwise wonder whether anything is happening — after a fan-out is sized, at the top
of each loop round.

**Verify before you report, in proportion to what a wrong claim costs.** Verification
exists to buy down the cost of the user acting on something false, so it goes where that cost
is real: a bug they will fix, a security claim, a number they will quote, a fact a decision
rests on. Each finding of that kind is confirmed independently before it reaches the user: a
second subagent that reproduces the finding from its evidence alone — reading the code,
running a check when one decides it, never editing — or a `world.run` command when one can
decide. The reviewer that found the problem does not get to confirm it; self-confirmation is
not confirmation. A finding that fails confirmation is **kept and labelled** `unconfirmed`,
never silently dropped — a real issue the confirmer could not reproduce is still worth a
human's eyes, and the label is what lets the user tell "seen" from "suspected".

Three things that look like verification and are waste. A confirmer on a finding a
`world.run` already decided: the exit code is the confirmation, so record the command in
`verified` and move on. A confirmer on work nobody will act on as fact — poems, brainstormed
options, a first-draft survey — which gets at most one independent read (§3). And a suite run
three times because the hunter ran it, the confirmer ran it and then the script gated on it:
a check the script runs as a gate is run once, by the script, and the asks say so ("the
script runs the full suite after you; do not run it yourself") so the subagents spend their
turns on what only they can do.

**Gate at the task's scale.** `verified` names the commands that actually decided the
result, and the reader will hold them against what they asked for. A unit suite in
`verified` and the end-to-end suite in `notCovered` is not a verified deliverable; it is an
unverified one with a disclosure attached. `notCovered` is for what *could not* be checked —
no test exists for it, the environment lacks the tool, the check needs something only the
user has — never for a check that exists in the repository and was skipped because it is
slow or because the fast tier already passed. If the strongest check the request implies
exists and did not run, the run is not done: run it, or say in `conclusion` that the work
is unverified and why (§5).

**Salvage by report.** `report(...)` is different from `log` and more important: reported
items are delivered with the completion notification **even when the run fails**. A
forty-task run that dies on task twelve still did eleven tasks' worth of work, and `report`
is the only thing that gets that work out. So report each finding at the moment it lands —
after its confirmation, with its status — rather than accumulating an array and returning it
at the end: the array is what you lose. Items are recorded, so a resumed run never shows the
same one twice.

Give open-ended work an explicit round cap in the script rather than an unbounded loop: the
harness enforces no node limit, so a loop that never goes dry only ends when the user cancels.

**Deliver a report.** The script's final `return` is the handoff the main agent presents,
in the order below; what the user themselves keep is the artifact of the next section.
Return this shape (copy the interfaces; they compile as-is) rather than a bare array:

<!-- compile -->
```ts
interface Finding {
  /** Workspace-relative path, with a line when it applies: "src/a.ts:42". */
  where: string;
  /** One sentence: what is wrong, or what was found. */
  what: string;
  /** What showed it: the lines read, or the command and the output that proved it. */
  evidence: string;
  /** "verified" when an independent subagent or a deterministic check confirmed it; "unconfirmed" when confirmation failed or was not attempted. */
  status: "verified" | "unconfirmed";
  /** How much it matters. Reserve "high" for data loss, a crash, or a wrong result. */
  severity: "low" | "medium" | "high";
}
interface WorkflowReport {
  /** Two or three sentences answering what the user asked for. */
  conclusion: string;
  findings: Finding[];
  /** What the run checked and how: the commands it ran, the files it covered. */
  verified: string[];
  /** What the run did not look at or could not check, and why. */
  notCovered: string[];
}

const result: WorkflowReport = {
  conclusion: "Nothing was reviewed: this snippet only shows the shape.",
  findings: [],
  verified: [],
  notCovered: ["everything — no subagent ran"],
};
return result;
```

Before returning it, give it an independent read (§3): a reader-proxy subagent that has seen nothing else
tells you what is unclear, unsupported, or missing, while there is still time to fix it.

Each field has a reader. `conclusion` is the answer to what the user asked. `findings` carry
their evidence and their status, so the user can tell what was seen from what was suspected.
`verified` says what this run actually checked and how — the commands it ran, the files it
covered. `notCovered` says what it did not look at and why, which is the difference between
"we found nothing" and "we looked nowhere". Write all four in the language the user is
speaking in this session.

**Deliver artifacts.** The `return` is read by the main agent, which retells it. `artifact.*`
is the other channel: things the *user* opens, shown as cards beside the run while it is
still going and kept after it ends. Two habits:

- **Every run publishes its deliverable.** The deliverable is whatever the user asked for.
  When that is a thing — a webpage, a PDF, a spreadsheet, a generated site — a subagent writes
  it into the workspace with its own tools and returns the path, and the script publishes that
  path with `artifact.file`. Until it does, nothing the run left in the workspace exists as far
  as the user is concerned, which is why "write it to `out/…` and return the path" belongs in
  the ask that produces it. When the user asked for an answer — findings, a review, an
  analysis — the deliverable is a report, and the `return` alone is not it: the return is a
  compact handoff the main agent retells, while the user deserves the long form of the same
  facts, the findings with their evidence, what was checked and what was not. Markdown
  composed from the same values the return is built from is the usual shape; a subagent-written
  document is fine when the user asked for one. Publish it once, at the end, when the facts are
  settled. The one exception is a run whose whole answer is a line: a verdict, a number, a yes
  or no. A page that restates one sentence is noise, so let the return carry it alone.
  When the run publishes more than one artifact, mark the deliverable `{ primary: true }`: the
  completion card and the run pane lead with it (its `description` is shown beside the title),
  and the notification the main agent reads lists it first. One id per run; a run whose only
  artifact is the deliverable needs no flag.
- **A dashboard is for the person watching the run.** Its job is to answer "where is it, how
  is it going" without opening anything. Declare one when the run has state worth watching
  while it is still running: the key number after each round, which items are done and which
  failed. Metrics or a chart when the watched thing is a number; a table or board when it is
  a set of items with a status. Declare it once at the top, then tag the items you are
  already reporting: `report(item, "perf")`. Nothing else is needed; the dashboard is a
  projection of that stream, so it grows live and comes back identical after a resume. A run
  that is over before anyone would look at it needs none.

**Show what matters.** More artifacts is not more delivery: every card competes with the
deliverable for the user's attention. Two tests decide what earns a card. Would the user open
it on its own? If not, it is a section of the deliverable. Does it repeat another artifact? A
CSV and a table of the same rows, a report and a copy of it, a chart and a metrics tile of the
same number — one of each pair is noise. The common run publishes one deliverable, one
dashboard when there is something to watch, and further files only when a subagent produced
something else a person opens.

Ids and the report tag are compile-time string literals. What a run can publish is therefore
fixed at the moment the user approves the script, and an id assembled at runtime is a compile
diagnostic rather than a surprise. Publishing an id a second time mints the next version and
keeps the old one, so a file worth republishing each round costs you nothing to name once.

**A rejected publish is a repair opportunity, not an error to swallow.** Content publishes
reject catchably — the file is missing, the path escaped the workspace, the bytes are over the
cap — and the honest response is to hand the gap back to a subagent and publish again, rather
than let a run end with nothing the user can open:

<!-- compile -->
```ts
interface RoundOutcome {
  /** 1-based round number. */
  round: number;
  /** Query latency in milliseconds, measured after this round's change. */
  queryMs: number;
  /** One sentence: what this round changed. */
  change: string;
}

const TARGET_MS = 20;
artifact.chart("perf", {
  title: "Query latency by round",
  x: { field: "round", label: "Round" },
  y: { field: "queryMs", label: "ms" },
  baseline: { field: "target" },
});

phase("Optimize until the bench meets its target");
const optimizer = agent("optimizer", {
  system:
    "You speed up a JSONL query engine: change one thing, measure it, and write the profile " +
    "of the query path to out/profile.html.",
});
const rounds: RoundOutcome[] = [];
for (let round = 1; round <= 3; round += 1) {
  const outcome = await optimizer.ask<RoundOutcome>(
    `Round ${round}: make the query faster, then measure it.`,
  );
  rounds.push(outcome);
  report({ ...outcome, target: TARGET_MS }, "perf");
  if (outcome.queryMs <= TARGET_MS) break;
}

phase("Hand the user the profile and the write-up");
try {
  await artifact.file("profile", "out/profile.html", { title: "Query profile after the last round" });
} catch {
  const profiler = agent("profiler");
  await profiler.ask("out/profile.html is missing. Profile the query path as it stands and write the profile there.");
  await artifact.file("profile", "out/profile.html", { title: "Query profile after the last round" });
}
const latest = rounds[rounds.length - 1]?.queryMs ?? 0;
await artifact.markdown(
  "report",
  [
    `# Query latency: ${latest}ms against a ${TARGET_MS}ms target`,
    "",
    ...rounds.map((r) => `- Round ${r.round}: ${r.queryMs}ms — ${r.change}`),
  ].join("\n"),
  {
    title: "Optimization report",
    description: `What each of the ${rounds.length} rounds changed and what it bought.`,
    primary: true,
  },
);

return {
  conclusion: `Latency landed at ${latest}ms against a ${TARGET_MS}ms target.`,
  verified: ["each round's latency was measured by the subagent that made the change"],
  notCovered: ["memory use; only query latency was measured"],
};
```

One `report` call feeds both surfaces: the tagged item lands on the chart *and* in the run's
progressive results, so tagging costs nothing and adds a picture. Three cards, none repeating
another: the chart is what the user watched, the profile is what a person opens, the report
is what they keep. The CSV the bench also wrote stays in the workspace — its rows are the
chart's points, so a card for it would say nothing new.

## 11. A complete example

The complete review workflow that used to sit here — a reviewer per changed file, triage
through one shared subagent that is fed as reviews land, a confirmer per kept finding chained
inside the same callback, one join for the cross-file deduplication — is
`${ZCODE_SKILL_DIR}/examples.md` §5. Read it when you want to see a whole script's arc; §7
and §10 already carry its rules.

## 12. Anti-patterns

| What you wrote | What it costs you |
| --- | --- |
| Hoisted one subagent out of a fan-out for tidiness | The fan-out silently became a FIFO queue |
| Gave every subagent in a fan-out the same fixed name | Every name in a run must be unique: a literal one is rejected at compile time, a computed one kills the run at the second item. Name per item or stay anonymous |
| A fresh subagent each loop round | Round five re-learns everything round four knew, at full price |
| Accumulated findings in an array, returned at the end | A failure on the last task loses all of them; `report` as you go |
| Interpolated a whole file into the prompt | Paid tokens to tell a subagent something its own tools would have read |
| A wide `files.grep` with no glob | The call rejects; it does not quietly hand you a partial workspace |
| An unbounded `while` | The run dies on a cap instead of finishing |
| Asked a subagent to run the tests and report whether they passed | Paid a session for what `world.run` does as code — and trusted a pass/fail claim the subagent can fake |
| Built the `world.run` command name from a variable or template hole | Compile fails: the user approves the script's command set, so the command must be a literal; runtime values belong in the args array |
| Submitted a workflow to find out whether a parse function works | A full run spent on what `EvalWorkflowSnippet` answers synchronously |
| Dereferenced a `.find()` / `.match()` result or an optional property without a guard | Compile fails: `strict` keeps those `T \| undefined` / `null`; plain `items[i]` needs no guard (`noUncheckedIndexedAccess` is off) |
| `Date.now()` / `Math.random()` / `fetch` / `fs` | Rejected at compile time; a replayable run cannot contain them |
| Awaited each call in a loop that had no ordering requirement | Serial wall-clock for concurrent work |
| Interpolated a tunable constant (a threshold, a cap) into an ask message | Amending that one number rewrites every prompt that mentions it: an `AmendWorkflow` re-run misses cache at the first such ask and re-pays everything downstream |
| Named a phase or a subagent after the machinery, in either language ("fan-out", "文件评审阶段", "节点3"), or shipped the script with no markers at all | The user approves a graph whose stages say nothing about their work — or thirty cards with no story (§9) |
| Wrote a persona that says only "make the check pass" | Against a check it cannot pass, faking one is the obedient reading. Tell the subagent to escalate an impossible gate instead (§14) |
| Asked the reviewer "is this good?" | A reviewer asked for approval approves; ask what would break it and what is missing (§3) |
| Reported findings nobody confirmed | The user gets a list that cannot tell "seen" from "suspected"; confirm each finding independently and label the ones that failed (§10) |
| Gated the loop on the unit tests and never ran the integration suite, the end-to-end suite, or the bench the request was about | The work passed the check that was cheap, not the one that decides; the strongest check runs at least once before `return` (§5) |
| Wrote the gate from habit (`cargo test`, `node --test`) without reading the project's own scripts | The `package.json`, `Makefile` or README already named a stronger check, and the user knows it; survey the repository's checks before you gate (§5) |
| Put a check that exists into `notCovered` because it was slow | An honest-looking report of unverified work; `notCovered` is for what could not be checked (§10) |
| Returned a bare array as the run's result | The main agent has to improvise the deliverable, so the user gets a different shape every time; return the report shape (§10) |
| Returned the report and published nothing | The user gets the main agent's retelling and nothing to keep; publish the deliverable — the file they asked for, or the findings in long form (§10) |
| Published a CSV and a table of its rows, or a report and a copy of it | Two cards for one fact: the second is noise the user has to look past (§10) |
| Declared a dashboard for a run nobody watches | Three points on a chart for a run that was over before anyone looked; a dashboard is for the person watching (§10) |
| Stacked a reviewer loop, a fresh final reviewer, a per-finding confirmer and a reader-proxy read on one deliverable | Four sessions re-reading the same work; one deliverable gets one mechanism, two at most with a reason (§3) |
| Sent a confirmer after a finding a `world.run` already decided, or let the hunter, the confirmer and the gate each run the suite | The same check paid three times; the exit code is the confirmation, and the gate runs the suite once (§10) |
| `Promise.all` between two stages whose items map one to one (review every file, *then* confirm every finding) | The slowest reviewer gates every confirmer while the slots idle; chain the stages per item and join once (§7) |
| Waited for a run you already knew was wrong to finish, or stopped it and left it | Everything after the fix point is re-paid either way; call `AmendWorkflow` on it now, while it runs (§13) |
| Pasted a whole revised script inline after a diagnostic or an error, instead of editing the file the tool named | Twenty thousand tokens re-streamed to change one line, on a provider that may stall on the resubmit — and a script compaction has dropped is one you can no longer paste; the file is on disk, so the revision is an `Edit` and a `path` (§13) |

## 13. After you submit

Compilation happens first. **Diagnostics mean nothing ran** — there is no partially-started
run to clean up — and they name the file your script is in. Every script you submit has one:
an inline `script` is written under `.zcode/workflow-drafts/` before it is even compiled and
the response hands you the path; a script you submitted by `path` is that file already. Each
diagnostic reads `{path}:L{line}:C{column} {message}`, counted in the file, so it addresses
an `Edit` as it stands.
**Edit that file and resubmit with `path`. Do not paste the script inline again**:
re-streaming twenty thousand tokens to change one line is slow enough that some providers
stall on it, while the `Edit` that fixes it costs a few lines. (If the response names no
file — a checkout the tool could not write to — then fix the script and resubmit it inline,
as before.)

On a clean compile the run starts in the background and you get a run ID. **Do not poll it.**
The completion notification arrives on its own and carries the final return value plus every
reported item. Go do other work, unless the user asked you to wait.

When you genuinely need to look at a run:

- `TaskOutput` blocks until a run *this session started* finishes. That is the waiting tool.
- `GetWorkflowRun` is an instant snapshot and never waits. Use it when you must not block,
  or for a run another session owns — `TaskOutput` cannot see those.
- `ListWorkflowRuns` enumerates the project's runs, including other sessions'.

Read the terminal state precisely. A run ends in exactly one of three states:

- **completed** — the script returned.
- **errored** — the script itself failed (an uncaught throw, a cap it overran, an artifact
  whose source file was missing). Replaying it would fail the same way, so
  `ResumeWorkflowRun` refuses it; edit the run's script file, which the notification names,
  and resubmit it with `AmendWorkflow` and `path`.
- **stopped** — the run was stopped and can be resumed as-is with `ResumeWorkflowRun`. The
  notification's `<stop-reason>` says why: `user` (the user stopped it — leave it alone
  unless they ask), `model` (you stopped it with TaskStop), `interrupted` (the process that
  owned it exited; resume it), or `provider` (a subagent hit a deterministic model-side
  error — sign-in expired, model not in the plan, quota cap; the `<error>` block names the
  cause and the fix — resolve it with the user, then resume). A fifth reason, `superseded`,
  never arrives as a notification: it is the state of a run you amended away, and the
  `AmendWorkflow` result already told you. Such a run is not resumable; its successor is
  the live one.

Reported items come back on errored and stopped runs too, so a dead run is still worth
reading. Model errors never reach the script: rate limits, overload, network errors,
timeouts and unknown provider errors are retried inside the run without limit while the
fan-out adapts to what the provider accepts, so a run that looks stalled with a "waiting for
provider" badge is waiting, not broken. If nothing succeeds for twenty minutes you get one
informational stall notification; the run is still running and needs nothing from you.

**When the script itself was wrong, do not start over.** Any run — errored, stopped,
completed, or still running — can be superseded: edit the run's script file and call
`AmendWorkflow` with `run_id: "<runId>"` and that `path`. That starts a new run which
imports the old one's finished work, matched per named subagent along its sequence of asks,
so every step you did not touch settles from cache at no token cost and only the changed
part actually runs — with one deliberate limit: the moment a live subagent writes to the
workspace (or a live `world.run` executes), the workspace may no longer be the one the old
results were computed against, so from that point every cached `world.run`, every cached
world read and every cached ask whose subagent had read or run something runs live even if
its text is unchanged; asks whose subagent only answered keep settling from the cache. A gate
command therefore always tests the code the amended run actually produced; the price is
that doing steps after the first live write are re-paid. The old run is left exactly as it
was. This is the move after a `ScriptError`, after a bad prompt
produced a useless result, and when a completed analysis needs one more stage — rewriting
the workflow from scratch throws away work that was already paid for. Two things make the
cache hit: names that stay the same across the revision, and asks whose instructions stay
byte-identical (an upstream change cascades — a changed result changes what interpolates
into everything downstream, which is what you want).

**Every run remembers its script file, so a revision is an `Edit`.** The terminal
notification names that path, and `GetWorkflowRun` reports it as `scriptPath` — which is why
you never have to keep the script in context: by the time a long run errors, compaction may
well have dropped the text you sent, while the file on disk has not moved. Edit it in place
and pass `path`; pass `script` only when what you are submitting is genuinely new. A `path`
whose bytes still equal what the run already ran is refused as `script_unchanged`, with
nothing stopped and nothing created — that refusal means your `Edit` did not land, not that
the run cannot be amended (changing only `max_concurrency` or `subagent_model` is a real
change and goes through).

**Editing a draft asks nothing.** `.zcode/workflow-drafts/` is machine-owned and
git-ignored, so an `Edit` or `Write` under it is pre-approved and opens no confirmation
window. Editing a script is not running one: the window still stands between the file and
the run, so fix the file freely and let the submission be the thing the user answers.

**Repair a run while it is still going.** The same call works before the run ends, and it
is cheaper the earlier you make it. When the user points at a run that is heading the wrong
way, or a reported item shows the script is wrong, do not wait for the completion
notification and do not stop the run first: call `AmendWorkflow` on it. The tool stops the
running predecessor, waits for it to settle, imports everything that settled before the
stop at zero cost, and starts the revision — one call, no `TaskStop`, no polling for the
stop. Nothing after the stop was ever paid for, and nothing the predecessor finished is paid
twice: the cache stays open until the revision's first write to the workspace, and asks whose
subagent only answered, without reading or running anything, keep replaying even after that. The
old run shows as "superseded" and sends no notification of its own; the result you get back
names both runs. A run you amended is not left for the user to ask about: the amend result
is the account of what happened to it.

That cascade rule has a converse that decides whether your revisions are cheap or ruinous.
Interpolating an upstream **result** into a prompt is safe: on resume the result replays
byte-identical, so the prompt does too. Interpolating a script **constant** is the
opposite bet — constants are exactly what an amendment tunes, and every ask whose text
mentions one is an ask whose cache you forfeit by tuning it, plus everything downstream of
it:

```ts
// ✗ Couples every revision ask to a knob you will want to tune. Amending
//   PASS_THRESHOLD (say 96 → 94) rewrites this text, so the resumed run misses
//   cache at the first revision and re-pays every ask after it — to reproduce
//   answers the old run already holds.
draft = await writer.ask<Draft>(
  `Scored ${review.score}; passing needs ${PASS_THRESHOLD}. Revise: ${JSON.stringify(review.comments)}`,
);

// ✓ The prompt carries only replayed upstream values and the fact that the
//   branch was taken; the threshold comparison already happened in script code.
//   Amending the threshold now moves only where the loop exits — every round
//   before that point settles from cache.
draft = await writer.ask<Draft>(
  `The review did not pass. Address each comment: ${JSON.stringify(review.comments)}`,
);
```

Keep the tunable knobs — thresholds, round caps — in the script's control flow,
where amending them is free, and out of ask text, where amending them is a cache purge.
A subagent almost never needs the number anyway; it needs the verdict and the feedback.

## 14. When a subagent escalates

Every subagent can escalate a blocking question to you while the run is going. Nothing in the
script switches this on and no persona field controls it — the tool is injected the way
`submit_result` is. It exists for the one thing a script
cannot design its way around: a subagent walled in by something that is not its fault. A gate
it cannot pass (a check capped at 95 against a threshold of 96), two instructions no single
output satisfies, or a fact only whoever started the run knows. With no way to ask,
a walled-in subagent has two moves left and both are bad — grind until the round cap, or
fake its way past.

**What reaches you.** A notification arrives mid-run carrying the run, the subagent, the
question with whatever evidence the subagent attached, and a globally unique question id shaped
like `dwfq-...`. Only the subagent that asked is parked, and only on that one call: its
siblings, the script's control flow and the run's status all carry on unchanged. Nothing
times out on its behalf either — it waits until you answer or until the run is cancelled. So
you need not drop what you are doing, but you cannot leave it unanswered.

**Then decide which of two things is true.**

- The question **has an answer** — a clarification, one missing fact, a tradeoff only you can
  call. Answer it with `ResolveWorkflowQuestion` and that `question_id`. Your text becomes
  the result of the subagent's own call, verbatim, and it continues its turn from there. Not
  sure? Read the run with `GetWorkflowRun`, or put the question to the user with
  `AskUserQuestion` — then come back and answer, because nothing answers in your place.
- The **script** is what is broken — a gate no output can pass, control flow routing work to
  the wrong subagent. No sentence fixes that. Edit the run's script file and call
  `AmendWorkflow` on it with that `path` (§13); it stops the run for you. The escalation is
  what makes that amendment cheap: the ask that escalated never settled, so it sits exactly
  past the cache boundary — everything before it imports from the old run at no token cost,
  and it re-runs against the fixed script.

**If the notification never arrives**, the question is still discoverable. `GetWorkflowRun`
lists what a run still owes an answer to under `pendingQuestions`, ids included. That
snapshot is the fallback when a notification is dropped, and the way to check on a run you
suspect is waiting on you.

**Write personas that make honesty the cheap move.** A subagent follows the instructions you
wrote, so a persona that says "make the check pass" and stops there leaves faking a pass as
the obedient reading. One sentence closes that off; adapt this one: *"If a check is
impossible to pass, or your instructions contradict each other, escalate and say so plainly
rather than working around it."*

One ask gets three escalations. The fourth comes back as an ordinary result telling the
subagent its escalation allowance is spent and to proceed on its own best judgement — a guard against
chatter, not an allowance to spend.

## 15. Going deeper

- `${ZCODE_SKILL_DIR}/patterns.md` — the topology catalogue: fan-out/fan-in, review sweeps,
  planner-reviewer loops, judge panels, staged pipelines, bounded discovery,
  `world.run`-gated verifier loops. Read it
  when you know the shape you want and want it written correctly.
- `${ZCODE_SKILL_DIR}/examples.md` — complete worked scripts, including a two-subagent
  adversarial prove/disprove loop. Read one when you want to see a whole script's arc.

## 16. Tool reference

The four authoring tools — `CreateWorkflow`, `AmendWorkflow`, `SaveWorkflow` and
`EvalWorkflowSnippet` — carry one-paragraph descriptions on purpose, and each of them refuses
to run until this skill has been loaded in the session. This section is the part of their
contract that the descriptions do not carry: the facade the compiler checks against, the
rules a script must satisfy, and the fields each tool takes. Read it before writing or
revising a script; the compiler holds you to it.

### 16.1 `CreateWorkflow`

Typechecks the script; on a clean compile asks the user to confirm and starts the run in the
background; you are notified with the script's final `return` value when the run settles.
Compilation errors come back as diagnostics, counted in the script's file (§13).

**Three sources; pass exactly one.**

- `script`: a one-off workflow written inline. It is saved to a file under
  `.zcode/workflow-drafts/` and the result names that file, whether the script compiled or
  not; revise it by editing the file and resubmitting with `path`, never by pasting the
  script again.
- `saved`: a workflow saved in this project or globally, by name —
  `saved: { name: "pr-review", args: { pr: "123" } }`. Its arguments are validated against
  its declaration (unknown keys, missing required values and wrong types are rejected)
  before anything runs. Before writing a workflow from scratch, check `ListSavedWorkflows`:
  a saved workflow the user reviewed and kept beats a rebuilt one. Running a saved workflow
  is the one call that does not need this skill loaded.
- `path`: a script file on disk, relative to the working directory or absolute — normally
  the file a previous result named. Pass `args` alongside it when the file declares
  arguments in its `/* zcode-workflow` block.

Either way the user confirms the run, and the confirmation shows the actual script.

**Fields.**

- `name`: a short label for the run in the user's language ("PR review", "代码评审").
  Always pass it for an inline script; it labels the run everywhere and names its draft
  file. It defaults to the saved workflow's name.
- `max_concurrency`: an upper bound on how many subagents work at once. Set it only when
  the user asks to limit parallelism — never on your own initiative and never as a
  reaction to provider rate limits or errors, which the runtime already adapts to. A value
  above the machine's ceiling is lowered to it. Otherwise how many subagents run at once is
  the runtime's decision, not the script's.
- `subagent_model`: the model the subagents run on, as `providerId/modelId` or a bare
  model id (optionally `$reasoningLevel`). Set it only when the user asks for a specific
  model; pass the name the user used, and if the tool cannot resolve it, pick from the ids
  it lists or call `ListModels`. You stay on the session model either way.

This tool starts a new run. To change a run that exists — errored, completed, stopped or
still running — call `AmendWorkflow` (§13, §16.4), never `CreateWorkflow` again.

### 16.2 The facade

Every script and snippet is checked against these declarations. They are the same constant
the compiler is fed, so what you read here is what typechecks. The block uses `declare`
because it is the host's ambient API description — do not imitate it; your script defines
plain interfaces and never uses `declare`.

<!-- facade-dts:start -->
```ts
/**
 * A node: one task assigned to an actor, producing a typed result.
 * Thenable — await it, or combine with Promise.all for joins.
 */
declare interface Node<T> extends PromiseLike<T> {}

/**
 * Persona of an actor: identity, fixed at creation (frozen for the actor's lifetime). Every
 * actor has the regular working tools (reading, searching, editing, running commands); what
 * it may do with them is said in the ask.
 */
declare interface AgentPersona {
  /** System prompt describing the actor's role. */
  system?: string;
}

/**
 * An actor: a persistent conversational context that executes tasks serially.
 * Context accumulates across asks; concurrent asks on one actor queue FIFO.
 */
declare interface Agent {
  /**
   * Assign one task. T is the task's output value: an interface you define in
   * this script with a plain "interface" declaration (no "declare" modifier; the
   * harness synthesizes its runtime schema from the type), or the final response
   * text when the type argument is omitted.
   */
  ask<T = string>(instructions: string): Node<T>;
}

/**
 * Create a fresh actor. Every call creates a new context; sharing context means
 * sharing this reference.
 *
 * The name is optional, but a non-empty one is an identity, not a label. It must be
 * unique within the run: two actors under the same name fail the whole run. It is
 * also the key a revised re-run matches its cache on (AmendWorkflow imports the
 * finished work of each named actor), so stable, meaningful
 * names carry work across script revisions. Anonymous actors are legal and never
 * reuse imported work.
 *
 * In a fan-out or a loop each iteration is a separate actor, so a single static name
 * there is the duplicate case: give each one its own name (agent("reviewer-" + file))
 * or leave them all anonymous. A literal name in a loop is reported when the script
 * is compiled; a computed one fails at run time.
 */
declare function agent(name?: string, persona?: string | AgentPersona): Agent;

/**
 * The run's arguments: the values supplied when this workflow was started.
 *
 * A workflow saved into the project declares its arguments (name, type, whether they
 * are required, defaults); the host validates the caller's values against that
 * declaration and fills in defaults before the run starts, so what lands here is
 * always a complete, checked bag. For an inline script — and inside a snippet — it is
 * simply empty.
 *
 * Always defined, so reading args.target is a plain property read rather than a crash.
 * The values are typed unknown on purpose: the compiler surface must not change from
 * one workflow to the next, so narrow them in the script -- String(args.target), or a
 * typeof guard -- exactly as you would any other external input.
 */
declare const args: Readonly<Record<string, unknown>>;

/** Emit a progress message to the user. */
declare function log(message: string): void;

/**
 * Publish one intermediate result while the run is still going. Like log() it
 * returns nothing and there is nothing to await — a finding has no reply.
 *
 * Unlike log() it is journaled: a resumed run never shows the same item twice, and
 * the items are delivered with the completion notification even when the run ends in
 * failure. That is the point of it — a run that dies on its twelfth of forty tasks
 * still did eleven tasks' worth of work, and reported items are how that work
 * survives.
 *
 * Two caps, and both fail the whole run rather than the call (there is no rejection
 * channel in a void return): at most 256 items per run, and at most 32KB per
 * serialized item. They are generous on purpose — report findings, not chatter.
 *
 * The item must be JSON-serializable: plain objects, arrays, strings, numbers,
 * booleans, null. Functions, class instances, Date and promises are rejected when
 * the script is compiled.
 *
 * The optional second argument routes the item to a dashboard artifact: pass the id
 * of a preset declared with artifact.chart / table / metrics / board, and this item
 * becomes one more point, row, tile value or card on it — the dashboard is nothing
 * but the items tagged with its id. The tag must be a compile-time string literal
 * naming a preset the script declares (anywhere in the text, but the declaration must
 * have executed by the time this call runs); a tag that names nothing, or names a
 * file/markdown artifact, fails the run. An untagged report is unchanged: it goes to
 * the run's Results, and a tagged one goes to both.
 */
declare function report(item: unknown, artifactId?: string): void;

/** A published artifact version: the id it was published under, and which version this call minted. */
declare interface ArtifactRef { id: string; version: number }
/** Card metadata every artifact kind accepts. */
declare interface ArtifactOptions {
  /** Shown as the card title; defaults to the id. In the user's language. */
  title?: string;
  /** A sentence or two, shown beside the title when this artifact leads the card. */
  description?: string;
  /** The run's deliverable: the card and the run pane lead with it. At most one id per run; once set it stays set for later versions. */
  primary?: boolean;
}
declare interface ArtifactFileOptions extends ArtifactOptions {
  /** Overrides the type sniffed from the extension ("application/pdf", "text/html", …). */
  contentType?: string;
}
/** One value taken from a reported item: a dot path into the item ("timing.after"). */
declare interface ArtifactField { field: string; label?: string; unit?: string }
declare interface ChartSpec extends ArtifactOptions {
  type?: "line" | "bar" | "scatter";        // default "line"
  x: ArtifactField;
  y: ArtifactField | ArtifactField[];        // several = several series
  scale?: "linear" | "log";                  // y axis, default "linear"
  /** A reference value drawn as a horizontal rule, taken from the first item that has the field. */
  baseline?: ArtifactField;
}
declare interface TableSpec extends ArtifactOptions {
  columns: ArtifactField[];
  /** Field that identifies a row; a later item with the same key replaces the row. Absent = append-only. */
  key?: string;
}
declare interface MetricsSpec extends ArtifactOptions {
  /** Each tile shows the value from the latest item that has the field. */
  metrics: ArtifactField[];
}
declare interface BoardSpec extends ArtifactOptions {
  /** Field identifying a card; a later item with the same key moves/updates the card. */
  key: string;
  /** Field holding the card's column. */
  status: string;
  /** Column order. Items whose status is not listed land in a trailing "other" column. */
  columns: string[];
  /** Field for the card title (default: the key) and extra fields shown on the card. */
  cardTitle?: string;
  detail?: ArtifactField[];
}
/**
 * Publish what the user should see: the run's own deliverable surface, kept after it ends.
 * Two habits. (1) EVERY RUN PUBLISHES ITS DELIVERABLE, whatever the user asked for: a webpage
 * or PDF a subagent wrote goes out via file(); an answer (findings, a review) goes out as the
 * long form of the facts the return summarises, usually via markdown(). Once, at the end;
 * skip it only when the whole answer is one line. When the run publishes more than one
 * artifact, mark the deliverable { primary: true }. (2) A DASHBOARD IS FOR THE PERSON WATCHING
 * THE RUN: declare one when there is state worth watching mid-run (the key number per round,
 * which items are done) and none when the run is over before anyone looks. Two tests keep it
 * to what matters: would the user open it on its own? does it repeat another artifact? A CSV
 * and a table of its rows: one of them is noise.
 *
 * Every id is a compile-time string literal (non-empty, at most 64 characters of
 * [A-Za-z0-9_.-]); the set a run can publish is fixed at submit time, so the compiler
 * rejects a computed one. Within one run an id belongs to exactly one member.
 * The two families are deliberately asymmetric, and the asymmetry is the whole design:
 * - CONTENT (file, markdown) are EFFECTS: async, resolve to an ArtifactRef, and REJECT
 *   catchably — missing file, not a file, path outside the workspace, over the size cap, no
 *   store. try { await artifact.file("book", "out/book.pdf") } catch { …ask a subagent to
 *   write it… } is the intended idiom. Bytes are copied at publish time, so later workspace
 *   edits never rewrite a version; republishing an id mints the NEXT version and keeps the
 *   old ones (at most 16 per id).
 * - PRESET (chart, table, metrics, board) are DECLARATIONS: synchronous, return nothing,
 *   never touch a file; they say how items tagged with their id are drawn. Declare each ONCE,
 *   at the top, then feed it with report(item, "<id>"). The same id with an identical spec is
 *   a no-op; a DIFFERENT or malformed spec fails the whole run — a void return has no
 *   rejection channel, exactly as with report().
 * Caps: 32 ids per run, 16 versions per id, 20 MiB per file, 256 KB per markdown, 120
 * characters of title and 500 of description.
 */
declare const artifact: {
  /**
   * Publish a file from the workspace. path is workspace-relative, resolved by the same
   * resolver files.read() uses; the bytes are copied at publish time. The content type is
   * read off the extension unless opts.contentType overrides it. Rejects (catchably)
   * rather than publishing something empty.
   */
  file(id: string, path: string, opts?: ArtifactFileOptions): Promise<ArtifactRef>;
  /** Publish markdown text the script composed: the usual shape of a report deliverable, the long form of what the return summarises. */
  markdown(id: string, content: string, opts?: ArtifactOptions): Promise<ArtifactRef>;
  /** Declare a chart fed by report(item, id): each tagged item is one point. */
  chart(id: string, spec: ChartSpec): void;
  /** Declare a table fed by report(item, id): each tagged item is one row. */
  table(id: string, spec: TableSpec): void;
  /** Declare a metric tile row fed by report(item, id): each tile shows the newest value it has. */
  metrics(id: string, spec: MetricsSpec): void;
  /** Declare a board fed by report(item, id): each tagged item is a card, placed by its status field. */
  board(id: string, spec: BoardSpec): void;
};

/**
 * Mark the start of a phase: a short, human-readable name for the group of steps that
 * follow, shown as one node on the workflow graph the user reads and approves.
 * Presentation only — it starts nothing, waits for nothing, returns nothing.
 *
 * Required in every script you submit, not optional: the phase graph is how the user
 * experiences the workflow. Without markers they face one card per step and no story;
 * group the whole script, top to bottom.
 *
 * Name phases for the user, in the language the user is speaking in this session: a short
 * natural phrase saying what the stage accomplishes ("Research each changed file in parallel",
 * "汇总并产出最终报告"). Graph-building vocabulary the user never chose — "fan-out", "gate",
 * "aggregate" — is not a name; the user approves stages by what they do. Say it the way you
 * would tell a colleague what is happening: "确认测试仍然通过", not "执行测试验证任务".
 *
 * The scope is the rest of the enclosing block: the marker claims every step issued
 * from it to the end of the block it stands in — nested blocks and inlined helper
 * calls included — and the enclosing phase resumes once that block ends. A marker
 * inside an if-branch therefore groups that branch and does not leak past it. Two
 * markers with the same name are one phase: repeating a name continues that phase,
 * which is the opposite of an actor's name — that one has to be unique.
 *
 * Two rules the compiler enforces. The name must be a compile-time string literal
 * ("review the diff" or a no-substitution template) and non-empty, because the phase
 * names label the graph the user confirms before anything runs. And the call must
 * stand alone as its own statement: a marker in expression position has no
 * rest-of-block to claim.
 *
 * Every phase must contain at least one subagent ask or one world.run. A phase is a
 * stage the user watches progress through; plain script logic between two asks (reading
 * args, shaping a prompt, building the return) runs in a flash and shows no progress, so
 * it is not a stage. Fold it into the phase before or after it; never open a phase for
 * the setup at the top or the return at the bottom.
 *
 * Idiom: one marker at the head of each stage that does work — name the loop body and
 * its check where they start, name the close-out that asks or runs after the loop.
 */
declare function phase(name: string): void;

/** One matching line found by files.grep. */
declare interface GrepMatch {
  /** Workspace-relative path of the file the match was found in. */
  path: string;
  /** One-based line number of the match. */
  line: number;
  /** The full text of the matching line. */
  text: string;
}

/**
 * Journaled read-only observations of the workspace, executed by the harness.
 * Replay returns the journal-recorded value. Prefer passing paths to agents and
 * letting them read files with their own tools; read() and grep() are for when the
 * script itself must shard or branch on content. There is no write — writing to the
 * world is an agent task.
 */
declare const files: {
  /**
   * List workspace files matching a glob pattern, as workspace-relative paths sorted
   * lexicographically. Capped at 2000 files: over the cap the call rejects instead of
   * returning a partial view — narrow the pattern.
   */
  glob(pattern: string): Promise<string[]>;
  /** Read one workspace file as UTF-8 text. Size-capped. */
  read(path: string): Promise<string>;
  /**
   * Search file contents with a ripgrep-compatible regular expression, optionally
   * narrowed to a glob over paths (the same syntax glob() takes: "*.ts", "src/**").
   * Returns one entry per matching line, with workspace-relative paths and one-based
   * line numbers.
   *
   * Capped at 2000 matches or 256KB of results, whichever comes first. Over the cap
   * the call rejects instead of returning a partial view — a silently truncated search
   * is the one result you cannot reason about — so narrow the pattern or add a glob.
   */
  grep(pattern: string, glob?: string): Promise<GrepMatch[]>;
};

/** The working tree's status, as reported by git.status(). */
declare interface GitStatus {
  /** Current branch name; absent when HEAD is detached. */
  branch?: string;
  /** True when nothing is staged, modified, or untracked. */
  clean: boolean;
  /** Workspace-relative paths staged for the next commit. */
  staged: string[];
  /** Workspace-relative paths modified in the working tree but not staged. */
  unstaged: string[];
  /** Workspace-relative paths git does not track (honouring .gitignore). */
  untracked: string[];
}

/** One commit, as reported by git.log(). */
declare interface GitCommit {
  /** Full commit hash. */
  hash: string;
  /** First line of the commit message. */
  subject: string;
  /** Author name. */
  author: string;
  /** Author date, ISO 8601. */
  date: string;
}

/**
 * Journaled read-only git observations — the same bargain as files.*: executed by the
 * harness, recorded in the journal, and replayed from the record, so a resumed run
 * sees the repository as it was rather than as it is now.
 *
 * Read-only by construction rather than by permission: the harness builds a fixed
 * argument list for one allowlisted subcommand and never a shell string, so there is
 * no call this surface can express that writes. A base must name a single ref — no
 * ".." ranges in this version — and paths are workspace-relative.
 *
 * Observations are scoped to the workspace, which is the same world files.* observes:
 * every path you get back is relative to the workspace and safe to pass straight to
 * files.read(). If the workspace is a subdirectory of the repository, changes outside
 * it are not reported — the workspace is the world. git.log is the exception, because
 * commits are repository-wide objects rather than paths.
 *
 * Caps reject rather than truncate (diff at 512KB, log at 100 commits), for the same
 * reason grep does. Outside a git repository, or with no git available, every call
 * rejects with a catchable error, so the idiom is try/catch with a files.glob fallback.
 */
declare const git: {
  /**
   * Workspace-relative paths that changed. With no base: files modified against HEAD
   * plus untracked files, because a brand-new file is a change to anyone reading. With
   * a base ref: files differing from that ref, tracked history only.
   */
  changedFiles(base?: string): Promise<string[]>;
  /**
   * Unified diff against base (default HEAD). Covers the whole workspace unless you
   * narrow it to one workspace-relative path.
   */
  diff(base?: string, path?: string): Promise<string>;
  /** The current working-tree status, for the workspace. */
  status(): Promise<GitStatus>;
  /**
   * The most recent commits, newest first. Default 20, maximum 100. Unlike the other
   * members this reads repository-wide history, not workspace paths.
   */
  log(count?: number): Promise<GitCommit[]>;
};

/** The outcome of one world.run command, including nonzero exits. */
declare interface WorldRunResult {
  /** The process exit code. Nonzero is a normal, returned outcome — branch on it. */
  exitCode: number;
  /** Captured stdout (UTF-8). Capped at 256KB; over the cap the call rejects. */
  stdout: string;
  /** Captured stderr (UTF-8). Same cap and rejection semantics as stdout. */
  stderr: string;
}

/**
 * Journaled command execution — the effect primitive. Executed by the harness exactly
 * once per call site and iteration, recorded in the journal, and replayed from the
 * record on resume (resume is crash recovery, not re-verification).
 *
 * Deliberately unlike git.*: a completed process with a NONZERO exit code RESOLVES to
 * a WorldRunResult — a failing check is the gating loop's normal case and must not
 * travel exception control flow. The promise only rejects (catchably) when the
 * command could not run as an observation at all: spawn failure, or timeout (default
 * 300000ms, override per call via timeoutMs, no upper cap).
 *
 * cmd must be a compile-time string literal: the script's command set is shown to the
 * user when the run is confirmed, and only those commands are executable. Fixed argv,
 * never a shell — no pipes, no redirection, no variable expansion; compose with
 * multiple calls and plain code. cwd is the workspace. Idiom: model generates, code
 * gates — run the check here, parse its output with pure script logic, and hand
 * failures to an agent to fix. A helper that needs Node builtins can be inlined as
 * world.run("node", ["-e", code]) — the code string lives inside the script, so it is
 * pinned by the journal key like every other argument.
 */
declare const world: {
  run(cmd: string, args?: string[], opts?: { timeoutMs?: number }): Promise<WorldRunResult>;
};
```
<!-- facade-dts:end -->

### 16.3 Rules the compiler and the analyzer enforce

- Plain TypeScript. Define result types with a plain `interface Foo { ... }` or
  `type Foo = ...` and pass them as `ask<T>` type arguments.
- Compiled under `strict` with `noUncheckedIndexedAccess` off: indexing an array or record
  (`items[i]`) needs no guard. `.find()`, `.match()`, `Map.get()` and optional properties
  still yield `T | undefined` / `null` and must be guarded before use.
- Never use the `declare` modifier: the script is compiled inside a function body, where
  ambient declarations are illegal. No `export` statements — the workflow's output is its
  final `return`. No `import` statements.
- Top-level `await` and a final `return <value>` are allowed; the returned value is exactly
  what the completion notification carries.
- No Node or web APIs: `process`, `fetch`, `fs` do not exist and fail typechecking.
- `world.run` executes a real command. Its first argument must be a compile-time string
  literal — the script's command set is shown to the user at confirmation — so interpolate
  runtime values into the args array, never into the command name. A nonzero exit code
  comes back as a value (`{ exitCode, stdout, stderr }`), not an exception: branch on
  `exitCode` for gate checks. Default timeout 300s; override per call with `timeoutMs` (no
  cap). Spawn failures and timeouts reject; stdout or stderr over 256KB rejects like any
  other over-cap world read.
- Phases are required (§8): cover the whole script with `phase("...")` markers, one at the
  head of each stage. The name is a compile-time string literal and the call is a standalone
  statement; the marker claims the rest of its enclosing block, nested blocks and inlined
  helper calls included, so a marker inside an `if` covers that branch only. Two markers
  with the same name are one phase, which is how a retry loop stays two nodes instead of
  per-round sprawl. Every phase contains at least one subagent `ask` or one `world.run`;
  plain script logic between two asks is not a stage, so do not open a phase for the setup
  at the top or the `return` at the bottom.
- Subagent names: the name you pass to `agent("...")` is the card the user sees, so write
  it for the user in the session's language (§9). It is also an identity: unique within the
  run, and the key an amended run matches its cached results by. Keep names stable across
  revisions of the same script, and give duplicates in a loop their own computed names.
- Artifact ids and the `report` tag are compile-time literals; one id belongs to one
  artifact kind; at most one artifact carries `primary: true`; a preset artifact is declared
  at the top level, not inside a loop, callback or branch (§10). Publishing rejects
  catchably when the file is missing or too large.
- Model-side errors never reach the script. Rate limits, concurrency limits, overload,
  network errors, timeouts and unknown provider errors are retried by the runtime without
  limit while it adapts the fan-out to what the provider accepts; a deterministic one
  (expired sign-in, model not in the plan, quota cap, invalid request) stops the whole run as
  `stopped` so the user can fix the cause and resume it. So do not write retry loops or
  `try`/`catch` for provider errors. Reserve them for logic failures — a subagent result
  that failed validation, a gate that did not pass, a world read over its cap, an artifact
  publish whose source file is missing, or a `ContextLimit` (the ask was too large for the
  model's context even after compaction: split the work or send less).
- The final `return` is the model-facing result; artifacts are the user-facing deliverable.
  Never put the same content in both.
- On diagnostics, edit the file the result names and call the tool again with `path`; never
  paste the script a second time.

### 16.4 `AmendWorkflow`

Revises an existing run: it starts a new run that supersedes the old one and imports its
finished work as a cache, so only what you changed is paid for again. §13 has the cache
rules and the repair-while-running move. It works on any run of this project — completed,
errored, stopped or still running; a running predecessor is stopped and superseded in the
same call, so never `TaskStop` it first and never rewrite it from scratch with
`CreateWorkflow`. To continue a stopped run unchanged, use `ResumeWorkflowRun` instead.

**Fields — every field you omit keeps the predecessor's value.**

- `run_id` (required): from a `CreateWorkflow` or `AmendWorkflow` result, a notification,
  `GetWorkflowRun` or `ListWorkflowRuns`.
- `path` or `script`, never both. `path` is the usual form: the errored notification and
  `GetWorkflowRun` name the run's script file — edit it in place and pass the same path
  back, so a revision costs one `Edit` instead of a second copy of the whole script. A file
  whose bytes you did not change is refused (`script_unchanged`) unless the call also
  changes `max_concurrency` or `subagent_model`; nothing is stopped and nothing is created.
  `script` carries a whole revised script inline, written against the same facade and rules;
  it is saved to a draft file the result names, so the next revision can go back to `path`.
  Omit both to keep the predecessor's script byte for byte and change only the settings
  below; its finished work still replays from the cache.
- `name`: a new display label; defaults to the predecessor's.
- `max_concurrency`: omit to keep the predecessor's limit, pass `null` to remove it, pass a
  number to change it (only when the user asks). A call carrying nothing but `run_id` and
  `max_concurrency`, against a run that is still going, retunes that run in place instead of
  starting a new one: same run, nothing stopped, nothing re-run.
- `subagent_model`: omit to keep the predecessor's choice, pass `null` to put the subagents
  back on the session model, pass a model id to change it (only when the user asks).
  `ListModels` lists the ids.

Changing only a setting on a running run still stops it; what it had in flight runs again in
the new run, except that a named subagent whose unfinished ask you did not change picks that
ask up where it left off instead of starting it over.

**How the cache works.** Named subagents are matched by name across the two scripts; each
one's asks are matched in order by byte-identical instructions. A match settles from the
recorded result at zero tokens; a changed or added ask runs live. The cache stays open until a
live subagent makes its first write to the workspace (or a live `world.run` executes): from
that moment, cached world reads and cached asks whose subagent had read or run something
would describe a workspace that no longer exists, so they run live too. Asks that only
answered keep settling from the cache. Editing an ask's text is how you force it to run
again; keep names stable and keep tunable constants out of ask text (§13).

An inline `script` that does not compile comes back as diagnostics: fix the file the result
names and call again with `path` — nothing was stopped and nothing was started.

**Confirmation.** A run this session started (through `CreateWorkflow`, a previous
`AmendWorkflow` or the workflows hub) is amended without a confirmation window, even while
it is running. A run the user stopped, or a run another session started, asks the user
first. The result names the run that was superseded (if one was stopped) and the new run's
id. The superseded run sends no notification of its own; the new run notifies when it
settles. Do not wait for it or poll it with `TaskOutput`.

### 16.5 `SaveWorkflow`

Saves a script with its metadata so it can be run again by name — through `CreateWorkflow`'s
`saved` source, discovered with `ListSavedWorkflows`. Project definitions go in
`.zcode/workflows/<name>.dwf.ts`, committed with the repository and visible only inside it;
global definitions go in `~/.zcode/workflows/<name>.dwf.ts` and are available from every
project on this machine.

**When to call it.**

- Never unsolicited. Saving writes a file into the user's repository; that is their
  decision, not yours.
- When a workflow you just built looks reusable — it would plausibly run again with
  different inputs — suggest saving it in prose, one sentence naming what you would save and
  why, then stop and wait. Call `SaveWorkflow` only after the user agrees, or when the user
  asks directly ("save this workflow", "保存这个工作流"). A one-off script tailored to a
  single question is not worth suggesting; it wastes the user's attention and clutters the
  project.

**Fields.**

- `name`: a file-safe identifier (letters, digits, dot, dash and underscore). Saving over an
  existing name replaces that workflow; the confirmation tells the user whether this is a
  new file or an overwrite, so reuse a name to update a workflow and pick a new one to add a
  variant.
- `description` (required) and `whenToUse`: written for a reader who has not seen this
  conversation. `description` shows up wherever the workflow is listed.
- `scope` (required, no default): `project` when the script references this repository's
  files, commands, conventions or layout; `global` when it depends on nothing in the
  project.
- `script` or `script_path`, never both. `script_path` saves a draft a `CreateWorkflow` or
  `AmendWorkflow` result named without re-emitting it; a `/* zcode-workflow` block in that
  file is dropped, because the metadata comes from this call's fields. `script` is the body
  only — no metadata block.
- `args`: name exactly the values that would change between runs — a PR number, a
  directory, a depth — and no more; every declared argument is one more thing a future
  caller has to get right. Each declaration gives a `type` (`string`, `number`, `boolean`,
  or `json` for anything else), and optionally a `description`, `required: true` and a
  `default`. Declared arguments are the workflow's calling convention; every future call is
  validated against them.

The saved file is valid TypeScript: a `/* zcode-workflow` block of YAML metadata, then the
script verbatim. The script is typechecked by the same compiler as `CreateWorkflow`, before
the user is asked; diagnostics mean nothing was written. The rules in §16.3 apply, phases
included — future callers did not see this conversation, so the phase names are all they
get.

### 16.6 `EvalWorkflowSnippet`

Compiles and runs a small snippet synchronously against the same compiler, sandbox and
world-read execution path a real run uses, and returns the result in the tool call. Nothing
persists: no journal rows, no background task, no run. It is the test bench for authoring
(§6): before writing or revising a script, check a parser against real command output, see
what a glob actually returns (workspace-relative sorted paths, cap rejections), or exercise a
gate predicate on real repository state, instead of guessing. It is not for orchestration:
there is no `agent()` here.

- `code` or `path`, never both: the snippet inline, or a file holding it, read whole with no
  metadata handling.
- `timeoutMs`: the wall clock for the whole snippet in milliseconds; default 60000, at most
  600000.
- The snippet facade is the `args`, `log`, `files`/`git` and `world.run` parts of the block
  in §16.2; `agent()`, `report()`, `artifact.*` and the `phase` marker do not exist here and
  fail typechecking. The language rules are a script's: plain interfaces, top-level `await`, a
  final `return <value>`. The returned value is serialized into the result, so return a
  summary, not a dump; over 256KB fails the call. `log(...)` lines come back in order. A
  snippet that contains `world.run` asks the user for confirmation before running. On
  diagnostics nothing ran: fix the snippet and call again.
