import { access, open, readFile, realpath, readdir } from "node:fs/promises";
import { constants } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { homedir } from "node:os";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
const exec = promisify(execFile);
export const APP_VERSION = "3.14.3";

// 只读取 ASAR 顶层 package.json，不加载 App 代码；有界读取避免将整个安装包载入内存。
export async function readAppPackage(resources) {
  try {
    return JSON.parse(await readFile(join(resources, "app/package.json"), "utf8"));
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const file = await open(join(resources, "app.asar"), "r");
  try {
    const prefix = Buffer.alloc(16);
    if ((await file.read(prefix, 0, 16, 0)).bytesRead !== 16)
      throw Error("Invalid app.asar header");
    const headerSize = prefix.readUInt32LE(4),
      jsonSize = prefix.readUInt32LE(12);
    if (headerSize < 8 || headerSize > 32 * 1024 * 1024 || jsonSize > headerSize - 8)
      throw Error("Invalid app.asar size");
    const header = Buffer.alloc(jsonSize);
    if ((await file.read(header, 0, jsonSize, 16)).bytesRead !== jsonSize)
      throw Error("Truncated app.asar header");
    const entry = JSON.parse(header.toString()).files?.["package.json"];
    const offset = Number(entry?.offset);
    if (
      !entry ||
      entry.unpacked ||
      !Number.isSafeInteger(offset) ||
      offset < 0 ||
      !Number.isInteger(entry.size) ||
      entry.size < 1 ||
      entry.size > 1024 * 1024
    )
      throw Error("Invalid App package metadata");
    const buffer = Buffer.alloc(entry.size);
    if ((await file.read(buffer, 0, entry.size, 8 + headerSize + offset)).bytesRead !== entry.size)
      throw Error("Truncated App metadata");
    return JSON.parse(buffer.toString());
  } finally {
    await file.close();
  }
}

export async function inspectApp(input) {
  let executable = resolve(input);
  if (executable.endsWith(".app")) executable = join(executable, "Contents/MacOS/ZCode");
  executable = await realpath(executable);
  await access(executable, constants.X_OK);
  const resources =
    process.platform === "darwin" && executable.includes("/Contents/MacOS/")
      ? resolve(dirname(executable), "../Resources")
      : join(dirname(executable), "resources");
  const pkg = await readAppPackage(resources);
  if (pkg.version !== APP_VERSION)
    throw Error(`Only ZCode ${APP_VERSION} is supported; found ${pkg.version ?? "unknown"}.`);
  const nodeBundle = join(resources, "glm/zcode.cjs");
  await access(nodeBundle, constants.R_OK);
  return { executable, resources, nodeBundle, version: pkg.version };
}

export async function discoverApp(input) {
  if (input) return inspectApp(input);
  const candidates =
    process.platform === "darwin"
      ? ["/Applications/ZCode.app", join(homedir(), "Applications/ZCode.app")]
      : process.platform === "win32"
        ? [
            join(process.env.LOCALAPPDATA ?? homedir(), "Programs/ZCode/ZCode.exe"),
            join(process.env.ProgramFiles ?? "C:\\Program Files", "ZCode/ZCode.exe"),
          ]
        : ["/opt/ZCode/zcode", "/opt/zcode/zcode"];
  for (const candidate of candidates) {
    try {
      await access(candidate);
    } catch {
      continue;
    }
    return inspectApp(candidate);
  }
  throw Error(
    "ZCode installation not found. Use --app <ZCode.app or executable>; Linux requires an extracted installation.",
  );
}

export async function runningAppPids(executable) {
  if (process.platform === "win32") {
    const { stdout } = await exec(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $env:ZCODE_RUST_APP_CHECK } | Select-Object -ExpandProperty ProcessId",
      ],
      {
        env: { ...process.env, ZCODE_RUST_APP_CHECK: executable },
        timeout: 15000,
        maxBuffer: 1024 * 1024,
      },
    );
    return stdout.trim().split(/\s+/).filter(Boolean).map(Number);
  }
  if (process.platform === "linux") {
    const pids = [];
    for (const pid of await readdir("/proc"))
      if (/^\d+$/.test(pid)) {
        try {
          if ((await realpath(`/proc/${pid}/exe`)) === executable) pids.push(Number(pid));
        } catch {
          /* 已退出或其他用户的进程。 */
        }
      }
    return pids;
  }
  // process.title 会把 ps 的 comm/args 改成应用显示名；按加载的 executable 查询，避免漏掉已有实例。
  try {
    const { stdout } = await exec(
      "/usr/sbin/lsof",
      ["-n", "-P", "-a", "-d", "txt", "-Fp", "--", executable],
      { timeout: 15000, maxBuffer: 1024 * 1024 },
    );
    return [
      ...new Set(
        stdout
          .split("\n")
          .filter((line) => /^p\d+$/.test(line))
          .map((line) => Number(line.slice(1))),
      ),
    ];
  } catch (error) {
    if (error.code === 1 && !error.stderr?.trim()) return [];
    throw error;
  }
}
