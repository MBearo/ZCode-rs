import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { test } from "node:test";
import { promisify } from "node:util";
import { ensurePackagedNodePtySpawnHelper } from "../../packages/desktop/scripts/node-pty-package-assets.mjs";

const exec = promisify(execFile);
const root = resolve(import.meta.dirname, "../..");
const appleSecrets = {
  APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64: "dGVzdA==",
  APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD: "fixture-password",
  APPLE_ID: "fixture@example.invalid",
  APPLE_PASSWORD: "fixture-app-password",
  APPLE_TEAM_ID: "FIXTURETEAM",
};

test(
  "macOS packaging repairs spawn-helper permissions after ASAR extraction",
  { skip: process.platform !== "darwin" },
  async (t) => {
    const dir = await mkdtemp(resolve(tmpdir(), "zcode-pty-assets-"));
    t.after(() => rm(dir, { recursive: true, force: true }));
    const packageDir = resolve(dir, "app.asar.unpacked/node_modules/node-pty");
    const helper = resolve(packageDir, "prebuilds/darwin-arm64/spawn-helper");
    await mkdir(resolve(packageDir, "prebuilds/darwin-arm64"), { recursive: true });
    await writeFile(helper, "fixture");
    await chmod(helper, 0o644);
    await ensurePackagedNodePtySpawnHelper({ resourcesDir: dir, platformKey: "darwin-arm64" });
    assert.equal((await stat(helper)).mode & 0o777, 0o755);
  },
);

for (const [os, arch, flags] of [
  ["mac", "arm64", "--mac --arm64"],
  ["win", "x64", "--win --x64"],
]) {
  test(`bundle ${os}/${arch} never publishes, even in tag CI`, async () => {
    const { stdout } = await exec(
      process.execPath,
      ["packages/desktop/scripts/bundle.mjs", "--os", os, "--arch", arch, "--dry-run"],
      { cwd: root, env: { ...process.env, CI: "true", GITHUB_REF: "refs/tags/v0.0.0" } },
    );
    assert.ok(stdout.includes(`${flags} --publish never`), stdout);
  });
}

test("bundle rejects 32-bit Windows", async () => {
  await assert.rejects(
    exec(
      process.execPath,
      ["packages/desktop/scripts/bundle.mjs", "--os", "win", "--arch", "ia32", "--dry-run"],
      { cwd: root },
    ),
    /不支持的目标 CPU 架构/,
  );
});

for (const [label, signing, expected] of [
  ["ad-hoc", {}, { identity: "-", force: false, hardened: false }],
  [
    "Developer ID",
    { ZCODE_ENABLE_MAC_SIGN: "1", APPLE_SIGNING_IDENTITY: "Developer ID Application: Fixture" },
    { identity: "Fixture", force: true, hardened: true },
  ],
]) {
  test(`macOS ARM64 ${label} signing policy`, async () => {
    const { stdout } = await exec(
      process.execPath,
      [
        "--input-type=module",
        "-e",
        `
      const { default: config } = await import('./packages/desktop/electron-builder.config.js');
      console.log(JSON.stringify({identity: config.mac.identity, force: config.forceCodeSigning, hardened: config.mac.hardenedRuntime}));
    `,
      ],
      {
        cwd: root,
        env: {
          ...process.env,
          ZCODE_ENV: "production",
          ZCODE_TARGET_OS: "mac",
          ZCODE_TARGET_ARCH: "arm64",
          ZCODE_ENABLE_MAC_SIGN: "0",
          APPLE_SIGNING_IDENTITY: "",
          CSC_NAME: "",
          ...signing,
        },
      },
    );
    assert.deepEqual(JSON.parse(stdout.trim()), expected);
  });
}

async function macFixture(t, env = {}) {
  const dir = await mkdtemp(resolve(tmpdir(), "zcode-signing-test-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  for (const path of [
    "scripts/ci",
    "bin",
    "temp",
    "packages/desktop/bundled-agents/darwin-arm64/glm",
    "packages/desktop/bundled-tools/darwin-arm64/ripgrep",
  ]) {
    await mkdir(resolve(dir, path), { recursive: true });
  }
  await copyFile(
    resolve(root, "scripts/ci/build-macos.sh"),
    resolve(dir, "scripts/ci/build-macos.sh"),
  );
  await writeFile(
    resolve(dir, "packages/desktop/bundled-tools/darwin-arm64/ripgrep/rg"),
    "fixture",
  );
  await chmod(resolve(dir, "packages/desktop/bundled-tools/darwin-arm64/ripgrep/rg"), 0o755);
  const mock = `#!/bin/bash
set -eu
tool="$(basename "$0")"
printf '%s %s\\n' "$tool" "$*" >> "$MOCK_LOG"
case "$tool" in
  security)
    case "$1" in
      list-keychains) if [ "$#" -eq 3 ]; then echo '"/fixture/login.keychain-db"'; fi ;;
      create-keychain) touch "\${@: -1}" ;;
      find-identity) printf '  1) ABCDEF123456 "Developer ID Application: Fixture"\\n     1 valid identities found\\n' ;;
      import) if [ "\${MOCK_FAIL:-}" = import ]; then exit 1; fi ;;
    esac ;;
  file) echo 'Mach-O 64-bit executable arm64' ;;
  codesign) if [ "\${MOCK_FAIL:-}" = sign ]; then exit 1; fi ;;
  pnpm)
    printf 'package-umask %s\\n' "$(umask)" >> "$MOCK_LOG"
    mkdir -p packages/desktop/dist/mac-arm64/ZCode.app
    printf 'installer' > packages/desktop/dist/ZCode-0.0.0-mac-arm64.dmg ;;
  xcrun)
    if [ "$1" = notarytool ]; then printf '{"id":"fixture-request","status":"%s"}\\n' "\${MOCK_NOTARY_STATUS:-Accepted}"; fi
    if [ "$1 $2" = 'stapler validate' ] && [ "\${MOCK_FAIL:-}" = staple ]; then exit 1; fi ;;
esac
`;
  for (const name of ["security", "file", "codesign", "pnpm", "xcrun"]) {
    const file = resolve(dir, "bin", name);
    await writeFile(file, mock);
    await chmod(file, 0o755);
  }
  const cleanEnv = { ...process.env };
  for (const key of [
    ...Object.keys(appleSecrets),
    "APPLE_SIGNING_IDENTITY",
    "CSC_NAME",
    "CSC_KEYCHAIN",
    "ZCODE_DESKTOP_DIST_DIR",
  ])
    delete cleanEnv[key];
  return {
    run: () =>
      exec("bash", ["scripts/ci/build-macos.sh"], {
        cwd: dir,
        env: {
          ...cleanEnv,
          PATH: `${resolve(dir, "bin")}:${process.env.PATH}`,
          RUNNER_TEMP: resolve(dir, "temp"),
          MOCK_LOG: resolve(dir, "commands.log"),
          ...env,
        },
      }),
    log: () => readFile(resolve(dir, "commands.log"), "utf8"),
  };
}

test(
  "macOS without secrets signs resources ad-hoc and skips notarization",
  { skip: process.platform !== "darwin" },
  async (t) => {
    const fixture = await macFixture(t);
    await fixture.run();
    const log = await fixture.log();
    assert.match(log, /codesign --force --sign - --timestamp=none/);
    assert.match(log, /pnpm bundle:desktop -- --os mac --arch arm64 --skip-prepare --skip-build/);
    assert.doesNotMatch(log, /security import|notarytool/);
    assert.match(log, /security create-keychain/);
    assert.match(log, /security delete-keychain/);
    assert.ok(log.includes(`package-umask ${process.umask().toString(8).padStart(4, "0")}`));
  },
);

test(
  "macOS partial secrets fail before packaging",
  { skip: process.platform !== "darwin" },
  async (t) => {
    const fixture = await macFixture(t, { APPLE_ID: appleSecrets.APPLE_ID });
    await assert.rejects(
      fixture.run(),
      /Missing GitHub Secrets:.*APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64/,
    );
  },
);

for (const [label, env, failure] of [
  ["success", {}, false],
  ["certificate import failure", { MOCK_FAIL: "import" }, true],
  ["resource signing failure", { MOCK_FAIL: "sign" }, true],
  ["notarization rejection", { MOCK_NOTARY_STATUS: "Invalid" }, true],
  ["staple validation failure", { MOCK_FAIL: "staple" }, true],
]) {
  test(
    `macOS signed build: ${label}, always cleans keychain`,
    { skip: process.platform !== "darwin" },
    async (t) => {
      const fixture = await macFixture(t, { ...appleSecrets, ...env });
      if (failure) await assert.rejects(fixture.run());
      else await fixture.run();
      const log = await fixture.log();
      assert.match(log, /security delete-keychain/);
      assert.match(log, /security list-keychains -d user -s \/fixture\/login.keychain-db/);
      if (!failure) {
        assert.ok(log.includes(`package-umask ${process.umask().toString(8).padStart(4, "0")}`));
        assert.ok(log.indexOf("codesign --force") < log.indexOf("pnpm bundle:desktop"));
        assert.ok(log.indexOf("pnpm bundle:desktop") < log.indexOf("xcrun notarytool submit"));
        assert.match(log, /xcrun stapler validate/);
      }
      if (env.MOCK_NOTARY_STATUS === "Invalid") assert.doesNotMatch(log, /xcrun stapler/);
    },
  );
}
