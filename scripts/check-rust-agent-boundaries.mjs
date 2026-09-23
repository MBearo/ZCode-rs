import { readdir, readFile } from "node:fs/promises";
import { resolve, relative } from "node:path";

const root = resolve(import.meta.dirname, "../apps/zcode-rust/src");
const failures = [];
async function walk(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      await walk(path);
      continue;
    }
    if (!path.endsWith(".rs")) continue;
    const source = await readFile(path, "utf8");
    const file = relative(root, path).replaceAll("\\", "/");
    const code = source.replace(/\/\/[^\n]*/g, "");
    if (source.split("\n").length > 400) failures.push(`${file}: exceeds 400 lines`);
    if (
      file.startsWith("app/") &&
      /\badapters\s*::|\b(?:tokio|std)\s*::\s*(?:fs|process|net)\b/.test(code)
    )
      failures.push(`${file}: app must use IO ports`);
    if (file.startsWith("adapters/") && /\bapp\s*::/.test(code))
      failures.push(`${file}: adapter imports app`);
    if (file.startsWith("domain/") && /\b(?:tokio|reqwest|rusqlite|adapters)\s*::/.test(code))
      failures.push(`${file}: domain imports runtime or IO`);
  }
}
await walk(root);
if (failures.length) {
  for (const failure of failures) console.error(failure);
  process.exitCode = 1;
} else console.log("Rust agent boundaries: OK");
