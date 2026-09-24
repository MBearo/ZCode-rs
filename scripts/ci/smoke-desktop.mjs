import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { promisify } from "node:util";

const root = resolve(import.meta.dirname, "../..");
const dist = resolve(root, "packages/desktop", process.env.ZCODE_DESKTOP_DIST_DIR || "dist");
const appRoot =
  process.platform === "darwin"
    ? resolve(dist, "mac-arm64/ZCode.app/Contents")
    : resolve(dist, "win-unpacked");
const executable = resolve(appRoot, process.platform === "darwin" ? "MacOS/ZCode" : "ZCode.exe");
const resources = resolve(appRoot, process.platform === "darwin" ? "Resources" : "resources");
const workdir = await mkdtemp(resolve(tmpdir(), "zcode-packaged-smoke-"));

// 直接运行包内 Electron，避免系统 Node 和开发机 node_modules 掩盖原生 ABI、漏包或文件权限问题。
const probe = `
  const { createRequire } = require('node:module');
  const path = require('node:path');
  const fromApp = createRequire(path.join(process.env.ZCODE_SMOKE_RESOURCES, 'app.asar/package.json'));
  for (const name of ['node-forge', 'undici', 'yauzl', 'yazl', 'ssh2', '@opentelemetry/exporter-trace-otlp-proto']) fromApp(name);
  const windows = process.platform === 'win32';
  const shell = windows ? path.join(process.env.SystemRoot, 'System32', 'cmd.exe') : '/bin/sh';
  const args = windows ? ['/d', '/c', 'echo ZCODE_NATIVE_PTY_OK'] : ['-c', 'printf ZCODE_NATIVE_PTY_OK'];
  const pty = fromApp('node-pty').spawn(shell, args, {cwd: process.cwd(), env: process.env, useConptyDll: false});
  let output = '';
  const deadline = setTimeout(() => { pty.kill(); process.exit(1); }, 10000);
  pty.onData(chunk => { output += chunk; });
  pty.onExit(({exitCode}) => {
    clearTimeout(deadline);
    if (exitCode !== 0 || !output.includes('ZCODE_NATIVE_PTY_OK')) process.exit(1);
    console.log(JSON.stringify({platform: process.platform, arch: process.arch, electron: process.versions.electron, runtimeModules: 'passed', nativePty: 'passed'}));
  });
`;

try {
  const { stdout, stderr } = await promisify(execFile)(executable, ["-e", probe], {
    cwd: workdir,
    timeout: 30_000,
    env: { ...process.env, ELECTRON_RUN_AS_NODE: "1", ZCODE_SMOKE_RESOURCES: resources },
  });
  process.stdout.write(stdout);
  process.stderr.write(stderr);
} finally {
  await rm(workdir, { recursive: true, force: true });
}
