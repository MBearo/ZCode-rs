// Run with node --import tsx. Exports the zcode config file JSON Schema from the
// TS zod contract so the Rust config loader validates files exactly like Node:
// a file failing `ZCodeConfigFileSchema` is ignored as a whole, while each MCP
// server is validated separately (after legacy normalization) and dropped alone.
import { readFile, writeFile } from "node:fs/promises";
import { format } from "oxfmt";
import { z } from "zod";
import { ZCodeConfigFileSchema } from "../apps/zcode-cli/packages/adapters/src/config/schema.ts";

const toSchema = (schema) => z.toJSONSchema(schema, { io: "input", unrepresentable: "any" });
// mcp.servers 的值是 z.preprocess(normalize, discriminatedUnion)；导出管道右侧的 union 本体。
const mcpServer = ZCodeConfigFileSchema.shape.mcp.unwrap().shape.servers.unwrap().valueType.out;
const data = { file: toSchema(ZCodeConfigFileSchema), mcpServer: toSchema(mcpServer) };
const path = new URL("../apps/zcode-cli-rust/crates/domain/schema/config.json", import.meta.url);
const formatted = await format(path.pathname, `${JSON.stringify(data, null, 2)}\n`);
if (formatted.errors.length) throw new Error("Cannot format Rust config schema");
if (process.argv.includes("--check")) {
  if ((await readFile(path, "utf8")) !== formatted.code)
    throw new Error("Rust config schema differs from TS; regenerate it");
} else await writeFile(path, formatted.code);
