import assert from "node:assert/strict";
import test from "node:test";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import {
  zcodeMcpListResultSchema,
  zcodePluginsListResultSchema,
  zcodePluginsOverviewResultSchema,
  zcodePluginsReferenceCatalogResultSchema,
  zcodePluginsConfigureResultSchema,
  zcodePluginsRestoreBuiltinResultSchema,
  zcodePluginsSetEnabledResultSchema,
  zcodePluginsUninstallResultSchema,
} from "@zcode/shared";
import { fixture, waitForFile, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

async function put(path: string, value: unknown) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, typeof value === "string" ? value : JSON.stringify(value));
}

/** inline 插件：skill、agent、hooks、stdio MCP（启动时写出收到的插件环境）。 */
async function demoPlugin(root: string) {
  const plugin = join(root, "plugins-src/demo");
  const envFile = join(root, "mcp-env.json");
  const script = join(root, "plugin-mcp.mjs");
  await put(
    script,
    `
import { createInterface } from "node:readline";
import { writeFileSync } from "node:fs";
writeFileSync(${JSON.stringify(envFile)}, JSON.stringify({id:process.env.ZCODE_PLUGIN_ID,root:process.env.CLAUDE_PLUGIN_ROOT}));
const reply=(m,result)=>process.stdout.write(JSON.stringify({jsonrpc:"2.0",id:m.id,result})+"\\n");
createInterface({input:process.stdin}).on("line",line=>{const m=JSON.parse(line);
 if(m.method==="initialize")reply(m,{protocolVersion:"2025-11-25",serverInfo:{name:"p",version:"1"},capabilities:{tools:{}}});
 else if(m.method==="server/discover")process.stdout.write(JSON.stringify({jsonrpc:"2.0",id:m.id,error:{code:-32601,message:"legacy"}})+"\\n");
 else if(m.method==="tools/list")reply(m,{tools:[]});});
`,
  );
  await put(join(plugin, ".zcode-plugin/plugin.json"), {
    name: "demo",
    version: "1.0.0",
    description: "Demo plugin",
    author: { name: "Z", url: "https://z.test" },
    userConfig: { token: { sensitive: true }, region: { default: "eu" } },
  });
  await put(
    join(plugin, "skills/review/SKILL.md"),
    "---\nname: review\ndescription: Review\n---\nBody",
  );
  await put(join(plugin, "agents/helper.md"), "---\nname: helper\ndescription: Helps\n---\nPrompt");
  await put(join(plugin, "hooks/hooks.json"), {
    hooks: {
      UserPromptSubmit: [
        {
          hooks: [
            {
              type: "command",
              command: 'printf %s "${CLAUDE_PLUGIN_ROOT}" > "${CLAUDE_PLUGIN_DATA}/hook-ran"',
            },
          ],
        },
      ],
    },
  });
  await put(join(plugin, ".mcp.json"), {
    mcpServers: { echo: { command: process.execPath, args: [script] } },
  });
  return { plugin, envFile };
}

/** 插件存储：已安装的 cache 插件、市场快照与官方 bundled 分片。 */
async function storage(root: string) {
  const store = join(root, ".zcode/cli/plugins");
  const tool = join(store, "cache/market/tool/2.0.0");
  await put(join(tool, ".zcode-plugin/plugin.json"), { name: "tool", version: "2.0.0" });
  await put(join(store, "installed_plugins.json"), {
    version: 1,
    plugins: [
      {
        id: "tool@market",
        name: "tool",
        marketplace: "market",
        version: "2.0.0",
        installPath: tool,
        installedAt: "2026-09-01T00:00:00.000Z",
        scope: "user",
      },
    ],
  });
  await put(join(store, "known_marketplaces.json"), {
    version: 1,
    marketplaces: [
      {
        id: "market",
        name: "market",
        source: { source: "url", url: "https://market.test/m.json" },
        pluginCount: 1,
        addedAt: "2026-09-01T00:00:00.000Z",
      },
    ],
  });
  await put(join(store, "marketplaces/market/marketplace.json"), {
    name: "market",
    plugins: [
      {
        name: "tool",
        version: "2.1.0",
        description: "Tool plugin",
        displayName: "Tool",
        category: "dev",
        icon: "https://market.test/icon.png",
        skills: "skills",
      },
    ],
  });
  const official = join(store, "cache/zcode-plugins-official/skill-creator/0.1.0");
  await put(join(official, ".zcode-plugin/plugin.json"), { name: "skill-creator" });
  await put(join(store, "marketplaces/zcode-plugins-official/bundled-marketplace.json"), {
    version: 1,
    manifest: {
      name: "zcode-plugins-official",
      plugins: [{ name: "skill-creator", cachePath: official }],
    },
  });
  return store;
}

const workspace = (h: Harness, cwd: string) => ({ workspace: { workspacePath: cwd } });

test("Rust plugins/list, overview and referenceCatalog project every plugin source like Node", async () => {
  const f = await fixture({
    userConfig: {
      plugins: {
        enabledPlugins: { "tool@market": true },
        options: { "demo@inline": { token: "secret", region: "us" } },
      },
    },
  });
  try {
    const { plugin } = await demoPlugin(f.root);
    const store = await storage(f.root);
    await put(join(f.cwd, ".zcode/config.json"), {
      plugins: { dirs: [plugin], enabledPlugins: { "ghost@market": false } },
    });
    const h = f.start();
    const list = await h.client.request(
      "plugins/list",
      workspace(h, f.cwd),
      zcodePluginsListResultSchema,
    );
    const byId = new Map(list.plugins.map((p) => [p.id, p]));
    const demo = byId.get("demo@inline")!;
    assert.equal(demo.source, "inline");
    assert.equal(demo.enabled, true);
    assert.equal(demo.rootSource, "workspace");
    assert.equal(demo.enabledSource, undefined);
    assert.deepEqual(demo.optionSources, { token: "user", region: "user" });
    assert.deepEqual(demo.configuredOptions, { region: "us" }, "sensitive values stay hidden");
    assert.deepEqual(
      demo.components?.map((g) => g.kind),
      ["agent", "skill", "hook", "mcp"],
    );
    assert.equal(demo.skillCount, 1);
    assert.deepEqual(demo.mcpServerNames, ["plugin:demo:echo"]);
    assert.equal(demo.hookDetails?.[0]?.event, "UserPromptSubmit");
    assert.equal(demo.author, "Z");
    const tool = byId.get("tool@market")!;
    assert.deepEqual([tool.source, tool.enabled, tool.enabledSource], ["cache", true, "user"]);
    assert.equal(byId.get("skill-creator@zcode-plugins-official")?.enabled, true);
    const ghost = byId.get("ghost@market")!;
    assert.deepEqual(
      [ghost.source, ghost.packageStatus, ghost.enabledSource],
      ["missing", "missing", "workspace"],
    );

    const user = await h.client.request(
      "plugins/list",
      { ...workspace(h, f.cwd), configScope: "user" },
      zcodePluginsListResultSchema,
    );
    // User 视图不加载项目配置：inline 目录与项目声明都不在；用户 options 里的 demo 变为缺失条目。
    assert.deepEqual(
      user.plugins.map((p) => [p.id, p.source]),
      [
        ["skill-creator@zcode-plugins-official", "official"],
        ["tool@market", "cache"],
        ["demo@inline", "missing"],
      ],
    );

    const overview = await h.client.request(
      "plugins/overview",
      workspace(h, f.cwd),
      zcodePluginsOverviewResultSchema,
    );
    const known = JSON.parse(await readFile(join(store, "known_marketplaces.json"), "utf8"));
    assert(known.marketplaces.some((m: Message) => m.id === "zcode-plugins-official"));
    assert.deepEqual(
      overview.marketplaces.map((m) => [m.id, m.pluginCount, m.isOfficial]),
      [
        ["market", 1, false],
        ["zcode-plugins-official", 0, true],
      ],
    );
    const available = overview.availablePlugins.find((p) => p.id === "tool@market")!;
    assert.deepEqual([available.installed, available.componentTypes], [true, ["skill"]]);
    const installed = overview.installedPlugins.find((p) => p.id === "tool@market")!;
    assert.deepEqual(
      [installed.updateStatus, installed.latestVersion, installed.listing?.displayName],
      ["update-available", "2.1.0", "Tool"],
    );

    const catalog = await h.client.request(
      "plugins/referenceCatalogWithCategory",
      workspace(h, f.cwd),
      zcodePluginsReferenceCatalogResultSchema,
    );
    assert.equal(catalog.authority, "workspace");
    const entries = new Map(catalog.plugins.map((p) => [p.pluginId, p]));
    assert.deepEqual(
      [entries.get("tool@market")?.category, entries.get("tool@market")?.icon],
      ["dev", "https://market.test/icon.png"],
    );
    const demoEntry = entries.get("demo@inline")!;
    assert.equal(demoEntry.category, "other");
    assert.deepEqual(demoEntry.skillQualifiedNames, ["demo:review"]);
    assert.deepEqual(demoEntry.subagentNames, ["demo:helper"]);
    assert.equal("rootPath" in demoEntry, false, "paths never leave the runtime");
    await assert.rejects(
      h.client.request(
        "plugins/referenceCatalog",
        { ...workspace(h, f.cwd), sessionId: "missing-session" },
        zcodePluginsReferenceCatalogResultSchema,
      ),
      /Session unavailable/,
      "an unknown session never falls back to the workspace catalog",
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test(
  "Rust runtime injects enabled plugin MCP servers and hooks with the plugin identity",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture();
    try {
      const { plugin, envFile } = await demoPlugin(f.root);
      await put(join(f.cwd, ".zcode/config.json"), { plugins: { dirs: [plugin] } });
      const h = f.start();
      const mcp = await h.client.request(
        "mcp/list",
        { workspace: { workspacePath: f.cwd }, mode: "connect" },
        zcodeMcpListResultSchema,
      );
      assert.equal(mcp.statuses["plugin:demo:echo"]?.status, "connected");
      const env = JSON.parse(await waitForFile(envFile));
      assert.deepEqual(env, { id: "demo@inline", root: plugin });
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "hello" }));
      await h.completed(id);
      const data = join(f.root, ".zcode/cli/plugins/data/demo@inline");
      assert.equal(await readFile(join(data, "hook-ran"), "utf8"), plugin);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  },
);

test("Rust freezes the session plugin catalog and commits @plugin reference reminders like Node", async () => {
  const f = await fixture();
  try {
    const { plugin } = await demoPlugin(f.root);
    await put(join(f.cwd, ".zcode/config.json"), { plugins: { dirs: [plugin] } });
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const catalog = (sessionId?: string) =>
      h.client.request(
        "plugins/referenceCatalog",
        { ...workspace(h, f.cwd), ...(sessionId ? { sessionId } : {}) },
        zcodePluginsReferenceCatalogResultSchema,
      );
    const frozen = await catalog(id);
    assert.equal(frozen.authority, "session");
    assert.deepEqual(
      frozen.plugins.map((p) => p.pluginId),
      ["demo@inline"],
    );
    const text = "Use [Demo](plugin://demo@inline) and [ghost](plugin://ghost@m) please";
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text }));
    await h.completed(id, after);
    const messages = f.requests.at(-1)!.messages as Message[];
    const index = messages.findIndex((m) => m.role === "user" && m.content === text);
    const reminder = String(messages[index + 1]?.content);
    assert(reminder.startsWith("<system-reminder>\n<plugin_reference>"), reminder);
    assert(reminder.includes('- id: "demo@inline"'));
    assert(reminder.includes('skills: ["demo:review"]'));
    assert(!reminder.includes("ghost@m"), "unknown references are skipped");
    // 配置变化后 workspace 目录随之变化，会话目录保持冻结。
    await put(join(f.cwd, ".zcode/config.json"), { plugins: { dirs: [] } });
    assert.deepEqual((await catalog()).plugins, []);
    assert.deepEqual(
      (await catalog(id)).plugins.map((p) => p.pluginId),
      ["demo@inline"],
    );

    // 提醒作为 canonical 消息持久化：重启后下一轮请求的前缀不变。
    await h.close();
    const restarted = f.start();
    await restarted.subscribe(`conversation/${id}`);
    const next = restarted.messages.length;
    await restarted.command(restarted.envelope("sendText", id, { text: "again" }));
    await restarted.completed(id, next);
    const replayed = f.requests.at(-1)!.messages as Message[];
    const at = replayed.findIndex((m) => m.role === "user" && m.content === text);
    assert.equal(replayed[at + 1]?.content, reminder);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust plugin configuration writes, uninstall and built-in restore follow Node", async () => {
  const f = await fixture();
  try {
    const { plugin } = await demoPlugin(f.root);
    const store = await storage(f.root);
    const project = join(f.cwd, ".zcode/config.json");
    const userConfig = join(f.root, ".zcode/cli/config.json");
    await put(project, { plugins: { dirs: [plugin] } });
    const h = f.start();
    const request = <T>(
      method: `plugins/${"setEnabled" | "configure" | "resetConfig" | "uninstall" | "restoreBuiltin" | "overview"}`,
      params: Message,
      schema: any,
    ) => h.client.request(method, { ...workspace(h, f.cwd), ...params }, schema) as Promise<T>;
    const list = async () =>
      new Map(
        (
          await h.client.request("plugins/list", workspace(h, f.cwd), zcodePluginsListResultSchema)
        ).plugins.map((p) => [p.id, p]),
      );
    const readJson = async (path: string) => JSON.parse(await readFile(path, "utf8"));

    const disabled = await request<Message>(
      "plugins/setEnabled",
      { pluginId: "demo", enabled: false, scope: "workspace" },
      zcodePluginsSetEnabledResultSchema,
    );
    assert.deepEqual(
      [disabled.enabled, disabled.plugin.enabled, disabled.plugin.enabledSource],
      [false, false, "workspace"],
    );
    assert.equal((await readJson(project)).plugins.enabledPlugins["demo@inline"], false);
    assert.equal((await list()).get("demo@inline")?.enabledSource, "workspace");
    await assert.rejects(
      request(
        "plugins/setEnabled",
        { pluginId: "nope", enabled: true },
        zcodePluginsSetEnabledResultSchema,
      ),
      /Plugin not found: nope/,
    );

    await request(
      "plugins/configure",
      { pluginId: "demo@inline", options: { region: "ap", token: "t", bad: { x: 1 } } },
      zcodePluginsConfigureResultSchema,
    );
    await request(
      "plugins/configure",
      { pluginId: "demo@inline", options: {}, clearOptionKeys: ["region", " "] },
      zcodePluginsConfigureResultSchema,
    );
    assert.deepEqual((await readJson(userConfig)).plugins.options["demo@inline"], { token: "t" });
    const configured = (await list()).get("demo@inline")!;
    assert.equal(configured.configuredOptions, undefined, "only the sensitive token is stored");
    assert.deepEqual(configured.optionSources, { token: "user" });

    await request(
      "plugins/resetConfig",
      { pluginId: "demo@inline", scope: "workspace" },
      zcodePluginsConfigureResultSchema,
    );
    assert.equal((await readJson(project)).plugins.enabledPlugins["demo@inline"], undefined);
    assert.equal((await list()).get("demo@inline")?.enabled, true);

    const removed = await request<Message>(
      "plugins/uninstall",
      { pluginId: "tool@market" },
      zcodePluginsUninstallResultSchema,
    );
    assert.deepEqual(
      [removed.removedPlugin.id, removed.removedPlugin.enabled, removed.removedPlugin.version],
      ["tool@market", false, "2.0.0"],
    );
    assert.deepEqual((await readJson(join(store, "installed_plugins.json"))).plugins, []);
    await assert.rejects(
      readFile(join(store, "cache/market/tool/2.0.0/.zcode-plugin/plugin.json")),
    );
    assert.equal((await list()).has("tool@market"), false);

    const builtin = "skill-creator@zcode-plugins-official";
    await request("plugins/uninstall", { pluginId: builtin }, zcodePluginsUninstallResultSchema);
    assert.deepEqual((await readJson(userConfig)).plugins.suppressedBuiltins, [builtin]);
    assert.equal((await list()).has(builtin), false);
    const overview = await request<Message>(
      "plugins/overview",
      {},
      zcodePluginsOverviewResultSchema,
    );
    assert(overview.restorableBuiltins.some((p: Message) => p.id === builtin));
    await request(
      "plugins/restoreBuiltin",
      { pluginId: builtin },
      zcodePluginsRestoreBuiltinResultSchema,
    );
    assert.deepEqual((await readJson(userConfig)).plugins.suppressedBuiltins, []);
    assert.equal((await list()).has(builtin), true);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
