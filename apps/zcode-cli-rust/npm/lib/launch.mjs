import { mkdir, mkdtemp, writeFile, copyFile, readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { once } from "node:events";

export function cleanLaunchEnv(source) {
  const env = { ...source };
  // 3.14.3 的 command override 会丢存储握手字段；不能让用户旧 shell 覆盖破坏桥接选择。
  for (const key of Object.keys(env))
    if (
      key.startsWith("ZCODE_AGENT_SERVER_") ||
      key.startsWith("ZCODE_STDIO_TAP_") ||
      ["ZCODE_DESKTOP_AGENT_BYTECODE", "ELECTRON_RUN_AS_NODE", "NODE_OPTIONS"].includes(key)
    )
      delete env[key];
  return env;
}
export async function prepareLaunch({ app, binary, runtimeDir }) {
  const launches = join(runtimeDir, "launches");
  await mkdir(launches, { recursive: true, mode: 0o700 });
  const cwd = await mkdtemp(join(launches, "app-"));
  const bridgeDir = join(cwd, "apps/zcode-cli/packages/cli/dist");
  await mkdir(bridgeDir, { recursive: true });
  const bridge = join(bridgeDir, "zcode.cjs"),
    manifest = join(cwd, "launch.json");
  await copyFile(new URL("../bridge.cjs", import.meta.url), bridge);
  await writeFile(
    manifest,
    JSON.stringify({
      schemaVersion: 1,
      appVersion: app.version,
      appExecutable: app.executable,
      nodeBundle: app.nodeBundle,
      binary,
      launchDir: cwd,
    }),
    { mode: 0o600 },
  );
  return { cwd, bridge, manifest };
}
export async function startApp(app, launch, env = process.env, args = []) {
  // 必须直接 exec 已安装的 App；open/LaunchServices 不保证使用这里的 cwd。
  const child = spawn(app.executable, args, {
    cwd: launch.cwd,
    env: cleanLaunchEnv(env),
    stdio: "ignore",
    detached: true,
  });
  await once(child, "spawn");
  const record = {
    schemaVersion: 1,
    appVersion: app.version,
    appExecutable: app.executable,
    appPid: child.pid,
    launchedAt: new Date().toISOString(),
  };
  await writeFile(join(launch.cwd, "app.json"), JSON.stringify(record), { mode: 0o600 });
  child.unref();
  return record;
}
export async function launchStatus(runtimeDir) {
  const root = join(runtimeDir, "launches");
  let names;
  try {
    names = await readdir(root);
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
  const records = [];
  for (const name of names.filter((n) => n.startsWith("app-"))) {
    const directory = join(root, name);
    try {
      const app = JSON.parse(await readFile(join(directory, "app.json"), "utf8"));
      const receipts = [];
      for (const file of (await readdir(directory)).filter((n) => /^bridge-\d+\.json$/.test(n)))
        receipts.push(JSON.parse(await readFile(join(directory, file), "utf8")));
      records.push({ ...app, launchDir: directory, receipts });
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  return records.sort((a, b) => b.launchedAt.localeCompare(a.launchedAt)).slice(0, 5);
}
