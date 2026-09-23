// Run with node --import tsx. Exports the V4 command JSON Schema from the TS zod
// contract; the Rust protocol crate compares its typed payloads against it so a
// field added or renamed in TS fails the Rust test suite instead of drifting.
import { readFile, writeFile } from "node:fs/promises";
import { format } from "oxfmt";
import { z } from "zod";
import {
  COMMANDS_REQUIRING_BASE_REVISION,
  ROW_TARGETING_COMMANDS,
  commandEnvelopeSchema,
  commandPayloadSchemas,
} from "../packages/shared/src/zcode-protocol-v4/command.ts";

const toSchema = (schema) => z.toJSONSchema(schema, { io: "input", unrepresentable: "any" });
const data = {
  envelope: toSchema(commandEnvelopeSchema),
  payloads: Object.fromEntries(
    Object.keys(commandPayloadSchemas)
      .sort()
      .map((type) => [type, toSchema(commandPayloadSchemas[type])]),
  ),
  requiresBaseRevision: [...COMMANDS_REQUIRING_BASE_REVISION].sort(),
  requiresBaseLogEpoch: [...ROW_TARGETING_COMMANDS].sort(),
};
const path = new URL(
  "../apps/zcode-cli-rust/crates/protocol/schema/v4-command.json",
  import.meta.url,
);
const formatted = await format(path.pathname, `${JSON.stringify(data, null, 2)}\n`);
if (formatted.errors.length) throw new Error("Cannot format Rust protocol schema");
if (process.argv.includes("--check")) {
  if ((await readFile(path, "utf8")) !== formatted.code)
    throw new Error("Rust protocol schema differs from TS; regenerate it and update Rust types");
} else await writeFile(path, formatted.code);
