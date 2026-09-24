import { readdir, readFile } from "node:fs/promises";
import { resolve, relative } from "node:path";

const root = resolve(import.meta.dirname, "../apps/zcode-cli-rust/crates");
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
    const crate = file.split("/", 1)[0];
    const code = source.replace(/\/\/[^\n]*/g, "");
    if (source.split("\n").length > 400) failures.push(`${file}: exceeds 400 lines`);
    if (
      crate === "domain" &&
      /\b(?:tokio|reqwest|rusqlite|zcode_cli_(?:state|model|tools|host))\s*::|\bstd\s*::\s*(?:fs|process|net)\b/.test(
        code,
      )
    )
      failures.push(`${file}: domain imports runtime or IO`);
    if (
      crate === "protocol" &&
      /\bzcode_cli_(?:core|core_api|state|model|tools|host)\s*::/.test(code)
    )
      failures.push(`${file}: protocol imports core or adapters`);
    if (
      crate === "core" &&
      /\bzcode_cli_(?:state|model|tools|host|app_server|headless|tui)\s*::/.test(code)
    )
      failures.push(`${file}: core imports an adapter or frontend`);
    // 前端只经传输契约（core-api/protocol/domain）与 runtime 交互，不能持有 core 内部状态。
    if (
      ["tui", "app-server", "headless"].includes(crate) &&
      /\bzcode_cli_(?:core|state|model|tools|host)\s*::/.test(code)
    )
      failures.push(`${file}: frontend imports core or an adapter`);
    // 网络出口是最底层基础设施：不依赖任何内部 crate，前端也不直接发起网络请求。
    if (crate === "net" && /\bzcode_cli_\w+\s*::/.test(code))
      failures.push(`${file}: net imports an internal crate`);
    if (["tui", "app-server", "headless"].includes(crate) && /\bzcode_cli_net\s*::/.test(code))
      failures.push(`${file}: frontend imports network egress`);
    if (
      ["state", "model", "tools", "host", "net", "plugins"].includes(crate) &&
      /\bzcode_cli_(?:core|app_server|headless|tui)\s*::/.test(code)
    )
      failures.push(`${file}: adapter imports core or frontend`);
  }
}
await walk(root);
if (failures.length) {
  for (const failure of failures) console.error(failure);
  process.exitCode = 1;
} else console.log("Rust workspace boundaries: OK");
