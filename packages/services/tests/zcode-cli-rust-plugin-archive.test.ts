import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import { createServer, type ServerResponse } from "node:http";
import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { ZipFile } from "yazl";
import {
  zcodePluginsCancelOperationResultSchema,
  zcodePluginsInstallResultSchema,
  zcodePluginsMarketplaceMutationResultSchema,
  zcodePluginsResolveSuggestedReferenceResultSchema,
  zcodePluginsValidateResultSchema,
} from "@zcode/shared";
import { fixture } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
const OFFICIAL = "zcode-plugins-official";
const LOOPBACK_ENV = { NO_PROXY: "127.0.0.1,localhost", no_proxy: "127.0.0.1,localhost" };

async function put(path: string, value: unknown) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, typeof value === "string" ? value : JSON.stringify(value));
}

const exists = (path: string) =>
  access(path).then(
    () => true,
    () => false,
  );

/** 用 yazl 生成 ZIP；mode 为 0o120777 的条目是符号链接。 */
function zip(entries: Array<[string, string, number?]>): Promise<Buffer> {
  const file = new ZipFile();
  for (const [name, body, mode] of entries) {
    file.addBuffer(Buffer.from(body), name, mode ? { mode } : {});
  }
  file.end();
  const chunks: Buffer[] = [];
  return new Promise((resolve, reject) => {
    file.outputStream.on("data", (chunk: Buffer) => chunks.push(chunk));
    file.outputStream.on("end", () => resolve(Buffer.concat(chunks)));
    file.outputStream.on("error", reject);
  });
}

const sha256 = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");

async function server(routes: Map<string, (res: ServerResponse) => void>) {
  let arrived: () => void = () => {};
  const held: ServerResponse[] = [];
  const http = createServer((req, res) => {
    const route = routes.get(req.url ?? "");
    if (route) route(res);
    else if (req.url === "/hold") {
      held.push(res);
      arrived();
    } else res.writeHead(500).end();
  });
  await new Promise<void>((resolve) => http.listen(0, "127.0.0.1", resolve));
  const port = (http.address() as { port: number }).port;
  return {
    url: (path: string) => `http://127.0.0.1:${port}${path}`,
    arrival: () => new Promise<void>((resolve) => (arrived = resolve)),
    close: async () => {
      for (const res of held) res.destroy();
      await new Promise((resolve) => http.close(resolve));
    },
  };
}

const workspace = (cwd: string) => ({ workspace: { workspacePath: cwd } });

test("Rust installs verified ZIP plugin sources like Node", async () => {
  const good = await zip([
    ["zp-main/.zcode-plugin/plugin.json", '{"name":"zp","version":"1.2.0"}'],
  ]);
  const linked = await zip([
    ["root/.zcode-plugin/plugin.json", '{"name":"link"}'],
    ["root/evil", "/etc/passwd", 0o120777],
  ]);
  const routes = new Map<string, (res: ServerResponse) => void>([
    ["/zp.zip", (res) => res.writeHead(302, { Location: "/files/zp.zip" }).end()],
    ["/files/zp.zip", (res) => res.writeHead(200).end(good)],
    ["/link.zip", (res) => res.writeHead(200).end(linked)],
  ]);
  const web = await server(routes);
  const f = await fixture({ env: LOOPBACK_ENV });
  try {
    const zipSource = (path: string, sha: string) => ({
      source: "url",
      type: "zip",
      url: web.url(path),
      sha256: sha,
    });
    const source = join(f.root, "zip-market");
    await put(join(source, "marketplace.json"), {
      name: "zipmk",
      plugins: [
        { name: "zp", source: zipSource("/zp.zip", sha256(good).toUpperCase()) },
        { name: "wrongsha", source: zipSource("/zp.zip", "0".repeat(64)) },
        { name: "named", source: zipSource("/zp.zip", sha256(good)) },
        { name: "link", source: zipSource("/link.zip", sha256(linked)) },
        {
          name: "plain",
          source: { ...zipSource("/x", sha256(good)), url: "http://example.test/x.zip" },
        },
      ],
    });
    const h = f.start();
    const request = (method: "plugins/install" | "plugins/validate", params: Message) => {
      const payload = { ...workspace(f.cwd), ...params };
      return (
        method === "plugins/install"
          ? h.client.request(method, payload, zcodePluginsInstallResultSchema)
          : h.client.request(method, payload, zcodePluginsValidateResultSchema)
      ) as Promise<Message>;
    };
    await h.client.request(
      "plugins/marketplace/add",
      { ...workspace(f.cwd), source },
      zcodePluginsMarketplaceMutationResultSchema,
    );
    const installed = await request("plugins/install", { pluginName: "zp", marketplace: "zipmk" });
    assert.deepEqual(installed.diagnostics, []);
    assert.equal(installed.installedPlugins[0].version, "1.2.0");
    const store = join(f.root, ".zcode/cli/plugins");
    assert(await exists(join(store, "cache/zipmk/zp/1.2.0/.zcode-plugin/plugin.json")));

    const message = async (name: string) =>
      (await request("plugins/install", { pluginName: name, marketplace: "zipmk" })).diagnostics[0]
        .message as string;
    assert.match(await message("wrongsha"), /^Plugin zip sha256 mismatch: expected=0{64}, actual=/);
    assert.equal(
      await message("named"),
      "Plugin manifest name 'zp' does not match marketplace entry 'named'",
    );
    assert.equal(await message("link"), "Plugin zip entry symlinks are not supported: root/evil");
    assert.equal(
      await message("plain"),
      "Plugin zip source URL must be HTTPS: http://example.test/x.zip",
    );
    const validated = await request("plugins/validate", { pluginName: "zp", marketplace: "zipmk" });
    assert.equal(validated.ok, true);
    const deferred = await request("plugins/validate", { source });
    assert.equal(deferred.ok, true);
    assert(
      deferred.diagnostics.every((d: Message) => d.code === "plugin_validation_deferred"),
      "remote sources defer validation at the marketplace level",
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
    await web.close();
  }
});

test("Rust resolves suggested official plugin references like Node", async () => {
  let officialStatus = 200;
  const officialManifest = {
    name: OFFICIAL,
    plugins: [
      {
        name: "helper",
        description: "Helper",
        icon: " https://example.test/h.png ",
        source: "./helper",
      },
    ],
  };
  const routes = new Map<string, (res: ServerResponse) => void>([
    [
      "/official.json",
      (res) =>
        officialStatus === 200
          ? res.writeHead(200).end(JSON.stringify(officialManifest))
          : res.writeHead(officialStatus).end(),
    ],
  ]);
  const web = await server(routes);
  const f = await fixture({
    env: LOOPBACK_ENV,
    userConfig: { plugins: { enabledPlugins: { [`skill-creator@${OFFICIAL}`]: true } } },
  });
  try {
    const store = join(f.root, ".zcode/cli/plugins");
    const official = join(store, `cache/${OFFICIAL}/skill-creator/0.1.0`);
    await put(join(official, ".zcode-plugin/plugin.json"), { name: "skill-creator" });
    await put(join(official, "skills/make/SKILL.md"), "---\nname: make\n---\nbody");
    await put(join(store, `marketplaces/${OFFICIAL}/bundled-marketplace.json`), {
      version: 1,
      manifest: { name: OFFICIAL, plugins: [{ name: "skill-creator", cachePath: official }] },
    });
    const knownRecord = (url: string) => ({
      version: 1,
      marketplaces: [
        {
          id: OFFICIAL,
          name: OFFICIAL,
          source: { source: "url", url },
          pluginCount: 0,
          addedAt: "2026-09-01T00:00:00.000Z",
        },
      ],
    });
    await put(join(store, "known_marketplaces.json"), knownRecord(web.url("/official.json")));
    const h = f.start();
    const resolve = (stableId: string, operationId: string) =>
      h.client.request(
        "plugins/resolveSuggestedReference",
        {
          ...workspace(f.cwd),
          stableId,
          operationId,
          clientMode: "desktop-continuous",
          deliveryKind: "desktop-continuous",
        },
        zcodePluginsResolveSuggestedReferenceResultSchema,
      ) as Promise<Message>;
    const progress = () =>
      h.messages.filter((m) => m.method === "plugins/operationProgress").map((m) => m.params);

    const untrusted = await resolve("helper@other", "op-0");
    assert.equal(untrusted.status, "unavailable");
    assert.equal(untrusted.diagnostics[0].code, "plugin_suggested_reference_untrusted_source");

    const ready = await resolve(`skill-creator@${OFFICIAL}`, "op-1");
    assert.deepEqual(
      [ready.status, ready.pluginName, ready.sourceTrust, ready.diagnostics],
      ["ready", "skill-creator", "official", []],
    );
    assert.deepEqual(progress(), [], "a local hit does not refresh");

    const missing = await resolve(`helper@${OFFICIAL}`, "op-2");
    assert.deepEqual(
      [missing.status, missing.pluginName, missing.icon, missing.listing.icon],
      ["missing", "helper", "https://example.test/h.png", " https://example.test/h.png "],
    );
    assert.deepEqual(progress(), [{ operationId: "op-2", state: "refreshing" }]);
    const notified = h.messages.findIndex((m) => m.method === "plugins/operationProgress");
    const replied = h.messages.findIndex((m) => m.result?.status === "missing");
    assert(notified >= 0 && notified < replied, "progress is delivered before the reply");
    const merged = JSON.parse(
      await readFile(join(store, `marketplaces/${OFFICIAL}/marketplace.json`), "utf8"),
    );
    assert.deepEqual(
      merged.plugins.map((p: Message) => p.name),
      ["helper", "skill-creator"],
    );

    const notListed = await resolve(`absent@${OFFICIAL}`, "op-3");
    assert.equal(notListed.diagnostics[0].code, "plugin_suggested_reference_not_listed");

    officialStatus = 500;
    const failed = await resolve(`absent@${OFFICIAL}`, "op-4");
    assert.deepEqual(failed.diagnostics[0], {
      code: "marketplace_refresh_failed",
      message: "Failed to fetch marketplace: 500 Internal Server Error",
      severity: "error",
      pluginId: `absent@${OFFICIAL}`,
    });

    await put(join(store, "known_marketplaces.json"), knownRecord(web.url("/hold")));
    const arrival = web.arrival();
    const pending = resolve(`absent@${OFFICIAL}`, "op-5");
    await arrival;
    assert.deepEqual(
      await h.client.request(
        "plugins/cancelOperation",
        { operationId: "op-5" },
        zcodePluginsCancelOperationResultSchema,
      ),
      { operationId: "op-5", cancelled: true },
    );
    const cancelled = await pending;
    assert.equal(cancelled.diagnostics[0].code, "plugin_operation_cancelled");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
    await web.close();
  }
});
