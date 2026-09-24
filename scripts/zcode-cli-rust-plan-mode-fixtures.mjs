// Plan-mode texts and reminder cadence for the Rust `domain::plan_mode`, taken
// from the TS implementations. Imported by generate-zcode-cli-rust-fixtures.mjs.
import {
  EnterPlanModeInputJsonSchema,
  ExitPlanModeInputJsonSchema,
  PLAN_MODE_MAX_PLAN_CHARS,
} from "../apps/zcode-cli/packages/contracts/src/tools/plan-mode.ts";
import {
  createEnterPlanModeProviderDescription,
  EXIT_PLAN_MODE_MODEL_INSTRUCTIONS,
} from "../apps/zcode-cli/packages/core/src/tool/handlers/plan-mode-prompts.ts";
import {
  enterPlanModeToolEntry,
  exitPlanModeToolEntry,
} from "../apps/zcode-cli/packages/core/src/tool/handlers/plan-mode.ts";
import {
  buildPlanModeExitReminderBody,
  buildRuntimeModeReminderBody,
} from "../apps/zcode-cli/packages/core/src/runtime/helpers/runtime-reminders.ts";

const reminder = { metadata: { source: "runtime_mode" }, message: { role: "user", content: "" } };
const human = { metadata: { source: "real_user" }, message: { role: "user", content: "hi" } };
const assistant = { message: { role: "assistant", content: "ok" } };
const synthetic = { metadata: { source: "todo_reminder" }, message: { role: "user", content: "" } };

/** `R` reminder, `U` real user, `A` assistant, `S` synthetic user. */
function entries(pattern) {
  const map = { R: reminder, U: human, A: assistant, S: synthetic };
  return [...pattern].map((c) => map[c]);
}

const full = buildRuntimeModeReminderBody([], "build", true);
const sparse = buildRuntimeModeReminderBody(entries("RUUUUU"), "build", true);

const CADENCE = [
  "",
  "UA",
  "RUA",
  "RUUUU",
  "RUUUUU",
  "RSSSSSUUUU",
  "RUUUUURUUUUURUUUUURUUUUU",
  "RUUUUURUUUUURUUUUURUUUUURUUUUU",
  "RUUUUURUUUUURUUUUURUUUUURUUUUURUUUUU",
  "AAAAARUUUUU",
];

export function planModeData() {
  return {
    maxPlanChars: PLAN_MODE_MAX_PLAN_CHARS,
    tools: {
      EnterPlanMode: {
        description: createEnterPlanModeProviderDescription({ embeddedSearchEnabled: false }),
        parameters: EnterPlanModeInputJsonSchema,
      },
      ExitPlanMode: {
        description: EXIT_PLAN_MODE_MODEL_INSTRUCTIONS[0],
        parameters: ExitPlanModeInputJsonSchema,
      },
    },
    texts: {
      enterResult: enterPlanModeToolEntry.formatModelContent({
        message:
          "Entered plan mode. You should now focus on exploring the codebase and designing an implementation approach.",
      }),
      exitApproved: exitPlanModeToolEntry.formatModelContent({ plan: "{plan}" }),
      exitEmpty: exitPlanModeToolEntry.formatModelContent({ plan: " " }),
      reminderFull: full,
      reminderSparse: sparse,
      reminderExit: buildPlanModeExitReminderBody(),
    },
    cadence: CADENCE.map((pattern) => {
      const body = buildRuntimeModeReminderBody(entries(pattern), "build", true);
      return [pattern, body === null ? null : body === full ? "full" : "sparse"];
    }),
  };
}
