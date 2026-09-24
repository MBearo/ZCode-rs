// Run through generate-zcode-cli-rust-fixtures.mjs. Exports the strict Node
// schemas the cold V4 projection parses stored metadata with, in the JSON
// Schema subset `zcode_cli_protocol::json_schema` validates and strips.
// Spec rust-m11-node-storage §4.
import { z } from "zod";
import { zodToJsonSchema } from "zod-to-json-schema";
import {
  conversationInputIntentSchema,
  errorAttributionSchema,
  workflowLaunchMetaSchema,
  workflowNotificationMetaSchema,
} from "../packages/shared/src/zcode-protocol-v4/index.ts";
import { completedToolPartMetadataSchema } from "../apps/zcode-cli/packages/contracts/src/tools/tool-result-metadata.ts";

const v4 = (schema) => z.toJSONSchema(schema, { io: "input", unrepresentable: "any" });

// CLI 契约包仍用 zod v3；按 v4 `io: "input"` 的约定标注对象未知键：
// strip 不写 additionalProperties，strict 为 false，passthrough 为 {}。
const v3 = (schema) =>
  zodToJsonSchema(schema, {
    $refStrategy: "none",
    target: "jsonSchema2019-09",
    postProcess: (json, def) => {
      if (json && def?.typeName === "ZodObject" && def.catchall?._def.typeName === "ZodNever") {
        delete json.additionalProperties;
        if (def.unknownKeys === "strict") json.additionalProperties = false;
        if (def.unknownKeys === "passthrough") json.additionalProperties = {};
      }
      // draft-04 形式的布尔 exclusive* 归一为数值形式，与 v4 输出一致。
      for (const [flag, bound] of [
        ["exclusiveMinimum", "minimum"],
        ["exclusiveMaximum", "maximum"],
      ]) {
        if (json?.[flag] === true) {
          json[flag] = json[bound];
          delete json[bound];
        }
      }
      return json;
    },
  });

/** Node's strict schemas the cold projection validates with. */
export function coldProjectionSchemas() {
  return {
    conversationInputIntent: v4(conversationInputIntentSchema),
    errorAttribution: v4(errorAttributionSchema),
    workflowLaunchMeta: v4(workflowLaunchMetaSchema),
    workflowNotificationMeta: v4(workflowNotificationMetaSchema),
    completedToolPartMetadata: v3(completedToolPartMetadataSchema),
  };
}
