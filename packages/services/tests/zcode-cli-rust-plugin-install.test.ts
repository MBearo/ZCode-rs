import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { createServer, type ServerResponse } from "node:http";
import { access, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import {
  zcodePluginsCancelOperationResultSchema,
  zcodePluginsDescribeResultSchema,
  zcodePluginsInstallResultSchema,
  zcodePluginsListResultSchema,
  zcodePluginsMarketplaceMutationResultSchema,
  zcodePluginsOverviewResultSchema,
  zcodePluginsSetEnabledResultSchema,
  zcodePluginsValidateResultSchema,
} from "@zcode/shared";
import { fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
type Method = `plugins/${
  | "marketplace/add"
  | "marketplace/remove"
  | "marketplace/update"
  | "install"
  | "update"
  | "validate"
  | "describe"
  | "overview"
  | "list"
  | "setEnabled"}`;

const schemas: Record<Method, any> = {
  "plugins/marketplace/add": zcodePluginsMarketplaceMutationResultSchema,
  "plugins/marketplace/remove": zcodePluginsMarketplaceMutationResultSchema,
  "plugins/marketplace/update": zcodePluginsMarketplaceMutationResultSchema,
  "plugins/install": zcodePluginsInstallResultSchema,
  "plugins/update": zcodePluginsInstallResultSchema,
  "plugins/validate": zcodePluginsValidateResultSchema,
  "plugins/describe": zcodePluginsDescribeResultSchema,
  "plugins/overview": zcodePluginsOverviewResultSchema,
  "plugins/list": zcodePluginsListResultSchema,
  "plugins/setEnabled": zcodePluginsSetEnabledResultSchema,
};

async function put(path: string, value: unknown) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, typeof value === "string" ? value : JSON.stringify(value));
}

const readJson = async (path: string) => JSON.parse(await readFile(path, "utf8"));
const exists = (path: string) =>
  access(path).then(
    () => true,
    () => false,
  );

function client(h: Harness, cwd: string) {
  return (method: Method, params: Message = {}): Promise<Message> =>
    h.client.request(
      method,
      { workspace: { workspacePath: cwd }, ...params },
      schemas[method],
    ) as Promise<Message>;
}

const hasGit = (() => {
  try {
    execFileSync("git", ["--version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

function git(cwd: string, ...args: string[]) {
  const env = {
    ...process.env,
    GIT_AUTHOR_NAME: "t",
    GIT_AUTHOR_EMAIL: "t@example.test",
    GIT_COMMITTER_NAME: "t",
    GIT_COMMITTER_EMAIL: "t@example.test",
  };
  return execFileSync("git", ["-c", "commit.gpgsign=false", ...args], { cwd, env })
    .toString()
    .trim();
}

async function commit(dir: string, files: Record<string, unknown>) {
  for (const [path, value] of Object.entries(files)) await put(join(dir, path), value);
  if (!(await exists(join(dir, ".git")))) git(dir, "init", "-q");
  git(dir, "add", ".");
  git(dir, "commit", "-qm", "change");
  return git(dir, "rev-parse", "HEAD");
}

/** 本地目录市场：alpha 依赖 beta，loose 没有 manifest（strict: false）。 */
async function localMarketplace(root: string) {
  const dir = join(root, "market-src");
  await put(join(dir, ".claude-plugin/marketplace.json"), {
    name: "local",
    description: "Local plugins",
    plugins: [
      { name: "alpha", source: "./plugins/alpha", dependencies: ["beta"] },
      { name: "beta", source: "./plugins/beta" },
      {
        name: "loose",
        source: "./plugins/loose",
        strict: false,
        version: "0.3.0",
        description: "Loose",
        icon: "https://example.test/i.png",
      },
    ],
  });
  await put(join(dir, "plugins/alpha/.zcode-plugin/plugin.json"), {
    name: "alpha",
    version: "1.0.0",
    author: { name: "A", url: "https://a.test" },
    homepage: "https://a.test/home",
  });
  await put(
    join(dir, "plugins/alpha/skills/s1/SKILL.md"),
    "---\nname: s1\ndescription: S1\n---\nx",
  );
  await put(join(dir, "plugins/beta/.claude-plugin/plugin.json"), { name: "beta" });
  await put(join(dir, "plugins/loose/skills/x/SKILL.md"), "---\nname: x\n---\ny");
  return dir;
}

test("Rust adds a directory marketplace and installs plugins with dependencies like Node", async () => {
  const f = await fixture();
  try {
    const source = await localMarketplace(f.root);
    const store = join(f.root, ".zcode/cli/plugins");
    const userConfig = join(f.root, ".zcode/cli/config.json");
    const h = f.start();
    const call = client(h, f.cwd);

    const dry = await call("plugins/marketplace/add", { source, dryRun: true });
    assert.deepEqual(dry.marketplace, {
      id: "dry-run",
      name: "dry-run",
      source: { source: "directory", path: source },
      pluginCount: 0,
      isOfficial: false,
    });
    assert.equal(await exists(join(store, "marketplaces/local")), false);

    const added = await call("plugins/marketplace/add", { source });
    assert.equal(added.marketplace.id, "local");
    assert.equal(added.marketplace.pluginCount, 3);
    assert.equal(added.marketplace.description, "Local plugins");
    assert.equal(added.marketplace.isOfficial, false);
    const known = await readJson(join(store, "known_marketplaces.json"));
    const record = known.marketplaces.find((m: Message) => m.id === "local");
    assert.match(record.cacheTransactionId, /^[0-9a-f-]{36}$/);
    assert.equal(
      (await readJson(join(store, "marketplaces/local/marketplace.json"))).name,
      "local",
    );
    assert(await exists(join(store, "marketplaces/local/plugins/alpha/.zcode-plugin/plugin.json")));
    const leftovers = (await readdir(join(store, "marketplaces"))).filter((n) => n.startsWith("."));
    assert.deepEqual(leftovers, [], "activation leaves no backup, marker or stage");

    const overview = await call("plugins/overview");
    const available = overview.availablePlugins.find((p: Message) => p.id === "alpha@local");
    assert.equal(available.installed, false);

    const described = await call("plugins/describe", { pluginName: "alpha", marketplace: "local" });
    assert.deepEqual(described.metadata, {
      author: "A",
      authorUrl: "https://a.test",
      homepage: "https://a.test/home",
      version: "1.0.0",
    });
    assert.deepEqual(
      described.components
        .find((g: Message) => g.kind === "skill")
        .items.map((i: Message) => i.name),
      ["s1"],
    );
    const validated = await call("plugins/validate", { pluginName: "alpha", marketplace: "local" });
    assert.equal(validated.ok, true);
    assert.deepEqual(validated.compatibility.unsupported, [
      "mcpb",
      "dxt",
      "npm",
      "hostPattern",
      "pathPattern",
    ]);
    assert.equal((await call("plugins/validate", { source })).ok, true);

    const planned = await call("plugins/install", {
      pluginName: "alpha",
      marketplace: "local",
      dryRun: true,
    });
    assert.deepEqual(planned, { dependencyClosure: [], installedPlugins: [], diagnostics: [] });
    assert.equal(await exists(join(store, "installed_plugins.json")), false);

    const installed = await call("plugins/install", { pluginName: "alpha", marketplace: "local" });
    assert.deepEqual(installed.dependencyClosure, ["beta@local", "alpha@local"]);
    assert.deepEqual(
      installed.installedPlugins.map((p: Message) => [p.id, p.version, p.enabled, p.scope]),
      [
        ["beta@local", "0.0.0", true, "user"],
        ["alpha@local", "1.0.0", true, "user"],
      ],
    );
    assert.equal(installed.installedPlugins[1].installPath, join(store, "cache/local/alpha/1.0.0"));
    assert(await exists(join(store, "cache/local/alpha/1.0.0/skills/s1/SKILL.md")));
    const records = (await readJson(join(store, "installed_plugins.json"))).plugins;
    assert.deepEqual(
      records.map((r: Message) => r.id),
      ["beta@local", "alpha@local"],
    );
    assert.deepEqual(records[1].dependencies, ["beta"]);
    assert.equal(records[1].source, "./plugins/alpha");
    assert.deepEqual((await readJson(userConfig)).plugins.enabledPlugins, {
      "beta@local": true,
      "alpha@local": true,
    });

    const loose = await call("plugins/install", { pluginName: "loose", marketplace: "local" });
    assert.equal(loose.installedPlugins[0].version, "0.3.0");
    assert.deepEqual(
      await readJson(join(store, "cache/local/loose/0.3.0/.claude-plugin/plugin.json")),
      { name: "loose", version: "0.3.0", description: "Loose" },
    );

    const listed = await call("plugins/list");
    const alpha = listed.plugins.find((p: Message) => p.id === "alpha@local");
    assert.deepEqual([alpha.source, alpha.enabled, alpha.version], ["cache", true, "1.0.0"]);

    // 显式停用后再更新：重装不覆盖用户的显式选择。
    await call("plugins/setEnabled", { pluginId: "alpha@local", enabled: false });
    const updated = await call("plugins/update", { pluginId: "alpha@local" });
    assert.deepEqual(updated.dependencyClosure, ["beta@local", "alpha@local"]);
    assert.deepEqual(
      updated.installedPlugins.map((p: Message) => p.enabled),
      [true, false],
    );
    const again = (await readJson(join(store, "installed_plugins.json"))).plugins;
    assert.equal(again[1].installedAt, records[1].installedAt, "reinstall keeps installedAt");

    await call("plugins/marketplace/remove", { marketplace: "local" });
    const remaining = (await readJson(join(store, "known_marketplaces.json"))).marketplaces;
    assert.equal(
      remaining.some((m: Message) => m.id === "local"),
      false,
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust rolls back a failed dependency install and keeps the previous cache", async () => {
  const f = await fixture();
  try {
    const source = join(f.root, "broken-src");
    await put(join(source, "marketplace.json"), {
      name: "broken",
      plugins: [
        { name: "a", source: "./missing", dependencies: ["b"] },
        { name: "b", source: "./b" },
        { name: "c", source: { source: "npm", package: "c" } },
      ],
    });
    await put(join(source, "b/.zcode-plugin/plugin.json"), { name: "b", version: "1.0.0" });
    const store = join(f.root, ".zcode/cli/plugins");
    const h = f.start();
    const call = client(h, f.cwd);
    await call("plugins/marketplace/add", { source });
    await call("plugins/install", { pluginName: "b", marketplace: "broken" });
    const marker = join(store, "cache/broken/b/1.0.0/marker.txt");
    await put(marker, "previous generation");
    const before = await readFile(join(store, "installed_plugins.json"), "utf8");

    const failed = await call("plugins/install", { pluginName: "a", marketplace: "broken" });
    assert.deepEqual(failed.installedPlugins, []);
    assert.deepEqual(failed.diagnostics, [
      {
        code: "plugin_marketplace_invalid",
        message: "Unsupported or missing plugin source: ./missing",
        severity: "error",
        pluginId: "a@broken",
      },
    ]);
    assert.equal(await readFile(marker, "utf8"), "previous generation");
    assert.equal(await readFile(join(store, "installed_plugins.json"), "utf8"), before);
    const leftovers = (await readdir(join(store, "cache/broken/b"))).filter((n) =>
      n.startsWith("."),
    );
    assert.deepEqual(leftovers, []);

    const unsupported = await call("plugins/install", { pluginName: "c", marketplace: "broken" });
    assert.equal(unsupported.diagnostics[0].code, "plugin_marketplace_source_unsupported");
    const missing = await call("plugins/install", { pluginName: "zzz", marketplace: "broken" });
    assert.equal(missing.diagnostics[0].code, "plugin_dependency_missing");
  } finally {
    await f.close();
  }
});

test(
  "Rust materializes a declared git marketplace and installs a pinned git plugin",
  { skip: !hasGit },
  async () => {
    const repos = await mkdtemp(join(tmpdir(), "zcode-cli-rust-git-"));
    const pluginRepo = join(repos, "plugin-repo");
    const pinned = await commit(pluginRepo, {
      ".zcode-plugin/plugin.json": { name: "gp", version: "2.0.0" },
    });
    await commit(pluginRepo, { ".zcode-plugin/plugin.json": { name: "gp", version: "3.0.0" } });
    const marketRepo = join(repos, "market-repo");
    await commit(marketRepo, {
      "marketplace.json": {
        name: "gitmk",
        plugins: [
          {
            name: "gp",
            source: { source: "git", url: pathToFileURL(pluginRepo).href, sha: pinned },
          },
        ],
      },
    });
    const f = await fixture({
      userConfig: {
        plugins: {
          extraKnownMarketplaces: {
            gitmk: { source: { source: "git", url: pathToFileURL(marketRepo).href } },
          },
        },
      },
    });
    try {
      const h = f.start();
      const call = client(h, f.cwd);
      const installed = await call("plugins/install", { pluginName: "gp", marketplace: "gitmk" });
      assert.deepEqual(installed.diagnostics, []);
      assert.equal(installed.installedPlugins[0].version, "2.0.0");
      const refreshed = await call("plugins/marketplace/update", { marketplace: "gitmk" });
      assert.deepEqual(
        refreshed.marketplaces.map((m: Message) => [m.id, m.pluginCount]),
        [["gitmk", 1]],
      );
      assert.deepEqual(refreshed.diagnostics, []);
    } finally {
      await f.close();
      await rm(repos, { recursive: true, force: true });
    }
  },
);

test("Rust reports an unavailable system Git as plugin_git_unavailable", async () => {
  const f = await fixture({ env: { ZCODE_GIT_BINARY: "/nonexistent/zcode-git" } });
  try {
    const call = client(f.start(), f.cwd);
    const result = await call("plugins/validate", {
      source: "https://user:secret@example.test/r.git",
    });
    assert.equal(result.ok, false);
    assert.equal(result.diagnostics[0].code, "plugin_git_unavailable");
    assert.match(result.diagnostics[0].message, /plugin source https:\/\/example\.test\/r\.git,/);
    assert.doesNotMatch(result.diagnostics[0].message, /secret/);
  } finally {
    await f.close();
  }
});

async function marketplaceServer() {
  let failing = false;
  const held: ServerResponse[] = [];
  let arrived: () => void = () => {};
  const server = createServer((req, res) => {
    if (req.url === "/m.json") {
      res.writeHead(302, { Location: "/real.json" }).end();
    } else if (req.url === "/real.json" && !failing) {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ name: "web", plugins: [{ name: "w", source: "./w" }] }));
    } else if (req.url === "/slow.json") {
      held.push(res);
      arrived();
    } else {
      res.writeHead(500).end();
    }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as { port: number }).port;
  return {
    url: (path: string) => `http://127.0.0.1:${port}${path}`,
    fail: () => {
      failing = true;
    },
    arrival: () => new Promise<void>((resolve) => (arrived = resolve)),
    release: () => {
      for (const res of held.splice(0)) res.destroy();
    },
    close: async () => {
      for (const res of held) res.destroy();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}

test("Rust fetches URL marketplaces, records refresh failures and cancels operations", async () => {
  const web = await marketplaceServer();
  const f = await fixture({
    env: { NO_PROXY: "127.0.0.1,localhost", no_proxy: "127.0.0.1,localhost" },
  });
  try {
    const store = join(f.root, ".zcode/cli/plugins");
    const h = f.start();
    const call = client(h, f.cwd);
    const added = await call("plugins/marketplace/add", { source: web.url("/m.json") });
    assert.deepEqual(
      [added.marketplace.id, added.marketplace.pluginCount, added.marketplace.source],
      ["web", 1, { source: "url", url: web.url("/m.json") }],
    );
    web.fail();
    const refreshed = await call("plugins/marketplace/update", { marketplace: "web" });
    assert.deepEqual(refreshed.marketplaces, []);
    assert.deepEqual(refreshed.diagnostics, [
      {
        code: "plugin_marketplace_invalid",
        message: "Failed to fetch marketplace: 500 Internal Server Error",
        pluginId: "web",
        severity: "error",
      },
    ]);
    const overview = await call("plugins/overview");
    const summary = overview.marketplaces.find((m: Message) => m.id === "web");
    assert.equal(summary.refreshFailure.code, "plugin_marketplace_invalid");

    // 取消挂起的市场拉取：marketplaceAdd 以错误结束。
    let arrival = web.arrival();
    const pendingAdd = call("plugins/marketplace/add", {
      source: web.url("/slow.json"),
      operationId: " add-1 ",
    });
    await arrival;
    const cancel = (operationId: string) =>
      h.client.request(
        "plugins/cancelOperation",
        { operationId },
        zcodePluginsCancelOperationResultSchema,
      );
    assert.deepEqual(await cancel("add-1"), { operationId: "add-1", cancelled: true });
    await assert.rejects(pendingAdd, /This operation was aborted/);
    assert.deepEqual(await cancel("add-1"), { operationId: "add-1", cancelled: false });

    // 取消安装：安装以诊断返回，不写入刷新失败。
    const known = await readJson(join(store, "known_marketplaces.json"));
    known.marketplaces.push({
      id: "slowmk",
      name: "slowmk",
      source: { source: "url", url: web.url("/slow.json") },
      pluginCount: 0,
      addedAt: "2026-09-01T00:00:00.000Z",
    });
    await put(join(store, "known_marketplaces.json"), known);
    arrival = web.arrival();
    const pendingInstall = call("plugins/install", {
      pluginName: "x",
      marketplace: "slowmk",
      operationId: "install-1",
    });
    await arrival;
    assert.deepEqual(await cancel("install-1"), { operationId: "install-1", cancelled: true });
    const cancelled = await pendingInstall;
    assert.deepEqual(cancelled.diagnostics, [
      {
        code: "plugin_marketplace_invalid",
        message: "This operation was aborted",
        severity: "error",
        pluginId: "x@slowmk",
      },
    ]);
    const after = await readJson(join(store, "known_marketplaces.json"));
    const slow = after.marketplaces.find((m: Message) => m.id === "slowmk");
    assert.equal(slow.lastRefreshFailure, undefined);

    // 同一 operationId 重复登记：后登记的作业覆盖前者，前者不再可取消（Node Map.set）。
    web.release();
    arrival = web.arrival();
    const first = call("plugins/install", {
      pluginName: "x",
      marketplace: "slowmk",
      operationId: "dup",
    });
    await arrival;
    const second = call("plugins/install", {
      pluginName: "x",
      marketplace: "slowmk",
      operationId: "dup",
    });
    assert.deepEqual(await cancel("dup"), { operationId: "dup", cancelled: true });
    assert.deepEqual(await cancel("dup"), { operationId: "dup", cancelled: false });
    web.release();
    assert.equal((await first).diagnostics[0].message, "fetch failed");
    // 第二个作业在存储锁后才开始，入口的取消检查使其以取消诊断结束。
    assert.equal((await second).diagnostics[0].message, "Plugin operation cancelled");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
    await web.close();
  }
});
