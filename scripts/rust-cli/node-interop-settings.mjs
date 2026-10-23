// rust-cli 与 Node CLI 的会话互操作检查：共用的配置文件（由 node-interop-check.mjs 调用）。
//
// 界面语言与主题（`~/.zcode/cli/config.json` 的 `ui.*`）、`/effort` 与 `/model` 的缺省选择（个人 Provider 配置）、
// `/mcp enable/disable`：两边互读，同一份输入写出的文本逐字节相同。
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { importNode } from "./node-root.mjs";

async function nodeConfig(nodeRoot) {
  return importNode(nodeRoot, "adapters/src/config/file-config.adapter.ts");
}

/**
 * 界面语言的配置文件（tui.md §7.1）：两边互读，其余字段与键序不变，同一份输入写出的文本逐字节相同。
 */
export async function checkUiLocale({ nodeRoot, uiLocale, expectEqual, home }) {
  const { loadFileConfig, updateUiLocaleInFileConfig } = await nodeConfig(nodeRoot);
  const file = join(home, ".zcode", "cli", "config.json");
  await mkdir(join(home, ".zcode", "cli"), { recursive: true });
  const original = {
    permission: { mode: "plan" },
    ui: { theme: "dark" },
    hooks: { enabled: false },
  };
  await writeFile(file, `${JSON.stringify(original, null, 2)}\n`);
  const written = uiLocale("write", home, "zh-CN");
  expectEqual("ui.locale file written by rust-cli", written.path, file);
  const rustText = await readFile(file, "utf8");
  const loaded = loadFileConfig(file);
  expectEqual("Node reads ui.locale written by rust-cli", loaded.config.ui?.locale, "zh-CN");
  expectEqual("Node keeps the other fields", loaded.config.permission?.mode, "plan");
  // 同一份输入交给 Node 写，文本应与 rust-cli 写出的逐字节相同。
  await writeFile(file, `${JSON.stringify(original, null, 2)}\n`);
  await updateUiLocaleInFileConfig(file, "zh-CN");
  expectEqual("the same input gives the same file text", await readFile(file, "utf8"), rustText);
  await updateUiLocaleInFileConfig(file, "auto");
  expectEqual("rust-cli reads ui.locale written by Node", uiLocale("read", home).locale, "auto");
  await updateUiLocaleInFileConfig(file, "en-US");
  expectEqual("rust-cli reads en-US written by Node", uiLocale("read", home).locale, "en-US");
}

/**
 * 主题的配置文件（tui-manage.md §2.1）：Node 没有改 `ui.theme` 的函数，Node 一侧用与 `patchUiLocale` +
 * `atomicWriteJson` 相同的写法（`{...parsed, ui: {...ui, theme}}`，两格缩进加换行）。
 */
export async function checkUiTheme({ nodeRoot, uiPrefs, expectEqual, home }) {
  const { loadFileConfig } = await nodeConfig(nodeRoot);
  const file = join(home, ".zcode", "cli", "config.json");
  await mkdir(join(home, ".zcode", "cli"), { recursive: true });
  const original = { permission: { mode: "plan" }, ui: { locale: "zh-CN" }, hooks: {} };
  const nodeWrite = async (theme) => {
    const parsed = JSON.parse(await readFile(file, "utf8"));
    const ui = typeof parsed.ui === "object" && parsed.ui !== null ? parsed.ui : {};
    await writeFile(file, `${JSON.stringify({ ...parsed, ui: { ...ui, theme } }, null, 2)}\n`);
  };
  await writeFile(file, `${JSON.stringify(original, null, 2)}\n`);
  expectEqual("ui.theme file written by rust-cli", uiPrefs("theme-write", home, "light").path, file);
  const rustText = await readFile(file, "utf8");
  const loaded = loadFileConfig(file);
  expectEqual("Node reads ui.theme written by rust-cli", loaded.config.ui?.theme, "light");
  expectEqual("Node keeps ui.locale next to ui.theme", loaded.config.ui?.locale, "zh-CN");
  await writeFile(file, `${JSON.stringify(original, null, 2)}\n`);
  await nodeWrite("light");
  expectEqual("the same ui.theme input gives the same file text", await readFile(file, "utf8"), rustText);
  await nodeWrite("dark");
  expectEqual("rust-cli reads ui.theme written by Node", uiPrefs("theme-read", home).theme, "dark");
}

/**
 * `/effort` 的缺省（tui-manage.md §2.2）：个人 Provider 配置的 `defaultModelSelection`。rust-cli 写的由 Node 的
 * Repository 读出；Node `saveConfiguredDefault` 写的由 rust-cli 读出；从同一份文件出发，两边写同一个选择得到
 * 相同的文本。
 */
export async function checkDefaultModel({ nodeRoot, uiPrefs, expectEqual, home }) {
  const { NodePersonalProviderConfigRepository, NodeModelSelectionConfigRepository } = await import(
    pathToFileURL(join(nodeRoot, "packages/provider-node/src/index.ts")).href
  );
  const file = join(home, ".zcode", "v2", "provider_config.json");
  const personal = new NodePersonalProviderConfigRepository({ filePath: file, pollingIntervalMs: false });
  const selections = new NodeModelSelectionConfigRepository({ personalRepository: personal });
  try {
    await selections.saveConfiguredDefault({ providerId: "zai", modelId: "GLM-5.3" });
    const start = await readFile(file, "utf8");
    const chosen = { providerId: "zai", modelId: "GLM-5.3", options: { reasoningLevel: "high" } };
    await selections.saveConfiguredDefault(chosen);
    const nodeText = await readFile(file, "utf8");
    expectEqual("rust-cli reads the default Node saved", uiPrefs("model-read", home), {
      provider: "zai",
      model: "GLM-5.3",
      reasoning: "high",
    });
    await writeFile(file, start);
    uiPrefs("model-write", home, "zai", "GLM-5.3", "high");
    expectEqual("the same default gives the same file text", await readFile(file, "utf8"), nodeText);
    uiPrefs("model-write", home, "zai", "GLM-5.3", "low");
    const fresh = new NodePersonalProviderConfigRepository({ filePath: file, pollingIntervalMs: false });
    expectEqual("Node reads the default rust-cli saved", (await fresh.read()).defaultModelSelection, {
      providerId: "zai",
      modelId: "GLM-5.3",
      options: { reasoningLevel: "low" },
    });
    fresh.dispose();
  } finally {
    selections.dispose();
    personal.dispose();
  }
}

/**
 * `/mcp enable/disable`（tui-manage.md §6.1）：rust-cli 写 `mcp.servers.<名字>.enabled`，Node 的 `loadFileConfig`
 * 读出同样的开关；旧字段 `enable: false` 被去掉（否则 Node 仍按停用）；与 Node 桌面端同样的写法逐字节相同。
 */
export async function checkMcpEnabled({ nodeRoot, uiPrefs, expectEqual, home }) {
  const { loadFileConfig } = await nodeConfig(nodeRoot);
  const file = join(home, ".zcode", "cli", "config.json");
  await mkdir(join(home, ".zcode", "cli"), { recursive: true });
  const original = {
    mcp: { servers: { docs: { command: "docs-server", enable: false }, other: { url: "https://mcp.example.com" } } },
    ui: { locale: "zh-CN" },
  };
  await writeFile(file, `${JSON.stringify(original, null, 2)}\n`);
  uiPrefs("mcp-write", home, "docs", "true");
  const rustText = await readFile(file, "utf8");
  const enabled = loadFileConfig(file).config.mcp?.servers;
  expectEqual("Node reads the server rust-cli enabled", enabled?.docs?.enabled, true);
  expectEqual("Node keeps the other server", enabled?.other?.url, "https://mcp.example.com");
  const { enable: _legacy, ...docs } = original.mcp.servers.docs;
  const nodeStyle = {
    ...original,
    mcp: { servers: { ...original.mcp.servers, docs: { ...docs, enabled: true } } },
  };
  expectEqual("the same enable gives the same file text", rustText, `${JSON.stringify(nodeStyle, null, 2)}\n`);
  uiPrefs("mcp-write", home, "other", "false");
  expectEqual(
    "Node reads the server rust-cli disabled",
    loadFileConfig(file).config.mcp?.servers?.other?.enabled,
    false,
  );
}
