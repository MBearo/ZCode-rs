// rust-cli 与 Node CLI 的互操作检查：MCP 服务器绑定到远端机器（远端执行 R5，docs/specs/rust-cli/remote-exec.md §22.2；
// 由 node-interop-check.mjs 调用）。
//
// 绑定写在 rust-cli 自己的旁路文件 `~/.zcode/rust-cli/mcp-machines.json`，服务器照旧写在共用的配置文件里：Node 读配置
// 没有诊断、服务器照常可用。对照：服务器里直接写 `machine` 时 Node 的 strict 校验报 `config_mcp_server_invalid` 并跳过
// 那个服务器（这是不把 `machine` 写进共用配置文件的依据之一）。
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { importNode } from "./node-root.mjs";

export async function checkMcpMachines({ nodeRoot, expectEqual, home }) {
  const { loadFileConfig } = await importNode(nodeRoot, "adapters/src/config/file-config.adapter.ts");
  const file = join(home, ".zcode", "cli", "config.json");
  await mkdir(join(home, ".zcode", "cli"), { recursive: true });
  await mkdir(join(home, ".zcode", "rust-cli"), { recursive: true });
  const browser = { type: "stdio", command: "npx", args: ["@playwright/mcp"], env: { TOKEN: "${TOKEN}" } };
  const docs = { type: "http", url: "https://mcp.example.com/mcp" };
  await writeFile(file, `${JSON.stringify({ mcp: { servers: { "browser-a": browser, docs } } }, null, 2)}\n`);
  await writeFile(
    join(home, ".zcode", "rust-cli", "mcp-machines.json"),
    `${JSON.stringify({ version: 1, servers: { "browser-a": { machine: "dev-a" } } }, null, 2)}\n`,
  );
  const loaded = loadFileConfig(file);
  expectEqual("Node reads the config next to the bindings without diagnostics", loaded.diagnostics, []);
  expectEqual(
    "Node keeps both servers",
    Object.keys(loaded.config.mcp?.servers ?? {}).sort(),
    ["browser-a", "docs"],
  );
  expectEqual("Node keeps the server as written", loaded.config.mcp?.servers?.["browser-a"]?.command, "npx");

  // 对照：`machine` 直接写进服务器。
  await writeFile(
    file,
    `${JSON.stringify({ mcp: { servers: { "browser-a": { ...browser, machine: "dev-a" }, docs } } }, null, 2)}\n`,
  );
  const inline = loadFileConfig(file);
  expectEqual(
    "an inline machine field makes Node skip the server",
    inline.diagnostics.map((diagnostic) => [diagnostic.code, diagnostic.path]),
    [["config_mcp_server_invalid", "mcp.servers.browser-a"]],
  );
  expectEqual("Node keeps the other server", Object.keys(inline.config.mcp?.servers ?? {}), ["docs"]);
}
