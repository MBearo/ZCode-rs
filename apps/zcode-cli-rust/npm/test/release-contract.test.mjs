import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { readFile, mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
const frozen = JSON.parse(
  await readFile(new URL("./fixtures/v3.14.3-resolver.json", import.meta.url), "utf8"),
);

test("the released resolver selects the bridge in packaged mode and preserves storage entry", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "release-resolver-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const entry = join(root, "apps/zcode-cli/packages/cli/dist/zcode.cjs");
  await mkdir(dirname(entry), { recursive: true });
  await writeFile(entry, "// fixture");
  const sandbox = {
    exports: {},
    process: {
      cwd: () => root,
      versions: { electron: "41" },
      execPath: "/installed/Electron Helper",
      env: {},
    },
    join,
    dirname,
    existsSync,
    findZCodeAgentRuntimeNodeBundle: () => "/installed/resources/glm/zcode.cjs",
    findZCodeAgentRuntimeBinary: () => null,
    ZCODE_AGENT_RUNTIME: { spawnArgs: ["app-server", "--stdio"] },
  };
  vm.runInNewContext(frozen.javascript, sandbox);
  const context = {
    workspacePath: "/user/work space",
    workspaceKey: "/user/work space",
    presentationSurface: "desktop",
  };
  const result = sandbox.exports.resolveDefaultZCodeAgentCommand(context);
  assert.equal(result.supportsStorageStartup, true);
  assert.equal(result.storagePreparationEntry, entry);
  assert.deepEqual([...result.args], [entry, "app-server", "--stdio", "--surface", "desktop"]);
  assert.equal(result.cwd, context.workspacePath);
  sandbox.process.env.ZCODE_AGENT_SERVER_COMMAND = "/native";
  const direct = sandbox.exports.resolveDefaultZCodeAgentCommand(context);
  assert.equal(Boolean(direct.supportsStorageStartup && direct.storagePreparationEntry), false);
});
