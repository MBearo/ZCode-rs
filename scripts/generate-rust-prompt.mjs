// Build-time parity assets. Rust never starts Node to assemble a request.
import { readFile, writeFile } from "node:fs/promises";
import { buildCliPrefixSection } from "../apps/zcode-cli/packages/core/src/context/sections/cli-prefix.ts";
import { buildIdentitySection } from "../apps/zcode-cli/packages/core/src/context/sections/identity.ts";
import { buildDesktopContextSection } from "../apps/zcode-cli/packages/core/src/context/sections/desktop.ts";
import {
  buildDynamicBehaviorSection,
  buildContextManagementSection,
} from "../apps/zcode-cli/packages/core/src/context/dynamic-sections.ts";

import { formatIncomingMessage } from "../apps/zcode-cli/packages/core/src/system-reminder/incoming-message.ts";
import {
  formatGoalContinuationPrompt,
  formatGoalCompletionVerificationPrompt,
  formatGoalStateForModel,
} from "../apps/zcode-cli/packages/contracts/src/tools/target.ts";
const goal = {
  sessionID: "session",
  targetID: "target",
  objective: "{objective}",
  summaryTitle: null,
  status: "active",
  tokenBudget: null,
  tokensUsed: 0,
  timeUsedSeconds: 0,
  time: { created: 0, updated: 0 },
};

const content = `${JSON.stringify(
  {
    userSteer: formatIncomingMessage("{zcode_input_text}", "user_steer"),
    goalContinue: formatGoalContinuationPrompt(goal),
    goalVerify: formatGoalCompletionVerificationPrompt(goal),
    goalState: formatGoalStateForModel(goal),
    cli: buildCliPrefixSection().content,
    identity: buildIdentitySection().content,
    desktop: buildDesktopContextSection().content,
    behavior: buildDynamicBehaviorSection().content,
    contextManagement: buildContextManagementSection().content,
  },
  null,
  2,
)}\n`;
const target = new URL("../apps/zcode-rust/src/domain/prompt_templates.json", import.meta.url);
if (process.argv.includes("--check")) {
  if ((await readFile(target, "utf8")) !== content) {
    throw new Error(
      "Rust prompt differs from TS; run node --import tsx scripts/generate-rust-prompt.mjs",
    );
  }
} else {
  await writeFile(target, content);
}
