# Worked dynamic-workflow examples

Five complete scripts, end to end. Each is a whole arc — world read, topology, loop or
fan-out, salvage, artifacts, return shape — so read one when you want to see how the pieces
sit together rather than looking up a single shape (`patterns.md` is for that).

Every script here is submitted as the `script` argument of the `CreateWorkflow` tool. The
API surface they are written against is `SKILL.md` §16 (the facade block in §16.2 and the
rules in §16.3), not the tool description.

---

## 1. Adversarial implement ↔ verify loop

Two subagents with opposed jobs, looping until the verifier cannot break the implementation.
The shape generalizes to anything with an author and a skeptic: code and property test, proof
and counter-example, schema and fuzzer.

<!-- compile -->
```ts
interface ImplUpdate {
  /** Files this round changed. */
  changed: string[];
  /** How this version addresses the previous counter-example. */
  notes: string;
  /** Whether the author's own build and tests passed before handing off. */
  selfCheckPassed: boolean;
}

interface Verdict {
  /** True only when the property was actually verified, not merely left untested. */
  verified: boolean;
  /** Where the verification lives, when it succeeded. */
  proofPath?: string;
  /** Concrete inputs that break the implementation, when it failed. */
  counterExamples?: string[];
  /** Why. This is the input to the implementer's next round. */
  reason: string;
}

const IMPL_PATH = "src/compress.ts";
const TEST_PATH = "src/compress.property.test.ts";
const PROPERTY = "decompress(compress(s)) === s for every string s";

const implementer = agent("implementer", {
  system:
    `You implement in ${IMPL_PATH}. Run the build and \`npx vitest run\` before handing off, ` +
    "and say in your notes exactly which commands you ran. " +
    "Your deliverable is the file itself: describe what you changed, never paste the source back.",
});

const verifier = agent("verifier", {
  system:
    `You are an adversarial verifier. Read ${IMPL_PATH} and write property tests into ${TEST_PATH}. ` +
    "If you cannot break it, report verified. If you can, give the exact failing input. " +
    "Never claim a verification you did not actually run.",
});

const MAX_ROUNDS = 8;

// What a watcher wants to know mid-run: which round, and how many attacks are still open.
artifact.metrics("progress", {
  title: "Verification progress",
  metrics: [{ field: "round", label: "Round" }, { field: "open", label: "Open counter-examples" }],
});

phase("Implement the first version of the compression");
let impl = await implementer.ask<ImplUpdate>(
  `Implement compress and decompress in ${IMPL_PATH} so that ${PROPERTY}.`,
);
phase("Try to break it with property tests");
let verdict = await verifier.ask<Verdict>(
  `Verify or refute: ${PROPERTY}\n` +
    `Changed: ${impl.changed.join(", ")}\nAuthor's notes: ${impl.notes}\n` +
    `Author's self-check: ${impl.selfCheckPassed ? "passed" : "did not pass"}`,
);

let round = 1;
while (!verdict.verified && round < MAX_ROUNDS) {
  phase("Fix what the tests broke");
  round = round + 1;
  const open = verdict.counterExamples?.length ?? 0;
  log(`round ${round}: ${open} counter-examples`);
  report({ round, open, reason: verdict.reason, counterExamples: verdict.counterExamples ?? [] }, "progress");

  impl = await implementer.ask<ImplUpdate>(
    `Verification failed.\nReason: ${verdict.reason}\n` +
      `Counter-examples: ${JSON.stringify(verdict.counterExamples ?? [])}\n` +
      `Fix ${IMPL_PATH} directly; the failing tests are in ${TEST_PATH}. Re-run your own check.`,
  );
  phase("Re-verify the fixed implementation");
  verdict = await verifier.ask<Verdict>(
    `The implementation changed (${impl.notes}). Verify or refute ${PROPERTY} again.`,
  );
}

// The verifier's word is a claim; the test file is a fact. Run it once for real before the
// result is called verified — the verifier may have run it, but this is the run that counts.
phase("Run the property tests for real");
const proof = await world.run("npx", ["vitest", "run", TEST_PATH], { timeoutMs: 600_000 });
const verified = verdict.verified && proof.exitCode === 0;
if (verdict.verified && !verified) log(`the verifier said verified but vitest exited ${proof.exitCode}`);

await artifact.markdown(
  "report",
  [
    `# ${PROPERTY}`,
    "",
    verified
      ? `Verified after ${round} round(s); the property tests live in ${verdict.proofPath ?? TEST_PATH} and pass under vitest.`
      : verdict.verified
        ? `The verifier reported verified, but \`npx vitest run ${TEST_PATH}\` exited ${proof.exitCode}; treat the result as unverified.`
        : `Still refuted after ${round} rounds: ${verdict.reason}`,
    "",
    `Implementation: ${IMPL_PATH}. Last change: ${impl.notes}`,
  ].join("\n"),
  {
    title: "Verification report",
    description: "Whether the property holds, how many rounds it took, and where the tests live.",
    primary: true,
  },
);

return {
  conclusion: verified
    ? `${PROPERTY} holds; the verifier could not break the implementation after ${round} round(s), and the property tests pass under vitest.`
    : verdict.verified
      ? `${PROPERTY} is not verified: the verifier reported success but the property tests exit ${proof.exitCode} when run for real.`
      : `${PROPERTY} is still refuted after ${round} rounds: ${verdict.reason}`,
  verified,
  rounds: round,
  implPath: IMPL_PATH,
  proofPath: verdict.proofPath,
  finalVerdict: verdict,
  checks: [`npx vitest run ${TEST_PATH} exited ${proof.exitCode}`],
  notCovered: ["inputs the property tests did not generate", "performance"],
};
```

**Why it is written this way.** Both subagents live outside the loop, so each keeps everything
it learned: the implementer remembers the approaches it already tried, and the verifier
remembers which attacks already worked. Both genuinely edit and run files.

The subagents exchange **paths and descriptions, never file contents**; each reads the other's
work with its own tools. Each round is reported as it happens, so a run that dies on round
six still shows five rounds of counter-examples. And the return value says `verified` and
`rounds`, so a caller can tell convergence from exhaustion — and opens with a `conclusion`
and closes with `notCovered`, because the return value is what the main agent retells.

`verified` is not the verifier's word. The verifier is a subagent, and a subagent's "I ran
the tests" is a claim; the `world.run` at the end runs the test file for real and its exit
code is what the result rests on. The implementer's persona names the exact commands it
must run for the same reason — "run the tests" lets a subagent pick the fastest thing that
turns green.

Two artifacts, each for a different moment. The metrics tile is for whoever is watching the
loop: round and open counter-examples, fed by the `report` call the script was already
making. The markdown report is for afterwards — the same facts as the return, in the long
form the user keeps. The tests themselves are not published: they live in the repository,
and a card for a file the user would open in the editor anyway says nothing new.

---

## 2. Flaky test triage

Scan, then plan a fix under critique, then judge each finding independently. Three
topologies in one script, each chosen for a different reason.

<!-- compile -->
```ts
interface Finding {
  /** Test identifier as the runner reports it. */
  testId: string;
  /** Workspace-relative path of the test file. */
  file: string;
  /** What makes it flaky. */
  kind: "timing" | "ordering" | "external";
  /** 0-1. How confident the scan is that this is really flaky. */
  confidence: number;
}
interface Scan {
  findings: Finding[];
}
interface Plan {
  /** One entry per test, in the order the fixes should be applied. */
  steps: { testId: string; change: string }[];
  /** Anything the plan deliberately does not address. */
  outOfScope: string[];
}
interface Review {
  approved: boolean;
  /** What is wrong with the plan. Empty when approved. */
  feedback: string;
}
interface Judgement {
  /** False when the finding is a false positive. */
  real: boolean;
  reason: string;
}
interface Confirmation {
  /** True only when you reproduced the flakiness yourself (re-ran the test, read the timing dependence). */
  reproduced: boolean;
  /** What you did to check, one sentence. */
  note: string;
}
interface ReportedFinding extends Finding {
  /** "verified" when the confirmer reproduced it; "unconfirmed" when it could not. */
  status: "verified" | "unconfirmed";
}
interface FlakyReport {
  /** Two or three sentences answering what the user asked for. */
  conclusion: string;
  findings: ReportedFinding[];
  /** The approved plan, when the loop converged. */
  plan?: Plan;
  converged: boolean;
  rounds: number;
  /** What the run checked and how. */
  verified: string[];
  /** What the run did not look at or could not check, and why. */
  notCovered: string[];
}

// Every candidate is a card; it moves column as the run makes up its mind about it.
artifact.board("candidates", {
  title: "Flaky test candidates",
  key: "testId",
  status: "status",
  columns: ["candidate", "false positive", "verified", "unconfirmed"],
  detail: [{ field: "file" }, { field: "kind" }],
});

phase("Scan the repository for flaky tests");
const scan = await agent("scanner", "You identify flaky tests from test code and CI history.")
  .ask<Scan>("Find flaky tests in this repository. Prefer precision over recall.");

log(`${scan.findings.length} candidate flaky tests`);
for (const finding of scan.findings) report({ ...finding, status: "candidate" }, "candidates");
if (scan.findings.length === 0) {
  const empty: FlakyReport = {
    conclusion: "No flaky tests found.",
    findings: [],
    converged: true,
    rounds: 0,
    verified: ["scanned test code and CI history"],
    notCovered: ["tests that only flake under load the scan could not observe"],
  };
  return empty;
}

// Independent judges: one fresh context per finding, because a judge that has seen the
// other findings starts grading on a curve instead of on the merits. Each carries the
// test id in its name — one subagent per finding means one name per finding.
//
// A judge's verdict is a judgement, not a reproduction, so each survivor goes on to a fresh
// confirmer that re-runs the test and reads the timing dependence itself; the ask forbids
// edits. The confirmer is chained right after its own judge, inside the same callback: a
// candidate is confirmed the moment its judge rules, not after every judge has. What the
// confirmer cannot reproduce is kept and labelled, not dropped.
phase("Judge each candidate and confirm the real ones as they are judged");
const outcomes = await Promise.all(
  scan.findings.map(async (finding): Promise<ReportedFinding | undefined> => {
    const judgement = await agent(`judge-${finding.testId}`).ask<Judgement>(
      `Is this really flaky, or a false positive?\n${JSON.stringify(finding)}`,
    );
    if (!judgement.real) {
      report({ ...finding, status: "false positive" }, "candidates");
      return undefined;
    }
    const check = await agent(`confirmer-${finding.testId}`).ask<Confirmation>(
      `Reproduce this flakiness from the evidence alone: run the test, read the timing dependence. Do not edit any file.\n${JSON.stringify(finding)}`,
    );
    const reported: ReportedFinding = {
      ...finding,
      status: check.reproduced ? "verified" : "unconfirmed",
    };
    report(reported, "candidates");
    return reported;
  }),
);
const real = outcomes.filter((o): o is ReportedFinding => o !== undefined);
log(`${real.length} of ${scan.findings.length} survived judging`);
if (real.length === 0) {
  const empty: FlakyReport = {
    conclusion: "Every candidate was judged a false positive.",
    findings: [],
    converged: true,
    rounds: 0,
    verified: ["scanned test code and CI history", "judged each candidate independently"],
    notCovered: ["tests that only flake under load the scan could not observe"],
  };
  return empty;
}

// Planner and reviewer: created once, reused every round, so each accumulates context.
const planner = agent("planner", "You write minimal, concrete fixes for flaky tests.");
const reviewer = agent("reviewer", "You find the flaw in a fix plan. Approve only when you cannot find one. You never edit files.");

let feedback = "none";
let approved: Plan | undefined;
let rounds = 0;
for (let round = 0; round < 5; round++) {
  rounds = round + 1;
  phase("Draft a fix plan");
  const plan = await planner.ask<Plan>(
    `Plan fixes for these flaky tests:\n${JSON.stringify(real)}\nPrevious critique: ${feedback}`,
  );
  phase("Critique the plan");
  const review = await reviewer.ask<Review>(`Critique this plan:\n${JSON.stringify(plan)}`);
  if (review.approved) {
    approved = plan;
    break;
  }
  log(`plan rejected in round ${rounds}: ${review.feedback}`);
  feedback = review.feedback;
}

const verified = ["scanned test code and CI history", "each survivor reproduced by an independent confirmer"];
const done: FlakyReport = approved
  ? {
      conclusion: `${real.length} flaky tests confirmed; a fix plan was approved after ${rounds} round(s).`,
      findings: real,
      plan: approved,
      converged: true,
      rounds,
      verified,
      notCovered: approved.outOfScope,
    }
  : {
      conclusion: `${real.length} flaky tests confirmed, but no fix plan survived review in ${rounds} rounds; last critique: ${feedback}`,
      findings: real,
      converged: false,
      rounds,
      verified,
      notCovered: ["a fix plan — none was approved"],
    };

await artifact.markdown(
  "report",
  [
    `# ${done.conclusion}`,
    "",
    ...real.map((f) => `- ${f.testId} (${f.file}, ${f.kind}): ${f.status}`),
    "",
    approved
      ? approved.steps.map((step) => `1. ${step.testId}: ${step.change}`).join("\n")
      : `No plan was approved. Last critique: ${feedback}`,
  ].join("\n"),
  { title: "Flaky test report" },
);
return done;
```

**Why it is written this way.** The judges are fresh per finding and the planner/reviewer pair
is hoisted — opposite choices, both deliberate. Judging wants independence, so sharing a
context would actively corrupt it; planning wants memory, so a fresh planner each round would
waste it. The confirmers are a third kind: fresh and per finding, told not to edit, because a
judge's verdict is an opinion about a description, and only re-running the test turns it into
a fact. What they cannot reproduce stays in the report as `unconfirmed`. Each confirmer
follows its own judge inside one callback, so the fan-out has one join instead of two:
confirming candidate three never waits for candidate forty's judge.

The two early returns matter. A scan that finds nothing and a judging pass that rejects
everything are both *successes* with an empty result, not failures — and each avoids paying
for a planning loop over an empty list. Both still return the full report shape, with a
`conclusion` that says which of the two happened. Findings are reported before planning
starts, so even a planning loop that never converges hands back the confirmed findings.

The board is the watcher's view of a fan-out: every candidate appears the moment the scan
returns and moves column as a judge or a confirmer rules on it, because a later item with the
same `testId` replaces the card. The report is published once, after the planning loop, which
is why the loop `break`s to a single return instead of returning from inside. The two early
returns publish nothing: "No flaky tests found" is a one-line answer, and a page that
restates it would be noise.

---

## 3. Call-site migration

Discover, transform in parallel, then verify. The transforming subagents write files, so this is
the one shape where isolation and verification really matter.

<!-- compile -->
```ts
interface Migration {
  /** Whether this file needed changing at all. */
  changed: boolean;
  /** What was rewritten, one line. */
  summary: string;
  /** Anything the migrator could not do safely and left alone. */
  skipped: string[];
}
interface Check {
  /** True when the file compiles and its tests pass after the change. */
  clean: boolean;
  /** What broke, when it did not. */
  problem: string;
}

const OLD_API = "getUserSync";
const NEW_API = "getUser";

artifact.table("migration", {
  title: "Migration by file",
  key: "path",
  columns: [
    { field: "path", label: "File" },
    { field: "summary", label: "Change" },
    { field: "clean", label: "Verified" },
  ],
});

phase("Find every call site of the old API");
const hits = await files.grep(`\\b${OLD_API}\\b`, "src/**/*.ts");
const paths = [...new Set(hits.map((h) => h.path))];
log(`${OLD_API} appears in ${paths.length} files (${hits.length} call sites)`);
if (paths.length === 0) return { migrated: [], failed: [], note: "nothing to migrate" };

// One persona, reused by every migrator subagent. Personas are values you can share; subagents
// and their names are not — each file gets its own of both.
const MIGRATOR = {
  system:
    `You migrate call sites from ${OLD_API} to the async ${NEW_API}, adding await and making ` +
    "the enclosing function async where needed. Change nothing else. If a call site cannot be " +
    "migrated safely, leave it and say why.",
};

// One migrator per file: the edits are disjoint, so they can run at once, and a fresh
// context per file keeps one file's oddities from leaking into another's rewrite.
phase("Migrate each file and verify the result");
const results = await Promise.all(
  paths.map(async (path) => {
    try {
      const migration = await agent(`migrator-${path}`, MIGRATOR).ask<Migration>(
        `Migrate every ${OLD_API} call in ${path} to ${NEW_API}.`,
      );
      if (!migration.changed) return { path, migration, check: undefined };

      // Verify with a separate context: a subagent asked to check its own work grades
      // generously. It compiles and runs tests; the ask forbids edits.
      const check = await agent(`checker-${path}`).ask<Check>(
        `Does ${path} still compile and pass its tests after the ${NEW_API} migration? ` +
          `Run the checks; do not edit any file. The change was: ${migration.summary}`,
      );
      report({ path, summary: migration.summary, clean: check.clean }, "migration");
      return { path, migration, check };
    } catch (error) {
      log(`skipped ${path}: ${String(error)}`);
      report({ path, summary: `skipped: ${String(error)}`, clean: false }, "migration");
      return { path, migration: undefined, check: undefined, error: String(error) };
    }
  }),
);

const migrated = results.filter((r) => r.check?.clean === true).map((r) => r.path);
const failed = results.filter((r) => r.check?.clean === false || r.error !== undefined);

// Every file passed its own check; now the tree as a whole, once. A per-file checker cannot
// see the caller in another file that a migrated signature broke — the full suite can, and
// it is the check the user will run themselves.
phase("Run the whole test suite once over the migrated tree");
const suite = await world.run("npm", ["test"], { timeoutMs: 1_800_000 });
const suiteClean = suite.exitCode === 0;
if (!suiteClean) log(`npm test failed after the migration:\n${suite.stderr.slice(-2000)}`);

await artifact.markdown(
  "report",
  [
    suiteClean
      ? `# ${OLD_API} → ${NEW_API}: ${migrated.length} of ${paths.length} files migrated and verified`
      : `# ${OLD_API} → ${NEW_API}: ${migrated.length} of ${paths.length} files migrated, but the full suite fails`,
    "",
    ...migrated.map((p) => `- ${p}: migrated; compiled and tests passed`),
    ...failed.map((r) => `- ${r.path}: failed — ${r.check?.problem ?? r.error}`),
    "",
    `Full suite (\`npm test\`): exit ${suite.exitCode}`,
  ].join("\n"),
  { title: "Migration report" },
);

return {
  conclusion: suiteClean
    ? `${migrated.length} of ${paths.length} files migrated from ${OLD_API} to ${NEW_API} and verified; ${failed.length} failed; the full suite passes.`
    : `${migrated.length} of ${paths.length} files migrated from ${OLD_API} to ${NEW_API}, but the full suite fails after the migration, so the result is not verified: ${suite.stderr.slice(-300)}`,
  migrated,
  failed: failed.map((r) => ({ path: r.path, problem: r.check?.problem ?? r.error })),
  skipped: results.flatMap((r) => r.migration?.skipped ?? []),
  verified: [
    ...migrated.map((p) => `${p}: compiled and tests passed, checked by a separate subagent`),
    `npm test over the whole tree exited ${suite.exitCode}`,
  ],
  notCovered: ["call sites outside src/**/*.ts", "callers in other packages that import the migrated functions"],
};
```

**Why it is written this way.** `files.grep` narrows before the fan-out: globbing `src/**/*.ts`
and asking every file whether it uses the old API would spend a session per file to learn
what one search already knew. Because `grep` rejects instead of truncating when it overruns
its cap, the path list is either complete or absent — never quietly partial, which for a
migration is the difference between finished and silently half-finished.

Both subagents in the fan-out are named per path. That is not decoration: a run rejects two
subagents sharing a name, so `agent("migrator")` inside the `map` would not survive its second
file — and the per-path names are also what a corrected re-run (`AmendWorkflow`) matches its
imported cache against, so re-running this script after fixing one detail does not re-migrate
the files that already came out clean.

The checker is a **separate subagent** from the migrator, which is what makes the check
meaningful: a subagent asked to grade its own migration grades generously. It builds and
tests, and the ask is what forbids it to patch what it finds broken. The checker also runs
inside the same callback as its migrator — a per-item pipeline with one join — so the first
file is verified while the fortieth is still being rewritten, instead of every checker
waiting behind a `Promise.all` for the slowest migration. The
`try`/`catch` is per file, so one unmigratable file costs one file rather than the run. And
the return value separates `migrated` from `failed` from `skipped`, because "we changed 40
files" is not an outcome anyone can act on.

The fan-out verifies each file; the `world.run` after it verifies the tree. Those are
different checks: forty files that each pass their own tests can still not compile together,
and the caller in a file nobody migrated is exactly what a per-file checker never opens. The
whole-suite run is the check the user would run before merging, so it runs here, once, with
the timeout a real suite needs — and when it fails, `conclusion` says the result is
unverified rather than letting the forty green rows speak for the tree.

The table is the fan-out seen live: one row per file, keyed by path, filled in as each
checker answers, with the skipped files landing in it too. The report at the end is the
same list in prose for the user to keep. The migrated files themselves are not published —
they are the repository, and the user reads them there. The early return for "nothing to
migrate" publishes nothing: a one-line answer needs no page.

---

## 4. Prover loop gated by the real checker

One subagent doing open-ended repair, and a gate that cannot be talked past: `world.run`
executes the actual proof checker, and the loop advances on its exit code, not on anyone's
claim. Compare example 1, where the skeptic is another subagent — right when breaking the work
takes creativity. When a command can render the verdict, the command should.

<!-- compile -->
```ts
interface FixNotes {
  /** What changed this round, one line. */
  summary: string;
}

const TARGET = "Proofs/Main.lean";
const MAX_ROUNDS = 10;

const prover = agent("prover", {
  system:
    `You write and repair Lean proofs in ${TARGET}. Fix exactly what the checker reports; ` +
    "never delete or weaken a theorem to silence an error.",
});

phase("Write the first proof attempt");
let notes = await prover.ask<FixNotes>(`Prove the open theorem in ${TARGET}.`);

const attempts: string[] = [];
let clean = false;
let rounds = 0;
for (let round = 1; round <= MAX_ROUNDS; round++) {
  rounds = round;
  attempts.push(notes.summary);
  phase("Check the file with the fast checker");
  // The fast tier: one file, seconds. It exits 0 on a proof that still says `sorry` and
  // only warns on stderr, so the exit code alone is not the verdict.
  const check = await world.run("lake", ["env", "lean", TARGET], { timeoutMs: 600_000 });
  clean = check.exitCode === 0 && !check.stderr.includes("sorry");
  if (clean) {
    report({ round, summary: notes.summary, checkerClean: true });
    break;
  }

  log(`round ${round}: checker rejected`);
  report({ round, summary: notes.summary, checkerClean: false });
  phase("Repair what the checker rejected");
  notes = await prover.ask<FixNotes>(
    `The Lean checker rejected the file. Its output:\n${check.stderr}\nRepair ${TARGET}.`,
  );
}

// The strong tier, once: the whole project builds with the new proof in it. That is what
// the request was about, and it runs even though the fast tier already said yes.
let proved = false;
if (clean) {
  phase("Build the whole project with the new proof");
  const build = await world.run("lake", ["build"], { timeoutMs: 1_800_000 });
  proved = build.exitCode === 0;
  if (!proved) log(`lake build rejected what the fast checker accepted:\n${build.stderr}`);
}

await artifact.markdown(
  "report",
  [
    proved
      ? `# ${TARGET}: proved in round ${rounds}`
      : `# ${TARGET}: not proved after ${MAX_ROUNDS} rounds`,
    "",
    ...attempts.map((summary, index) => `${index + 1}. ${summary}`),
  ].join("\n"),
  { title: "Proof report" },
);

return proved
  ? {
      conclusion: `The open theorem in ${TARGET} is proved; the file checked clean in round ${rounds} and the whole project builds.`,
      proved: true,
      rounds,
      lastChange: notes.summary,
      verified: [`lake env lean ${TARGET} exited 0 with no sorry warning`, "lake build exited 0"],
      notCovered: [],
    }
  : clean
    ? {
        conclusion: `Not proved: ${TARGET} checks clean on its own, but lake build fails with the new proof in the project.`,
        proved: false,
        rounds,
        lastChange: notes.summary,
        verified: [`lake env lean ${TARGET} exited 0 with no sorry warning`, "lake build exited nonzero"],
        notCovered: ["why the whole-project build disagrees with the single-file check"],
      }
    : {
        conclusion: `Not proved: the Lean checker still rejects ${TARGET} after ${MAX_ROUNDS} rounds.`,
        proved: false,
        rounds,
        lastChange: notes.summary,
        verified: [`lake env lean ${TARGET} run ${MAX_ROUNDS} times, none clean`],
        notCovered: ["approaches the prover did not try", "lake build — never reached"],
      };
```

**Why it is written this way.** The prover is created once and keeps its context, so round
seven remembers what rounds one through six already tried. The checker is not a subagent at
all: `world.run` runs the real command, a nonzero exit comes back as a **value**, and the
loop's normal case is reading `exitCode` and handing `stderr` to the prover as the next
round's instructions — no `catch` anywhere, because rejections are reserved for the world
failing to answer (spawn failure, timeout, over-cap output).

There are two checks, and they are not interchangeable. `lake env lean` on one file is the
fast tier: it drives the rounds because it answers in seconds, and it reads stderr as well
as the exit code because a proof that still says `sorry` exits 0 with a warning. `lake
build` over the whole project is the strong tier — the check the request is really about —
and it runs once at the end whether or not the fast tier already passed. A loop gated on
the fast tier alone would return "proved" for a file that still contains `sorry`, and never
learn that the new proof breaks a module that imports it. Which two commands play these
roles is a fact about the repository, read from its README or build files before the script
is written, not a habit carried in from the last project.

The command name is a literal and the varying part — the target path — rides in the args
array; that is the rule, and it is also what the user sees and approves at confirmation.
The long `timeoutMs` is deliberate: proof checking is allowed to be slow, and the per-call
override exists precisely for real build-sized checks. Every round is reported as it lands,
so a run that dies on round eight still shows seven rounds of attempts. Before wiring a
different checker command, test what its stderr actually looks like with a one-line
`EvalWorkflowSnippet` call — the parse you write against a guess is the parse that breaks.

One artifact, deliberately. The report lists every attempt, which is more than the
`conclusion` can say, so it earns its page. There is no dashboard: the only state a watcher
could follow is the round number and whether the checker passed, and the phase timeline
lighting up "Repair what the checker rejected" again already shows exactly that. A metrics
tile would repeat it, and the proof file is the repository's to show, not a card's.

## 5. Changed-file review with confirmation as reviews land

Review every changed file, triage its findings on one scale, confirm each kept finding
independently, and hand back a report. Everything a file needs happens as soon as its own
review lands; the only join is the cross-file deduplication that needs every finding.

<!-- compile -->
```ts
interface Finding {
  /** Workspace-relative path, with a line when it applies: "src/a.ts:42". */
  where: string;
  /** One sentence: what is wrong. */
  what: string;
  /** What showed it: the lines read, or the command and the output that proved it. */
  evidence: string;
  /** How much it matters. Reserve "high" for data loss, a crash, or a wrong result. */
  severity: "low" | "medium" | "high";
}
interface Review {
  findings: Finding[];
}
interface Keep {
  /** True when this finding is worth putting in front of a human. */
  keep: boolean;
}
interface Confirmation {
  /** True only when you reproduced the problem yourself from the evidence. */
  reproduced: boolean;
  /** What you did to check, one sentence. */
  note: string;
}
interface ReportedFinding extends Finding {
  /** "verified" when the confirmer reproduced it; "unconfirmed" when it could not. */
  status: "verified" | "unconfirmed";
}
interface Digest {
  /** The confirmed findings with duplicates across files merged, one line each. */
  lines: string[];
  /** Two or three sentences a reader can act on. */
  summary: string;
}
interface WorkflowReport {
  /** Two or three sentences answering what the user asked for. */
  conclusion: string;
  findings: ReportedFinding[];
  /** What the run checked and how. */
  verified: string[];
  /** What the run did not look at or could not check, and why. */
  notCovered: string[];
}

phase("Review each changed file and confirm its findings as they land");
let paths: string[];
try {
  paths = await git.changedFiles("origin/main");
} catch {
  paths = await files.glob("src/**/*.ts");
}
log(`reviewing ${paths.length} changed files`);

// One triage subagent for all findings: severity only means anything if it is judged on a
// consistent scale. It is a queue, not a barrier — asks on it run FIFO, so a finding reaches
// it the moment its file's review lands, whichever file that is.
const triage = agent("triage", "You decide which review findings deserve a human's attention. Be strict.");

// One reviewer per file, fresh each: the files are unrelated, so nothing is gained by
// sharing a context and everything is gained by running them at once. Triage and
// confirmation live inside the same callback, so no file waits for the slowest review
// before its findings move on; the outer `Promise.all` is the only join the report waits on.
const perFile = await Promise.all(
  paths.map(async (p) => {
    const review = await agent(`reviewer-${p}`).ask<Review>(`Review ${p} for correctness bugs.`);

    const kept: Finding[] = [];
    for (const finding of review.findings) {
      const verdict = await triage.ask<Keep>(`Worth reporting? ${JSON.stringify(finding)}`);
      if (verdict.keep) kept.push(finding);
    }

    // A separate confirmer per kept finding, blind to the reviewer that raised it: it reads
    // the code, runs a check if one decides it, and is told not to fix anything. The name
    // carries the path and the index, because every subagent name in a run must be unique.
    return Promise.all(
      kept.map(async (finding, index) => {
        const check = await agent(`confirmer-${p}-${index}`).ask<Confirmation>(
          `Reproduce this finding from its evidence alone: read the code, run a check if one exists. Do not edit any file.\n${JSON.stringify(finding)}`,
        );
        const reported: ReportedFinding = {
          ...finding,
          status: check.reproduced ? "verified" : "unconfirmed",
        };
        report(reported); // published now, with its status, so it survives a later failure
        return reported;
      }),
    );
  }),
);
const confirmed = perFile.flat();
log(`${confirmed.length} findings confirmed or labelled`);

// The one stage that genuinely needs every finding: two reviewers can flag the same root
// cause from two files, and only a reader of the whole list can merge them.
phase("Merge duplicate findings across files and write them up");
const digest = await agent("editor", "You merge review findings that share a root cause and write them up for the engineer who fixes them.")
  .ask<Digest>(`Merge duplicates and write these up:\n${JSON.stringify(confirmed)}`);

const verifiedCount = confirmed.filter((f) => f.status === "verified").length;
await artifact.markdown(
  "report",
  [
    `# Review of ${paths.length} changed files: ${confirmed.length} findings, ${verifiedCount} reproduced`,
    "",
    digest.summary,
    "",
    ...digest.lines.map((line) => `- ${line}`),
    "",
    "## Every finding",
    ...confirmed.map((f) => `- **${f.where}** (${f.severity}, ${f.status}): ${f.what}\n  ${f.evidence}`),
  ].join("\n"),
  { title: "Review report" },
);
const result: WorkflowReport = {
  conclusion: digest.summary,
  findings: confirmed,
  verified: paths.map((p) => `reviewed ${p}; every kept finding re-checked by a separate subagent`),
  notCovered: ["files outside the change set", "runtime behaviour no existing test exercises"],
};
return result;
```

Three shapes in one script, each chosen for a reason. The reviewers are fresh and parallel
because the files are unrelated. The triage subagent is shared because severity must mean the
same thing twice — and sharing costs nothing here, because its FIFO queue is fed as reviews
land rather than after all of them. The confirmers are fresh, per finding and blind to the
reviewer, and they start the moment their file's triage is done: with twelve files and one
slow reviewer, eleven files' findings are confirmed and reported while it is still reading.
Nothing gets a second reader on top: each finding was confirmed, and the editor's job is
merging and writing up, not re-checking.
