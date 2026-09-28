#!/usr/bin/env node
/* eslint-disable max-lines */

import { access, cp, mkdir } from "node:fs/promises";
import {
  chmodSync,
  copyFileSync,
  cpSync,
  createWriteStream,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import process from "node:process";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { resolveRemoteNativeSearchPrebuiltPlan } from "./remote-native-search-tools-config.mjs";
import { prepareNativeSearchTools } from "./prepare-native-search-tools.mjs";
import { stageNodeNotices, stageThirdPartyNotices } from "./third-party-notices.mjs";
import {
  computeDeterministicSourceSha256 as computeComponentSourceSha256,
  packSourceAsDeterministicTarGzip as packComponentSourceAsArchive,
} from "./deterministic-tar-archive.mjs";
import { runCommand } from "./spawn-command.mjs";
import { resolveIntranetDepsBaseUrl } from "./intranetDefaults.mjs";

export { computeComponentSourceSha256, packComponentSourceAsArchive };

const require = createRequire(import.meta.url);
const scriptDir = dirname(fileURLToPath(import.meta.url));
const rootDir = resolve(scriptDir, "..");
const desktopDir = join(rootDir, "packages/desktop");
const mockCdnDir = join(desktopDir, "mock-cdn");
const version = require(join(rootDir, "package.json")).version;
const ZCODE_AGENT_RUNTIME = {
  glm: {
    version: readZCodeAgentRuntimeVersion(),
  },
};
const releaseDir = join(mockCdnDir, "releases", version);
const nodeVersion = "v22.16.0";
const componentSchemaVersion = 1;
const remotePlatforms = ["linux-arm64", "linux-x64", "darwin-arm64", "darwin-x64"];
const pnpmCommand = process.platform === "win32" ? "pnpm.cmd" : "pnpm";
const isBootstrapWithRemote = process.env.ZCODE_BOOTSTRAP_WITH_REMOTE === "1";

/**
 * Node dist 下载源。默认走国内镜像，`ZCODE_NODE_DIST_MIRROR` 可覆盖（与
 * `.gitlab/ci/00-workflow.yml` 的同名 CI 变量、`scripts/cua-helper-sea-base.mjs` 同一约定）。
 *
 * 这里原本硬编码 `https://nodejs.org/dist`，而 macOS
 * runner 连不上它 —— 3 次尝试全部 `UND_ERR_CONNECT_TIMEOUT`（10s）。更糟的是本文件的报错文案
 * 一直在让人「检查 Node.js 镜像地址」，可当时根本没有这个旋钮。
 *
 * 为什么之前没暴露：`mock-cdn` 靠 GIT_CLEAN_FLAGS 排除项跨 job 持久化，而
 * `build:remote:assets` 每次都 `rm -rf` 掉除自己 $VERSION 以外的所有 release 目录。
 * 不同版本目录因此互相驱逐持久化产物，谁被驱逐谁就必须回源下载。
 * 平时都是 `[skip] already exists`，所以这条网络路径长期没被真正走过。
 */
export const DEFAULT_NODE_DIST_BASE = "https://cdn.npmmirror.com/binaries/node";

export function nodeDistBase(env = process.env) {
  const mirror = env.ZCODE_NODE_DIST_MIRROR?.trim();
  return (mirror || DEFAULT_NODE_DIST_BASE).replace(/\/+$/u, "");
}
const BROWSER_USE_PLUGIN_PACKAGE_NAME = "@zcode/browser-use-plugin";
// node_repl 宿主抽成独立包 @zcode/node-repl-host 之后，browser-use
// 不再产出 dist/mcp/server.js，CUA 资产也已归 @zcode/zcode-cua-plugin。这是**第三份**平行清单
// （另两份：packages/desktop/scripts/prepare-agent-node-bundle.mjs 的生产打包、
// scripts/build-desktop-agent-cli.mjs 的 dev 构建），当时只改了 dev 那份，于是先后在
// build:macos:arm64 与 build:remote:assets 上以 "missing runtime" 挂掉两次。
// 权威归属见 bootstrap/official-plugin-definitions.ts。
const browserUseRequiredRuntimePaths = [
  "scripts/browser-client.mjs",
  "docs/api.json",
  "docs/documents.json",
  "docs/overview.md",
  // remote prebuild 必须和桌面 seed 使用同一录屏文档完整性合同。
  "docs/recording.md",
  "docs/workflow.md",
  "skills/control-browser/SKILL.md",
  "skills/web-gui-tester/SKILL.md",
];
const remoteOfficialPluginPackages = [
  // 44b25ed46c「remove bundled plugins except browser use and cua」删掉了其余
  // 内置插件源码，但漏改这份清单，bootstrap:with-remote 在 staging 第一个 manifest 就抛
  // missing。此处与 packages/desktop/scripts/prepare-agent-node-bundle.mjs 的桌面 seed
  // 清单、packages/server/src/remote/zcodeAgentOfficialPluginAssets.ts 的远端合同保持一致。
  {
    // 远端 shared-host 必须部署 node_repl runtime，否则只剩 skill 而没有 mcp__node_repl__js ——
    // 该 runtime 现由 @zcode/node-repl-host 提供（见下一个条目），browser-use 只带自己的
    // client script 与 skill/docs。
    packageName: "@zcode/browser-use-plugin",
    relativePath: "apps/zcode-cli/packages/browser-use-plugin",
    requiresRuntime: true,
    requiredRuntimePaths: browserUseRequiredRuntimePaths,
    runtimeBuildScript: "scripts/build.mjs",
    stagedPath: "packages/browser-use-plugin",
  },
  {
    // node_repl 宿主：Browser Use 与 Computer Use 共用的 MCP runtime。远端 shared-host 缺它
    // 就没有 mcp__node_repl__js，bua/cua 两边都会连不上。
    packageName: "@zcode/node-repl-host",
    relativePath: "apps/zcode-cli/packages/node-repl-host",
    requiresRuntime: true,
    requiredRuntimePaths: ["dist/mcp/server.js"],
    runtimeBuildScript: "scripts/build.mjs",
    stagedPath: "packages/node-repl-host",
  },
];
// 随 CLI 内置的技能包（不是插件）：远端 agent 的 bootstrap 沿官方插件同款候选目录在 zcode.cjs 旁
// 找 packages/bundled-skills 并原地读取；与 packages/desktop/scripts/prepare-agent-node-bundle.mjs 同一份清单。
const remoteBundledSkillPack = {
  relativePath: "apps/zcode-cli/packages/bundled-skills",
  requiredPaths: [
    "skills/dynamic-workflows/SKILL.md",
    "skills/dynamic-workflows/patterns.md",
    "skills/dynamic-workflows/examples.md",
  ],
  stagedPath: "packages/bundled-skills",
  topLevelPaths: ["skills"],
};
const remoteOfficialPluginTopLevelPaths = new Set([
  ".mcp.json",
  ".zcode-plugin",
  "README.md",
  // 生产远程预构建有独立顶层白名单，遗漏 agents 会在上传前永久裁掉子代理。
  "agents",
  "commands",
  "dist",
  "docs",
  "hooks",
  "output-styles",
  "package.json",
  "scripts",
  "skills",
  "templates",
]);
const excludedOfficialPluginAssetNames = new Set([
  ".DS_Store",
  ".venv",
  "__pycache__",
  "node_modules",
]);

function shouldCopyOfficialPluginAsset(sourcePath) {
  const name = basename(sourcePath);
  return !excludedOfficialPluginAssetNames.has(name) && !name.endsWith(".pyc");
}
const remoteOfficialPluginRequiredPaths = [
  "packages/browser-use-plugin/.zcode-plugin/plugin.json",
  "packages/node-repl-host/.zcode-plugin/plugin.json",
];

function readZCodeAgentRuntimeVersion() {
  const runtimeSourcePath = join(rootDir, "packages/shared/src/zcode-agent-runtime.ts");
  const runtimeSource = readFileSync(runtimeSourcePath, "utf8");
  const match = runtimeSource.match(/version:\s*["']([^"']+)["']/);
  if (!match?.[1]) {
    throw new Error("Unable to parse ZCode Agent runtime version");
  }
  return match[1];
}

async function download(url, destinationPath) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    throw new Error(`Download failed: HTTP ${response.status} (${url})`);
  }
  if (!response.body) {
    throw new Error(`Download failed: empty response body (${url})`);
  }

  // 原实现使用 response.pipe(file) + finish 监听，网络中断时可能既不 resolve 也不 reject，
  // 最终触发 Node 24 的 unsettled top-level await。改为 pipeline，确保异常路径可观测且可失败退出。
  await pipeline(
    Readable.fromWeb(response.body),
    createWriteStream(destinationPath, { flags: "w" }),
  );
}

async function downloadWithRetry(url, destinationPath, maxAttempts = 3) {
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      await download(url, destinationPath);
      return;
    } catch (error) {
      if (attempt >= maxAttempts) {
        throw error;
      }
      console.warn(`  [warn] download attempt ${attempt}/${maxAttempts} failed: ${url}`);
      console.warn(`  [warn] retry reason: ${String(error)}`);
    }
  }
}

async function extractArchiveMember(url, destinationDir, archiveMember) {
  const tempDir = mkdtempSync(join(tmpdir(), "zcode-node-dist-"));
  const archivePath = join(tempDir, "node.tar.xz");

  try {
    await downloadWithRetry(url, archivePath);
    // Bugfix: Windows 下绝对路径带盘符冒号（C:\...），GNU tar（Git Bash）会把 "C:" 当成
    // 远程主机名报 "Cannot connect to C"。改用 cwd + 相对归档名，避开 -f 参数里的冒号。
    // 反斜杠路径同样会被 MSYS tar 参数转换破坏（\3 被当转义），-C 目标统一转正斜杠，
    // 对 bsdtar 与 Linux/macOS CI 无影响。
    runCommand(
      "tar",
      [
        "-xJf",
        "node.tar.xz",
        "--strip-components=2",
        "-C",
        destinationDir.replaceAll("\\", "/"),
        archiveMember,
      ],
      {
        cwd: tempDir,
      },
    );
  } finally {
    // Bugfix: Windows 下刚写完的归档可能被杀毒/索引器或尚未退出的 xz 子进程短暂持有句柄，
    // rmSync 立即删除会 EPERM，且 finally 里抛出的异常会掩盖真正的下载/解压错误。
    // 带重试删除，失败时仅告警，让原始错误正常抛出。
    try {
      rmSync(tempDir, { force: true, recursive: true, maxRetries: 10, retryDelay: 500 });
    } catch (error) {
      console.warn(`  [warn] 清理临时目录失败（可忽略）: ${tempDir}`);
      console.warn(`  [warn] ${String(error)}`);
    }
  }
}

function resolveDedicatedPackageRoot(packageName, fromDir) {
  const packageEntryPath = require.resolve(packageName, { paths: [fromDir] });
  let currentDir = dirname(packageEntryPath);

  while (true) {
    const packageJsonPath = join(currentDir, "package.json");
    if (existsSync(packageJsonPath)) {
      const packageJson = require(packageJsonPath);
      if (packageJson?.name === packageName) {
        return currentDir;
      }
    }

    const parentDir = dirname(currentDir);
    if (parentDir === currentDir) {
      break;
    }
    currentDir = parentDir;
  }

  throw new Error(
    `Unable to resolve package root for ${packageName} from entry ${packageEntryPath}`,
  );
}

function resolveNodePtyPackageName(platformKey) {
  return `@lydell/node-pty-${platformKey}`;
}

function resolveNodePtyPackageVersion(platformKey) {
  const packageName = resolveNodePtyPackageName(platformKey);
  const packageRoot = resolveDedicatedPackageRoot(packageName, join(rootDir, "packages/server"));
  const packageJson = require(join(packageRoot, "package.json"));
  if (typeof packageJson?.version !== "string" || !packageJson.version.trim()) {
    throw new Error(`Unable to resolve version for ${packageName}`);
  }
  return packageJson.version.trim();
}

async function prepareNodeBinaries() {
  for (const platformKey of remotePlatforms) {
    const nodeDir = join(releaseDir, "node", platformKey);
    const nodeBinaryPath = join(nodeDir, "node");
    await stageNodeNotices(nodeDir, nodeVersion, rootDir);

    if (existsSync(nodeBinaryPath)) {
      console.log(`  [skip] mock-cdn node/${platformKey} already exists`);
      continue;
    }

    mkdirSync(nodeDir, { recursive: true });
    const archiveName = `node-${nodeVersion}-${platformKey}.tar.xz`;
    const url = `${nodeDistBase()}/${nodeVersion}/${archiveName}`;

    console.log(`  [download] ${url}`);

    try {
      await extractArchiveMember(url, nodeDir, `node-${nodeVersion}-${platformKey}/bin/node`);
      chmodSync(nodeBinaryPath, 0o755);
      console.log(`  [ok] mock-cdn node/${platformKey}`);
    } catch (error) {
      console.error(`  [error] 下载或解压失败: ${url}`);
      console.error(
        `  [error] 请检查 CI runner 的外网访问、tar/xz 依赖，或用 ZCODE_NODE_DIST_MIRROR 覆盖下载源（当前 ${nodeDistBase()}）`,
      );
      throw error;
    }
  }
}

function buildServerBundle() {
  console.log("==> Building server bundle");

  try {
    if (isBootstrapWithRemote) {
      runBootstrapServerRemoteBuild();
      return;
    }

    // Windows CI（Node 24）里直接 spawnSync("pnpm.cmd") 会在拉起子进程前就抛 EINVAL。
    // 这里统一走跨平台启动封装，让 .cmd 通过 shell/cmd.exe 执行，避免远端资源准备阶段提前中断。
    runCommand(pnpmCommand, ["run", "build:remote"], {
      cwd: join(rootDir, "packages/server"),
    });
  } catch (error) {
    console.error(
      "  [error] packages/server build:remote 失败，请优先检查 CI 日志中的 TypeScript / esbuild 输出",
    );
    throw error;
  }
}

function runBootstrapServerRemoteBuild() {
  // bootstrap:with-remote 会在本地串联 install、remote assets、workspace build。
  // 这里不能复用已有 zcode-server.cjs：开发时 package version 常不变，旧 bundle 会把缺少新 RPC 的
  // server 部署到 SSH 远端。只保留“直接用当前 Node 启动 tsx”的低内存优化，不改变 CI 的 build:remote。
  runCommand(
    process.execPath,
    [join(rootDir, "node_modules/tsx/dist/cli.mjs"), "build-remote.ts"],
    {
      cwd: join(rootDir, "packages/server"),
      env: process.env,
    },
  );
}

function copyServerBundle() {
  const serverDir = join(releaseDir, "server");
  mkdirSync(serverDir, { recursive: true });
  copyFileSync(
    join(rootDir, "packages/server/dist/remote/zcode-server.cjs"),
    join(serverDir, "zcode-server.cjs"),
  );
  console.log("  [ok] mock-cdn server/zcode-server.cjs");
}

function copyNodePtyPrebuilds() {
  console.log("==> Copying node-pty prebuilds from @lydell/node-pty");

  for (const platformKey of remotePlatforms) {
    const ptyDir = join(releaseDir, "node-pty", platformKey);
    const targetBinaryPath = join(ptyDir, "pty.node");
    const targetSpawnHelperPath = join(ptyDir, "spawn-helper");
    const requiresSpawnHelper = platformKey.startsWith("darwin-");

    if (
      existsSync(targetBinaryPath) &&
      (!requiresSpawnHelper || existsSync(targetSpawnHelperPath))
    ) {
      console.log(`  [skip] mock-cdn node-pty/${platformKey} already exists`);
      continue;
    }

    mkdirSync(ptyDir, { recursive: true });

    const packageName = resolveNodePtyPackageName(platformKey);
    let packageRoot;
    try {
      packageRoot = resolveDedicatedPackageRoot(packageName, join(rootDir, "packages/server"));
    } catch {
      console.log(`  [warn] ${packageName} not found, run: pnpm install`);
      continue;
    }

    const sourcePrebuildDir = join(packageRoot, "prebuilds", platformKey);
    const sourceBinaryPath = join(sourcePrebuildDir, "pty.node");
    if (!existsSync(sourceBinaryPath)) {
      console.log(`  [warn] binary not found at ${sourceBinaryPath}`);
      continue;
    }

    // Darwin 平台 node-pty 除了 pty.node 还依赖 spawn-helper。
    // 之前 mock-cdn 只复制了 pty.node，远端部署后会在 terminal.create 阶段报 posix_spawn ENOENT。
    // 这里把 spawn-helper 一并拷贝进 remote 资产目录，避免远端终端启动时缺关键二进制。
    copyFileSync(sourceBinaryPath, targetBinaryPath);
    if (requiresSpawnHelper) {
      const sourceSpawnHelperPath = join(sourcePrebuildDir, "spawn-helper");
      if (!existsSync(sourceSpawnHelperPath)) {
        console.log(`  [warn] spawn-helper not found at ${sourceSpawnHelperPath}`);
        continue;
      }
      copyFileSync(sourceSpawnHelperPath, targetSpawnHelperPath);
      chmodSync(targetSpawnHelperPath, 0o755);
    }
    console.log(`  [ok] mock-cdn node-pty/${platformKey} (copied from ${packageName})`);
  }
}

function buildRemoteOfficialPluginRuntimes() {
  for (const plugin of remoteOfficialPluginPackages) {
    if (!plugin.requiresRuntime) continue;
    console.log(`==> Building remote official plugin runtime: ${plugin.packageName}`);
    if (isBootstrapWithRemote) {
      buildRemoteOfficialPluginRuntimeForBootstrap(plugin);
      assertRemoteOfficialPluginRuntime(plugin);
      continue;
    }

    runCommand(
      pnpmCommand,
      ["--dir", join(rootDir, "apps/zcode-cli"), "--filter", plugin.packageName, "build"],
      {
        cwd: rootDir,
        env: process.env,
      },
    );
    assertRemoteOfficialPluginRuntime(plugin);
  }
}

function buildRemoteOfficialPluginRuntimeForBootstrap(plugin) {
  const pluginRoot = join(rootDir, plugin.relativePath);
  const hasCompleteRuntime = plugin.requiredRuntimePaths.every((relativePath) =>
    existsSync(join(pluginRoot, ...relativePath.split("/"))),
  );
  if (plugin.packageName !== BROWSER_USE_PLUGIN_PACKAGE_NAME && hasCompleteRuntime) {
    console.log(`  [skip] reuse existing remote official plugin runtime: ${plugin.packageName}`);
    return;
  }

  // bootstrap:with-remote 会串行准备远端资源和工作区构建。
  // 官方插件 runtime 只在 stage 资源时需要，这里用当前 Node 执行等价构建，避免再嵌套 pnpm/tsc shim。
  // browser-use 的 MCP server 与 browser-client 必须来自同一次构建；只凭旧 server.js 判定可复用
  // 会让远端资源混入陈旧或缺失的 client，因此 bootstrap 模式下对该插件无条件重建。
  runCommand(process.execPath, ["../../node_modules/typescript/bin/tsc"], {
    cwd: pluginRoot,
    env: process.env,
  });
  runCommand(process.execPath, [plugin.runtimeBuildScript], {
    cwd: pluginRoot,
    env: process.env,
  });
}

function assertRemoteOfficialPluginRuntime(plugin) {
  const pluginRoot = join(rootDir, plugin.relativePath);
  for (const relativePath of plugin.requiredRuntimePaths) {
    const runtimePath = join(pluginRoot, ...relativePath.split("/"));
    if (!existsSync(runtimePath)) {
      throw new Error(`[prepare-prebuilds] missing remote official plugin runtime: ${runtimePath}`);
    }
  }
}

function stageRemoteOfficialPlugins(glmDir) {
  for (const plugin of remoteOfficialPluginPackages) {
    const sourceRoot = join(rootDir, plugin.relativePath);
    const manifestPath = join(sourceRoot, ".zcode-plugin", "plugin.json");
    if (!existsSync(manifestPath)) {
      throw new Error(
        `[prepare-prebuilds] missing remote official plugin manifest: ${manifestPath}`,
      );
    }

    const targetRoot = join(glmDir, ...plugin.stagedPath.split("/"));
    mkdirSync(targetRoot, { recursive: true });
    for (const entryName of remoteOfficialPluginTopLevelPaths) {
      const sourcePath = join(sourceRoot, entryName);
      if (!existsSync(sourcePath)) continue;
      cpSync(sourcePath, join(targetRoot, entryName), {
        recursive: true,
        filter: shouldCopyOfficialPluginAsset,
      });
    }
    for (const relativePath of remoteOfficialPluginRequiredPaths) {
      if (!relativePath.startsWith(`${plugin.stagedPath}/`)) continue;
      const stagedAssetPath = join(glmDir, ...relativePath.split("/"));
      if (!existsSync(stagedAssetPath)) {
        throw new Error(
          `[prepare-prebuilds] missing staged remote official plugin seed asset: ${stagedAssetPath}`,
        );
      }
    }
    console.log(`  [ok] mock-cdn glm official plugin ${plugin.stagedPath}`);
  }
}

async function stageRemoteBundledSkillPack(glmDir) {
  const sourceRoot = join(rootDir, remoteBundledSkillPack.relativePath);
  const targetRoot = join(glmDir, ...remoteBundledSkillPack.stagedPath.split("/"));
  await mkdir(targetRoot, { recursive: true });
  for (const entryName of remoteBundledSkillPack.topLevelPaths) {
    const sourcePath = join(sourceRoot, entryName);
    await cp(sourcePath, join(targetRoot, entryName), {
      recursive: true,
      filter: shouldCopyOfficialPluginAsset,
    });
  }
  for (const relativePath of remoteBundledSkillPack.requiredPaths) {
    const stagedAssetPath = join(targetRoot, ...relativePath.split("/"));
    await access(stagedAssetPath);
  }
  console.log(`  [ok] mock-cdn glm bundled skill pack ${remoteBundledSkillPack.stagedPath}`);
}

// 远端 agent 现在跑编译出来的 zcode.cjs（而不是各平台独立的原生二进制）：
// 远端部署时已经有一份独立 node（跑 zcode-server.cjs），agent 复用它执行 zcode.cjs 即可，
// 不必再为每个平台准备一份内嵌 node 的 SEA 二进制。zcode.cjs 跨平台同一份，逐平台只是放进各自的
// glm/<platform> 组件目录，保持现有 manifest 组件结构不变。
async function stageRemoteAgentBundles() {
  console.log("==> Building zcode-cli bundle for remote agents");
  // 复用桌面同款构建脚本（turbo build:desktop-agent --filter=@zcode/cli），命中缓存时几乎瞬时。
  runCommand(process.execPath, [join(rootDir, "scripts/build-desktop-agent-cli.mjs")], {
    cwd: rootDir,
    env: process.env,
  });
  // browser-use runtime 的 tsc 依赖 @zcode/core/dist。远端资产也必须先构建
  // agent CLI 依赖，避免 CI 干净检出时被开发机缓存掩盖的 TS2307。
  buildRemoteOfficialPluginRuntimes();
  const cliBundlePath = join(rootDir, "apps/zcode-cli/packages/cli/dist/zcode.cjs");
  if (!existsSync(cliBundlePath)) {
    throw new Error(`[prepare-prebuilds] expected cli bundle missing: ${cliBundlePath}`);
  }

  for (const platformKey of remotePlatforms) {
    const glmDir = join(releaseDir, "glm", platformKey);
    // 干净重建：glm 组件现在只含 zcode.cjs，清掉历史遗留的原生二进制 / 旧 meta，
    // 避免被打进组件 tar 把远端资源撑大。
    rmSync(glmDir, { recursive: true, force: true });
    mkdirSync(glmDir, { recursive: true });
    copyFileSync(cliBundlePath, join(glmDir, "zcode.cjs"));
    stageRemoteOfficialPlugins(glmDir);
    await stageRemoteBundledSkillPack(glmDir);
    console.log(`  [ok] mock-cdn glm/${platformKey}/zcode.cjs`);
  }
}

function canResolveIntranetDepsBaseUrl() {
  try {
    resolveIntranetDepsBaseUrl();
    return true;
  } catch {
    return false;
  }
}

export async function prepareRemoteNativeSearchTools({
  platforms = remotePlatforms,
  outputDir = join(releaseDir, "tools"),
} = {}) {
  console.log("==> Preparing local native search binaries for remote platforms");
  for (const platformKey of platforms) {
    const [targetOs, targetArch] = platformKey.split("-");
    if (!targetOs || !targetArch) {
      throw new Error(`Invalid remote platform key: ${platformKey}`);
    }

    await prepareNativeSearchTools({
      prebuiltPlan: resolveRemoteNativeSearchPrebuiltPlan({
        platform: targetOs,
        arch: targetArch,
        outputDir: join(outputDir, platformKey),
      }),
    });
  }
}

function joinPosix(...segments) {
  return segments.join("/").replace(/\/+/g, "/");
}

function normalizeSemanticPrefix(rawPrefix, fallback = "v1") {
  const prefix = String(rawPrefix ?? "").trim();
  if (!prefix) {
    return fallback;
  }

  const normalized = prefix.replace(/\+/g, "-");
  if (!normalized) {
    return fallback;
  }
  return normalized.startsWith("v") ? normalized : `v${normalized}`;
}

function computeFileSha256(filePath) {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

function isSha256(value) {
  return /^[a-f0-9]{64}$/u.test(
    String(value ?? "")
      .trim()
      .toLowerCase(),
  );
}

function buildComponentVersion(semanticPrefix) {
  return normalizeSemanticPrefix(semanticPrefix);
}

export function buildContentAddressedComponentVersion(semanticPrefix, sha256) {
  const normalizedSha = String(sha256 ?? "")
    .trim()
    .toLowerCase();
  if (!/^[a-f0-9]{64}$/.test(normalizedSha)) {
    throw new Error(`Invalid component sha256: ${sha256}`);
  }
  return `${buildComponentVersion(semanticPrefix)}+${normalizedSha.slice(0, 12)}`;
}

export function buildComponentArtifactRelativePath(platformKey, componentId, componentVersion) {
  return joinPosix("components", platformKey, componentId, `${componentVersion}.tar.gz`);
}

function resolveComponentSemanticVersion(componentVersion) {
  const version = String(componentVersion ?? "").trim();
  const plusIndex = version.lastIndexOf("+");
  if (plusIndex < 0 || plusIndex === version.length - 1) {
    return version;
  }

  const suffix = version.slice(plusIndex + 1).toLowerCase();
  return /^[a-f0-9]{12,64}$/.test(suffix) ? version.slice(0, plusIndex) : version;
}

// glm 承载 zcode-cli app-server 协议 schema。即使 runtime 版本未变化，
// zcode.cjs 也可能随 app 代码变更；跨 release 复用旧 glm 会让远端 agent 拒绝新协议字段。
const nonReusableReleaseAssetIds = new Set(["server-bundle", "glm"]);

function readJsonFile(filePath) {
  try {
    return JSON.parse(readFileSync(filePath, "utf8"));
  } catch {
    return null;
  }
}

function compareVersionSegments(left, right) {
  const leftParts = String(left).split(/[.-]/);
  const rightParts = String(right).split(/[.-]/);
  const length = Math.max(leftParts.length, rightParts.length);

  for (let index = 0; index < length; index += 1) {
    const leftPart = leftParts[index] ?? "0";
    const rightPart = rightParts[index] ?? "0";
    const leftNumber = /^\d+$/.test(leftPart) ? Number(leftPart) : null;
    const rightNumber = /^\d+$/.test(rightPart) ? Number(rightPart) : null;

    if (leftNumber !== null && rightNumber !== null) {
      if (leftNumber !== rightNumber) {
        return leftNumber - rightNumber;
      }
      continue;
    }

    const compared = leftPart.localeCompare(rightPart, undefined, {
      numeric: true,
    });
    if (compared !== 0) {
      return compared;
    }
  }

  return 0;
}

function findReusableReleaseDirs({ mockCdnDir, currentVersion }) {
  const releasesDir = join(mockCdnDir, "releases");
  if (!existsSync(releasesDir)) {
    return [];
  }

  return readdirSync(releasesDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && entry.name !== currentVersion)
    .map((entry) => entry.name)
    .filter((candidate) => compareVersionSegments(candidate, currentVersion) < 0)
    .sort((left, right) => compareVersionSegments(right, left))
    .map((candidate) => join(releasesDir, candidate));
}

function normalizeManifestComponents(manifest) {
  if (!manifest || !Array.isArray(manifest.components)) {
    return new Map();
  }

  return new Map(
    manifest.components
      .filter((component) => typeof component?.id === "string")
      .map((component) => [component.id, component]),
  );
}

export function restoreReusableReleaseAssets({
  mockCdnDir,
  currentVersion,
  releaseDir,
  componentDefinitionsByPlatform,
}) {
  const previousReleaseDirs = findReusableReleaseDirs({
    mockCdnDir,
    currentVersion,
  });
  if (previousReleaseDirs.length === 0) {
    return;
  }

  for (const [platformKey, componentDefinitions] of componentDefinitionsByPlatform.entries()) {
    for (const componentDefinition of componentDefinitions) {
      if (nonReusableReleaseAssetIds.has(componentDefinition.id)) {
        continue;
      }

      for (const previousReleaseDir of previousReleaseDirs) {
        const previousManifest = readJsonFile(
          join(previousReleaseDir, `manifest-${platformKey}.json`),
        );
        const previousComponents = normalizeManifestComponents(previousManifest);
        const previousComponent = previousComponents.get(componentDefinition.id);
        if (
          resolveComponentSemanticVersion(previousComponent?.version) !==
          resolveComponentSemanticVersion(componentDefinition.version)
        ) {
          continue;
        }

        const sourcePath = join(previousReleaseDir, ...componentDefinition.mount.split("/"));
        const targetPath = join(releaseDir, ...componentDefinition.mount.split("/"));
        if (!existsSync(sourcePath)) {
          continue;
        }

        const requiredPaths = componentDefinition.requiredPaths ?? [];
        const hasTargetRequiredPaths =
          existsSync(targetPath) &&
          requiredPaths.every((relativePath) =>
            existsSync(join(targetPath, ...relativePath.split("/"))),
          );
        if (hasTargetRequiredPaths) {
          continue;
        }

        const hasRequiredPaths = requiredPaths.every((relativePath) =>
          existsSync(join(sourcePath, ...relativePath.split("/"))),
        );
        if (!hasRequiredPaths) {
          continue;
        }

        // app version 变更会生成新的 releases/<version> 目录，mock-cdn cache 命中不能依赖该路径。
        // 这里仅在组件自身版本一致且关键文件完整时复制历史 release，避免稳定 runtime 重复下载。
        mkdirSync(dirname(targetPath), { recursive: true });
        if (existsSync(targetPath)) {
          // 上一次 bootstrap 中断可能留下只有 .part 文件的残缺目标目录。
          // 目标目录存在但关键文件不完整时不能跳过复用，先清掉再用历史 release 的完整资源修复。
          rmSync(targetPath, { force: true, recursive: true });
        }
        cpSync(sourcePath, targetPath, { recursive: true });
        console.log(
          `  [reuse] ${componentDefinition.id} ${platformKey} from ${basename(previousReleaseDir)}`,
        );
        break;
      }
    }
  }
}

function buildReusableComponentDefinitionsByPlatform() {
  return new Map(
    remotePlatforms.map((platformKey) => [
      platformKey,
      buildRemoteComponentDefinitions(platformKey).map((component) => ({
        id: component.id,
        version: buildComponentVersion(component.semanticPrefix),
        mount: component.mount,
        requiredPaths: buildReusableComponentRequiredPaths(component.id, platformKey),
      })),
    ]),
  );
}

function buildReusableComponentRequiredPaths(componentId, platformKey) {
  switch (componentId) {
    case "node-runtime":
      return ["node"];
    case "node-pty":
      return platformKey.startsWith("darwin-") ? ["pty.node", "spawn-helper"] : ["pty.node"];
    case "glm":
      // GLM 现在是编译产物 zcode.cjs（跨平台同一份），远端用已部署的 node 执行它。
      // 复用时还要确认官方插件 seed 资源完整，否则旧 release 会继续产出 0 builtin plugin 的远端资源包。
      return ["zcode.cjs", ...remoteOfficialPluginRequiredPaths];
    case "bfs":
      return ["bfs"];
    case "ripgrep":
      return [platformKey.startsWith("win32-") ? "rg.exe" : "rg"];
    case "ugrep":
      return ["ugrep"];
    default:
      return [];
  }
}

export function buildRemoteComponentDefinitions(platformKey) {
  const baseComponents = [
    {
      id: "server-bundle",
      semanticPrefix: version,
      mount: "server",
      sourcePath: join(releaseDir, "server"),
    },
    {
      id: "node-runtime",
      semanticPrefix: nodeVersion,
      mount: joinPosix("node", platformKey),
      sourcePath: join(releaseDir, "node", platformKey),
    },
    {
      id: "node-pty",
      // node-pty 组件之前固定成 v1，平台包升级后客户端仍会命中旧 cache。
      // 这里使用实际复制来源包的版本，让 @lydell/node-pty-<platform> 升级时组件 cache 自动失效。
      semanticPrefix: resolveNodePtyPackageVersion(platformKey),
      mount: joinPosix("node-pty", platformKey),
      sourcePath: join(releaseDir, "node-pty", platformKey),
    },
    {
      id: "glm",
      // GLM native binary 之前固定成 v1，二进制版本升级后不会触发组件 cache 失效。
      // 这里复用 ZCODE_AGENT_RUNTIME.glm.version，保持 manifest 版本与运行时描述一致。
      semanticPrefix: ZCODE_AGENT_RUNTIME.glm.version,
      mount: joinPosix("glm", platformKey),
      sourcePath: join(releaseDir, "glm", platformKey),
    },
  ];

  const [platform, arch] = platformKey.split("-");
  const nativeSearchPlan = resolveRemoteNativeSearchPrebuiltPlan({
    platform,
    arch,
    outputDir: join(releaseDir, "tools", platformKey),
  });

  return [
    ...baseComponents,
    ...nativeSearchPlan.artifacts
      .toSorted((left, right) => left.toolId.localeCompare(right.toolId))
      .map((artifact) => ({
        id: artifact.toolId,
        semanticPrefix: artifact.release,
        mount: joinPosix("tools", platformKey, artifact.toolId),
        sourcePath: dirname(artifact.binaryPath),
      })),
  ];
}

function tryReuseRemoteComponentArtifact({
  mockCdnDir,
  component,
  previousComponent,
  sourceSha256,
}) {
  if (!previousComponent) {
    return null;
  }

  if (previousComponent.id !== component.id || previousComponent.mount !== component.mount) {
    return null;
  }

  if (
    resolveComponentSemanticVersion(previousComponent.version) !==
    buildComponentVersion(component.semanticPrefix)
  ) {
    return null;
  }

  if (previousComponent.sourceSha256 !== sourceSha256) {
    return null;
  }

  if (
    typeof previousComponent.artifactPath !== "string" ||
    typeof previousComponent.sha256 !== "string" ||
    !isSha256(previousComponent.sha256)
  ) {
    return null;
  }

  const artifactPath = join(mockCdnDir, ...previousComponent.artifactPath.split("/"));
  if (!existsSync(artifactPath)) {
    return null;
  }

  if (computeFileSha256(artifactPath) !== previousComponent.sha256) {
    return null;
  }

  return previousComponent;
}

export function prepareRemoteComponentArtifact({
  mockCdnDir,
  platformKey,
  component,
  previousComponents = new Map(),
}) {
  if (!existsSync(component.sourcePath)) {
    throw new Error(
      `Missing component source for ${component.id} (${platformKey}): ${component.sourcePath}`,
    );
  }

  const sourceSha256 = computeComponentSourceSha256(component.sourcePath);
  const previousComponent = previousComponents.get(component.id);
  const reusedComponent = tryReuseRemoteComponentArtifact({
    mockCdnDir,
    component,
    previousComponent,
    sourceSha256,
  });
  if (reusedComponent) {
    // remote mock-cdn 组件源内容没变时不能每次重打 tar.gz。
    // 这里用源目录内容指纹命中已有 manifest 和 artifact，避免 bootstrap:with-remote 重复压缩大组件。
    console.log(`  [skip] component ${component.id} ${platformKey} unchanged`);
    return reusedComponent;
  }

  const semanticComponentVersion = buildComponentVersion(component.semanticPrefix);
  const stagingArtifactRelativePath = joinPosix(
    "components",
    platformKey,
    component.id,
    `${semanticComponentVersion}.tmp-${process.pid}-${Date.now()}.tar.gz`,
  );
  const stagingArtifactPath = join(mockCdnDir, ...stagingArtifactRelativePath.split("/"));
  mkdirSync(dirname(stagingArtifactPath), { recursive: true });

  // 同版本本地重跑时继续复用旧 tar 会让 manifest sha256 指向陈旧内容。
  // 这里先打临时包再把内容 hash 写进最终文件名，避免 CDN 缓存继续命中同名旧对象。
  packComponentSourceAsArchive(component.sourcePath, stagingArtifactPath);
  const artifactSha256 = computeFileSha256(stagingArtifactPath);
  const componentVersion = buildContentAddressedComponentVersion(
    component.semanticPrefix,
    artifactSha256,
  );
  const artifactRelativePath = buildComponentArtifactRelativePath(
    platformKey,
    component.id,
    componentVersion,
  );
  const artifactPath = join(mockCdnDir, ...artifactRelativePath.split("/"));
  if (artifactPath !== stagingArtifactPath) {
    rmSync(artifactPath, { force: true });
    mkdirSync(dirname(artifactPath), { recursive: true });
    renameSync(stagingArtifactPath, artifactPath);
  }
  console.log(`  [component] ${component.id} ${platformKey} -> ${artifactRelativePath}`);

  return {
    id: component.id,
    version: componentVersion,
    sha256: artifactSha256,
    sourceSha256,
    artifactPath: artifactRelativePath,
    mount: component.mount,
  };
}

function prepareRemoteComponentArtifacts() {
  console.log("==> Packaging component artifacts and manifests");

  const componentRootDir = join(mockCdnDir, "components");
  mkdirSync(componentRootDir, { recursive: true });

  for (const platformKey of remotePlatforms) {
    const componentManifestEntries = [];
    const componentDefinitions = buildRemoteComponentDefinitions(platformKey);
    const previousComponents = normalizeManifestComponents(
      readJsonFile(join(releaseDir, `manifest-${platformKey}.json`)),
    );

    for (const component of componentDefinitions) {
      if (!existsSync(component.sourcePath)) {
        if (!canResolveIntranetDepsBaseUrl()) {
          console.warn(
            `  [skip] component ${component.id} (${platformKey}): source missing and intranet deps source is not configured`,
          );
          continue;
        }
      }
      componentManifestEntries.push(
        prepareRemoteComponentArtifact({
          mockCdnDir,
          platformKey,
          component,
          previousComponents,
        }),
      );
    }

    const manifestPath = join(releaseDir, `manifest-${platformKey}.json`);
    writeFileSync(
      manifestPath,
      `${JSON.stringify(
        {
          schemaVersion: componentSchemaVersion,
          appVersion: version,
          platformArch: platformKey,
          components: componentManifestEntries,
        },
        null,
        2,
      )}\n`,
      "utf8",
    );
    console.log(`  [ok] mock-cdn releases/${version}/manifest-${platformKey}.json`);
  }
}

async function main() {
  console.log(`==> Preparing mock CDN release in ${releaseDir}`);

  mkdirSync(releaseDir, { recursive: true });
  restoreReusableReleaseAssets({
    mockCdnDir,
    currentVersion: version,
    releaseDir,
    componentDefinitionsByPlatform: buildReusableComponentDefinitionsByPlatform(),
  });

  await prepareNodeBinaries();
  buildServerBundle();
  copyServerBundle();
  copyNodePtyPrebuilds();
  await stageRemoteAgentBundles();
  await prepareRemoteNativeSearchTools();
  // 修复：server、pty、agent 均可独立下载，需在组件哈希计算前补齐各自的声明。
  await stageThirdPartyNotices(join(releaseDir, "server"), rootDir);
  for (const platformKey of remotePlatforms) {
    await stageThirdPartyNotices(join(releaseDir, "node-pty", platformKey), rootDir);
    await stageThirdPartyNotices(join(releaseDir, "glm", platformKey), rootDir);
  }
  prepareRemoteComponentArtifacts();

  console.log(`==> Done! Mock CDN release ready at ${releaseDir}`);
}

const entryHref = process.argv[1] ? pathToFileURL(process.argv[1]).href : null;
if (entryHref === import.meta.url) {
  await main();
}
