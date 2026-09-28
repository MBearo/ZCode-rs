/* eslint-disable max-lines -- Electron Builder config keeps related packaging hooks together so build order stays explicit. */
import { cpSync, existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createRequire } from "node:module";
import { runCommand, runCommandAndReadStdout } from "../../scripts/spawn-command.mjs";
import { loadBuiltinProviderConfig } from "../../scripts/builtin-provider-config.mjs";
import { noticesFileName, stageElectronNotices } from "../../scripts/third-party-notices.mjs";
import { resolveNativeSearchReleasePlan } from "../../scripts/native-search-tools-config.mjs";
import { getBuildMetadata } from "./scripts/build-metadata.mjs";
import { collectRuntimeModuleClosureEntries } from "./scripts/runtime-dependency-closure.mjs";
import {
  resolvePackagedNodePtyPrebuildPath,
  restoreTargetNodePtyPrebuild,
} from "./scripts/node-pty-package-assets.mjs";
import { cleanupPackagedSourcemaps } from "./scripts/packaged-sourcemap-cleanup.mjs";
import { getTargetPlatform } from "./scripts/target-platform.mjs";
import {
  resolveDesktopArtifactSuffix,
  resolveDesktopProductIdentity,
} from "./scripts/desktop-product-identity.mjs";
import { verifyStagedKoffi } from "./scripts/koffi-package-assets.mjs";
const ELECTRON_BUILDER_ARCH = {
  1: "x64",
  3: "arm64",
};
function resolveElectronBuilderWindowsTarget({
  electronPlatformName,
  arch,
  configuredTargetPlatform,
}) {
  if (electronPlatformName !== "win32") {
    throw new Error(
      `[electron-builder.config] context platform is not win32: ${String(electronPlatformName)}`,
    );
  }
  const actualArch = ELECTRON_BUILDER_ARCH[arch];
  if (!actualArch) {
    throw new Error(
      `[electron-builder.config] unsupported electron-builder Windows architecture: ${String(arch)}`,
    );
  }
  const actualTarget = {
    os: "win32",
    arch: actualArch,
    key: `win32-${actualArch}`,
  };
  if (
    configuredTargetPlatform?.os !== actualTarget.os ||
    configuredTargetPlatform?.arch !== actualTarget.arch ||
    configuredTargetPlatform?.key !== actualTarget.key
  ) {
    throw new Error(
      `[electron-builder.config] configured target ${String(configuredTargetPlatform?.key)} does not match electron-builder target ${actualTarget.key}`,
    );
  }
  return actualTarget;
}
import {
  findDesktopNativePackageViolations,
  createDesktopNativePackagePrunePatterns,
  parseAsarListWithPackState,
} from "./scripts/desktop-native-package-policy.mjs";
import { replaceAppAsarFromStaging } from "./scripts/app-asar-repack.mjs";
import {
  patchNsisInstallSectionFile,
  restoreNsisInstallSectionFileSync,
} from "./scripts/patch-nsis-install-section.mjs";

const buildMetadata = getBuildMetadata();
const targetPlatform = getTargetPlatform();
const builtinProviderConfig = await loadBuiltinProviderConfig();
const desktopProductIdentity = resolveDesktopProductIdentity({
  ...process.env,
  ZCODE_ENV: builtinProviderConfig.environment,
});
const nativeSearchReleasePlan = resolveNativeSearchReleasePlan({
  platform: targetPlatform.os,
  arch: targetPlatform.arch,
});
const rawMacSigningIdentity = process.env.APPLE_SIGNING_IDENTITY || process.env.CSC_NAME;
const macSigningIdentity =
  rawMacSigningIdentity?.replace(/^Developer ID Application:\s*/, "") ?? null;
const shouldEnableMacSigning =
  process.env.ZCODE_ENABLE_MAC_SIGN === "1" && Boolean(macSigningIdentity);
const workspaceRoot = resolve(import.meta.dirname, "../..");
const desktopPackageRoot = import.meta.dirname;
const runtimeModuleLookupRoots = [
  desktopPackageRoot,
  workspaceRoot,
  resolve(desktopPackageRoot, "node_modules", ".pnpm", "node_modules"),
  resolve(workspaceRoot, "node_modules", ".pnpm", "node_modules"),
];
const desktopDistDir = process.env.ZCODE_DESKTOP_DIST_DIR || "dist";
const DEFAULT_ELECTRON_MIRROR = "https://npmmirror.com/mirrors/electron/";
// `pnpm exec asar` 依赖 `.bin/asar`，但 @electron/asar 仅是 electron-builder 传递依赖时，
// Linux CI（pnpm hoisted）往往解析不到该二进制，`asar list` 未运行即 exit 1。
// 显式依赖 @electron/asar 并用 Node 直接执行 CLI，避免跨平台找不齐 shim。
const requireFromConfig = createRequire(import.meta.url);
let nsisInstallSectionPatched = false;
let nsisInstallSectionOriginalSource = null;
let nsisInstallSectionPath = null;
const desktopElectronVersion = requireFromConfig("./package.json").devDependencies.electron;
const asarCliPath = resolve(
  dirname(requireFromConfig.resolve("@electron/asar/package.json")),
  "bin",
  "asar.js",
);
const REQUIRED_ASAR_RUNTIME_MODULES = [
  "module-details-from-path",
  "@opentelemetry/api-logs",
  // Bugfix: telemetry 的 OTLP exporter 会在启动阶段加载 sdk-metrics。pnpm 开发态可从
  // workspace 根目录解析，但 electron-builder 不会稳定复制这条 hoisted 依赖，导致安装包启动即崩溃。
  // 将 sdk-metrics 作为闭包根注入，同时递归带齐它的 OpenTelemetry 运行时依赖。
  "@opentelemetry/sdk-metrics",
  // OTLP proto 导出链闭包根：递归带齐 otlp-transformer/protobufjs 及其子依赖，
  // 否则 hoisted 布局漏 protobufjs 时已安装应用启动即报 Cannot find module 'protobufjs/minimal'。
  "@opentelemetry/exporter-trace-otlp-proto",
  "@opentelemetry/exporter-metrics-otlp-proto",
  "pngjs",
  // @zcode/services 的代理连通性探测会动态 require("undici") 取 ProxyAgent。
  // tsup 虽然把 services 代码并进了主/host 产物，但不会把这个运行时 require 的包内联进去，
  // electron-builder 产物又可能漏掉 hoisted 的 undici，最终 mac 安装包启动即报 Cannot find module "undici"。
  // 这里把 undici 和其他兜底依赖一样强制注入 app.asar，避免用户在已安装应用里主进程直接崩溃。
  "undici",
  // node-forge 一直只写在 bundle.mjs 的校验名单里，靠 electron-builder 自己打进 app.asar；
  // 这与 yauzl 漏 pend 是同一类隐患——校验要求的模块必须有人负责补齐。node-forge 无子依赖，
  // 已在产物里时 afterPack 扫描会跳过它，不改变现有打包结果。
  "node-forge",
  // 2.7.0 起 services 新增反馈日志压缩链路并引入 yazl；2.6.0 没有这条启动期依赖，
  // pnpm hoisted 布局下 yazl 可能进了 app.asar，但子依赖 buffer-crc32 没有稳定随包进入产物；
  // 这里显式以 yazl 作为闭包根注入，让递归依赖收集把 ZIP 打包链路所需依赖一起补齐。
  "yazl",
  // yauzl 成为 desktop/services 的直接生产依赖后，pnpm list --prod 会把顶层 yauzl 节点
  // 去重成没有子依赖的空节点；electron-builder 的 pnpm collector 以先登记的空节点为准，
  // 跳过后面带完整子树的那个，于是 app.asar 里有 yauzl 却没有它的运行时依赖 pend，
  // 要到 bundle 校验阶段才报「缺少运行时依赖 pend」。这里以 yauzl 作为闭包根注入，
  // 与 bundle.mjs 的校验名单保持一致，让递归依赖收集把 pend 一起补进产物。
  "yauzl",
  // Bugfix: 飞书 SDK 被 desktop main/host 外置后，安装包运行时必须能从 app.asar 解析到它。
  // 这里显式注入 scoped 包，避免开发态正常、生产包启动时才报 Cannot find module。
  "@larksuiteoapi/node-sdk",
  // 生产态里 ssh2 虽然被打进 app.asar，但它的依赖链偶发被 electron-builder 漏拷。
  // 已出现线上报错 Cannot find module 'asn1'（Require stack: ssh2 keyParser）。
  // 这里把 ssh2 关键依赖链一起注入，避免远程 SSH 连接在已安装应用里因缺包直接失败。
  "asn1",
  "bcrypt-pbkdf",
  "tweetnacl",
  // electron-updater → builder-util-runtime → debug 运行时 require("ms")。
  // pnpm hoisted 布局下 electron-builder 偶发漏拷这个叶子依赖；3.4.0(ci/cua-v0.3.17 打的)
  // 已在线上触发安装包启动即报 Cannot find module 'ms'（Require stack: debug/src/common.js），
  // 自动更新链路直接崩。ms 是叶子包，显式注入即可让 debug 在 app.asar 内稳定解析。
  "ms",
];
// pacman 依赖必须使用 Arch 官方仓库中的包名。electron-builder 的历史默认集合包含
// 已移除的 libappindicator-gtk3/http-parser，且缺少 Electron 实际需要的运行库；显式
// 维护最小运行时闭包，避免 pacman -U 无法解析依赖或启动时才暴露缺库。
const PACMAN_RUNTIME_DEPENDENCIES = [
  "gtk3",
  "nss",
  "libxss",
  "libxtst",
  "libnotify",
  "alsa-lib",
  "mesa",
  "xdg-utils",
];

const WINDOWS_INSTALL_MANIFEST_NAME = ".zcode-install-manifest";

async function writeWindowsInstallManifest(context) {
  if (context.electronPlatformName !== "win32") return;

  const root = context.appOutDir;
  const files = [];
  const visit = async (directory, relativeDirectory = "") => {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const relativePath = relativeDirectory ? `${relativeDirectory}/${entry.name}` : entry.name;
      const absolutePath = join(directory, entry.name);
      if (entry.isDirectory()) {
        await visit(absolutePath, relativePath);
      } else if (entry.isFile() || entry.isSymbolicLink()) {
        files.push(relativePath.replaceAll("/", "\\"));
      }
    }
  };

  await visit(root);
  files.sort();
  await writeFile(join(root, WINDOWS_INSTALL_MANIFEST_NAME), `${files.join("\r\n")}\r\n`, "utf8");
}

function resolveElectronDownloadMirror(env = process.env) {
  const existingMirror =
    env.ZCODE_ELECTRON_RUNTIME_MIRROR ||
    env.NPM_CONFIG_ELECTRON_MIRROR ||
    env.npm_config_electron_mirror ||
    env.npm_package_config_electron_mirror ||
    env.ELECTRON_MIRROR;
  if (existingMirror?.trim()) {
    return existingMirror.trim();
  }

  return DEFAULT_ELECTRON_MIRROR;
}

const commandStdoutMaxBuffer = 64 * 1024 * 1024;
// 产物后缀只标记后端环境（_TEST）；身份靠 productName 区分，生产后端的 Preview 包没有后缀。
const desktopArtifactEnvSuffix = resolveDesktopArtifactSuffix(process.env);

// Preview 是内部签名测试包。CI 明确打开 macOS 签名时若没有身份，必须在生成未签名包前失败，
// 避免“产物存在”被误认为已经走完和生产版相同的签名链路。
if (
  desktopProductIdentity.flavor === "preview" &&
  process.env.ZCODE_ENABLE_MAC_SIGN === "1" &&
  !macSigningIdentity
) {
  throw new Error(
    "ZCode Preview macOS packaging requires APPLE_SIGNING_IDENTITY or CSC_NAME when ZCODE_ENABLE_MAC_SIGN=1",
  );
}

const PACKAGING_PRUNE_PATTERNS = [
  "!**/*.map",
  "!**/*.pdb",
  "!**/__tests__/**",
  "!**/test/**",
  "!**/tests/**",
  "!**/example/**",
  "!**/examples/**",
  "!**/README*",
  "!**/CHANGELOG*",
  "!**/CONTRIBUTING*",
  "!**/CODE_OF_CONDUCT*",
  "!**/SECURITY*",
];

function buildDesktopArtifactName(platformName, extension = "${ext}") {
  // 测试环境产物必须和正式安装包文件名区分，避免上传、下载或人工验收时混用。
  return `\${productName}-\${version}-${platformName}-\${arch}${desktopArtifactEnvSuffix}.${extension}`;
}

function runAsarCommand(args) {
  runCommand(process.execPath, [asarCliPath, ...args], {
    cwd: import.meta.dirname,
    env: process.env,
  });
}

function runAsarCommandAndReadStdout(args) {
  return runCommandAndReadStdout(process.execPath, [asarCliPath, ...args], {
    cwd: import.meta.dirname,
    env: process.env,
    // app.asar 在当前桌面包体积较大，asar list 输出可能超过默认缓冲并触发 ENOBUFS。
    // 这里显式放大缓冲，避免“为了判断是否需要注入”反而把打包流程误判失败。
    maxBuffer: commandStdoutMaxBuffer,
    stdio: ["ignore", "pipe", "inherit"],
  });
}

function runTimedSync(label, fn) {
  const startMs = Date.now();
  console.log(`[ci][timer] ${label} start`);
  try {
    return fn();
  } finally {
    console.log(`[ci][timer] ${label} end duration_ms=${Date.now() - startMs}`);
  }
}

async function runTimedAsync(label, fn) {
  const startMs = Date.now();
  console.log(`[ci][timer] ${label} start`);
  try {
    return await fn();
  } finally {
    console.log(`[ci][timer] ${label} end duration_ms=${Date.now() - startMs}`);
  }
}

function resolveAppAsarPath(context) {
  if (context.electronPlatformName === "darwin") {
    const appName = `${context.packager?.appInfo?.productFilename ?? "ZCode"}.app`;
    return resolve(context.appOutDir, appName, "Contents", "Resources", "app.asar");
  }

  return resolve(context.appOutDir, "resources", "app.asar");
}

function resolvePackagedResourcesDir(context) {
  if (context.electronPlatformName === "darwin") {
    const appName = `${context.packager?.appInfo?.productFilename ?? "ZCode"}.app`;
    return resolve(context.appOutDir, appName, "Contents", "Resources");
  }

  return resolve(context.appOutDir, "resources");
}

function normalizeAsarEntry(entry) {
  return entry.trim().replaceAll("\\", "/");
}

function resolveMissingRuntimeModules(appAsarPath) {
  const asarEntries = runAsarCommandAndReadStdout(["list", appAsarPath])
    .split("\n")
    .map(normalizeAsarEntry)
    .filter(Boolean);
  const asarEntrySet = new Set(asarEntries);

  const runtimeModules = collectRuntimeModuleClosureEntries(
    REQUIRED_ASAR_RUNTIME_MODULES,
    runtimeModuleLookupRoots,
  );
  const resolvableRuntimeModules = runtimeModules.filter((entry) => {
    if (!entry.sourceModulePath) {
      // 不同平台/安装布局下，部分运行时依赖可能被裁剪或未落到本次打包工作区。
      // 之前这里直接在 copy 阶段抛错会中断整个平台出包；改为记录告警并跳过该模块，
      // 让 afterPack 只处理当前环境确实可解析的依赖，避免 CI 因单个可选依赖缺失全量失败。
      console.warn(
        `[afterPack] runtime module not found, skip injection: ${entry.moduleName}; searched=${runtimeModuleLookupRoots
          .map((lookupRoot) => resolve(lookupRoot, "node_modules", entry.moduleName))
          .join(", ")}`,
      );
      return false;
    }
    return true;
  });
  return resolvableRuntimeModules.filter((entry) => {
    const { moduleName } = entry;
    const moduleRoot = `/node_modules/${moduleName}`;
    if (asarEntrySet.has(moduleRoot)) {
      return false;
    }
    for (const entry of asarEntrySet) {
      if (entry.startsWith(`${moduleRoot}/`)) {
        return false;
      }
    }
    return true;
  });
}

async function injectHoistedRuntimeModulesIntoAsar(context) {
  const appAsarPath = resolveAppAsarPath(context);
  if (!existsSync(appAsarPath)) {
    throw new Error(`打包产物缺少 app.asar: ${appAsarPath}`);
  }

  const missingRuntimeModules = runTimedSync("afterPack:scan-missing-runtime-modules", () =>
    resolveMissingRuntimeModules(appAsarPath),
  );
  if (missingRuntimeModules.length === 0) {
    // 之前 afterPack 每次都完整 extract/pack app.asar，即使运行时依赖已经齐全也会重复重写。
    // 这会把每次打包固定拉长十几秒到几十秒。先做缺失扫描，只有真的缺包才执行重写流程。
    console.log("[afterPack] runtime modules already complete, skip app.asar rewrite");
    return;
  }
  console.log(`[afterPack] missing runtime modules count=${missingRuntimeModules.length}`);

  // CI 会把 TMPDIR 指到项目内 .tmp，GitLab get_sources/clean 可能在脚本启动前清掉该目录。
  // afterPack 里重写 app.asar 同样依赖 mkdtempSync，必须自己兜底创建父目录，避免后续签名阶段只看到 .app 消失。
  mkdirSync(tmpdir(), { recursive: true });
  const stagingDir = mkdtempSync(resolve(tmpdir(), "zcode-app-asar-"));
  try {
    runTimedSync("afterPack:asar-extract", () =>
      runAsarCommand(["extract", appAsarPath, stagingDir]),
    );

    const stagingNodeModulesDir = resolve(stagingDir, "node_modules");
    mkdirSync(stagingNodeModulesDir, { recursive: true });

    runTimedSync("afterPack:copy-runtime-modules", () => {
      for (const runtimeModule of missingRuntimeModules) {
        const { moduleName, sourceModulePath } = runtimeModule;
        const targetModulePath = resolve(stagingNodeModulesDir, moduleName);

        if (!sourceModulePath) {
          throw new Error(
            `未找到运行时依赖 ${moduleName}，已搜索: ${runtimeModuleLookupRoots
              .map((lookupRoot) => resolve(lookupRoot, "node_modules", moduleName))
              .join(", ")}`,
          );
        }

        // pnpm hoisted 布局下，electron-builder 可能把主包打进 app.asar，
        // 却漏掉它解析时还要去根 node_modules 找的运行时依赖。
        // 之前 require-in-the-middle 漏过 module-details-from-path，这次 @fiahfy/icns 又漏了 pngjs，
        // 最终都会在已安装应用里触发 Cannot find module 并让主进程启动直接崩溃。
        // 这里按 package.json 递归补齐依赖闭包，避免每次只补一个缺失包、上线后再暴露下一个子依赖。
        // 只靠 package.json 显式依赖、本包 node_modules 镜像、files include 都没让它稳定进 asar，
        // 所以在 afterPack 阶段直接重写 app.asar，先把这些运行时包补进去，再交给后续签名和出包。
        mkdirSync(dirname(targetModulePath), { recursive: true });
        rmSync(targetModulePath, { force: true, recursive: true });
        cpSync(sourceModulePath, targetModulePath, { recursive: true });
      }
    });

    await runTimedAsync("afterPack:asar-pack", () =>
      replaceAppAsarFromStaging({
        sourceDir: stagingDir,
        appAsarPath,
        targetPlatformKey: targetPlatform.key,
        runAsarCommand,
      }),
    );
  } finally {
    rmSync(stagingDir, { force: true, recursive: true });
  }
}

async function stripPackagedSourcemapReferences(context) {
  // electron-builder 的 files 规则能排除 .map 文件，但无法删除 JS/CSS 末尾
  // 指向 sourcemap 的注释；afterPack 注入运行时依赖后也可能重新带入第三方 sourceMappingURL。
  // 这里统一清理 app.asar 与 unpacked/extraResources，保证最终发布包不暴露 sourcemap 路径入口。
  await cleanupPackagedSourcemaps({
    appAsarPath: resolveAppAsarPath(context),
    resourcesDir: resolvePackagedResourcesDir(context),
    runAsarCommand,
    runTimedSync,
    runTimedAsync,
    replaceAppAsarFromStaging: ({ sourceDir, appAsarPath }) =>
      replaceAppAsarFromStaging({
        sourceDir,
        appAsarPath,
        targetPlatformKey: targetPlatform.key,
        runAsarCommand,
      }),
  });
}

function assertPackagedNativeResourcePolicy(context) {
  const appAsarPath = resolveAppAsarPath(context);
  const entries = parseAsarListWithPackState(
    runAsarCommandAndReadStdout(["list", "--is-pack", appAsarPath]),
  );
  const violations = findDesktopNativePackageViolations(entries, targetPlatform.key);
  if (violations.length > 0) {
    // supportedArchitectures 允许工作区准备多平台依赖，但安装包只能携带目标平台资源。
    // 之前 Canvas 和 node-pty 的其他平台 native 被同时写进 asar/unpacked，包体被放大数百 MiB。
    throw new Error(`桌面 native 资源边界校验失败:\n- ${violations.join("\n- ")}`);
  }
}

function assertPackagedNodePtyPrebuild(context) {
  const targetBinaryPath = resolvePackagedNodePtyPrebuildPath({
    resourcesDir: resolvePackagedResourcesDir(context),
    platformKey: targetPlatform.key,
  });
  if (!existsSync(targetBinaryPath))
    throw new Error(`node-pty 预编译产物缺失: ${targetBinaryPath}`);
}

/** @type {import("electron-builder").Configuration} */
export default {
  appId: desktopProductIdentity.appId,
  // Linux deb 打包（fpm）会校验 package metadata 中的 homepage、author.email、maintainer。
  // CI 环境下若这些字段缺失会在产物阶段直接失败。这里统一在构建配置补齐，避免依赖外部注入。
  extraMetadata: {
    version: buildMetadata.appVersion,
    zcodeProductFlavor: desktopProductIdentity.flavor,
    homepage: "https://zcode.z.ai",
    author: {
      name: "ZCode",
      email: "dev@zcode.z.ai",
    },
  },
  // macOS 签名阶段会对 Electron Framework 下每个语言包逐个 codesign。
  // 默认全量语言会产生大量 locale.pak 签名调用，显著拉长打包时长。
  // 这里仅保留当前产品必需语言，减少签名文件数并缩短 CI 总耗时。
  electronLanguages: ["en-US", "zh-CN"],
  // pnpm workspace + semver range（如 ^41.0.3）下，electron-builder
  // 有时无法从依赖树里稳定推导出 Electron 版本，导致 bundle 直接中断。
  // 显式写死当前桌面端使用的 Electron 版本，避免打包阶段再做不可靠的猜测。
  electronVersion: "41.0.3",
  electronDownload: {
    // ELECTRON_MIRROR 是 @electron/get 的全局环境变量，会覆盖 dmg-builder 等
    // generic artifact 自己传入的 mirrorOptions，导致 builder 辅助包被错误拼到 Electron runtime 镜像目录。
    // 这里改用 electron-builder 的专用配置，只影响 Electron runtime zip 下载。
    mirror: resolveElectronDownloadMirror(),
  },
  productName: desktopProductIdentity.productName,
  directories: {
    // macOS arm64/x64 CI 可能共享同一个 checkout 并行打包。
    // 输出根目录允许按架构隔离，避免一个 job 清理 dist 时删除另一个 job 正在签名的 .app。
    output: desktopDistDir,
    buildResources: "build",
  },
  files: [
    "out/**/*",
    "package.json",
    // app.asar 会把桌面端运行时 node_modules 一并打进去，依赖包自带的 .map / README
    // 默认也会原样进入安装包。这里统一在主包层做一次裁剪，只移除非运行时文件，LICENSE 继续保留。
    ...PACKAGING_PRUNE_PATTERNS,
    ...createDesktopNativePackagePrunePatterns(targetPlatform.key),
    "!node_modules/@zcode/**",
    "!node_modules/react/**",
    "!node_modules/react-dom/**",
  ],
  asarUnpack: [
    // node-pty 的 target prebuild 还包含 spawn-helper / winpty-agent.exe 等辅助可执行文件，
    // 整个目标目录必须 unpack；其他平台目录已由 files 规则裁剪。
    `node_modules/node-pty/prebuilds/${targetPlatform.key}/**`,
  ],
  beforePack: async (context) => {
    runTimedSync("beforePack:restoreTargetNodePtyPrebuild", () =>
      restoreTargetNodePtyPrebuild({ desktopPackageRoot, targetPlatform }),
    );
    if (context.electronPlatformName !== "win32" || nsisInstallSectionPatched) {
      return;
    }

    nsisInstallSectionPath = resolve(
      dirname(requireFromConfig.resolve("app-builder-lib/package.json")),
      "templates",
      "nsis",
      "installSection.nsh",
    );
    const patchResult = await runTimedAsync("beforePack:patchNsisInstallSection", () =>
      patchNsisInstallSectionFile(nsisInstallSectionPath),
    );
    nsisInstallSectionPatched = true;
    nsisInstallSectionOriginalSource = patchResult.originalSource;
    if (patchResult.changed) {
      // electron-builder 在当前进程内随后才会编译 NSIS；等整个构建进程退出后恢复 node_modules
      // 中的上游模板，避免把一次打包的定制内容永久留在开发依赖里。
      process.once("exit", () => {
        restoreNsisInstallSectionFileSync({
          filePath: nsisInstallSectionPath,
          originalSource: nsisInstallSectionOriginalSource,
        });
      });
    }
  },
  afterExtract: async (context) => {
    // 修复：macOS 重命名阶段会删除归档顶层许可证，必须在 afterExtract 保留目标平台原文。
    const framework = context.packager.info.framework;
    const resources =
      context.electronPlatformName === "darwin"
        ? resolve(context.appOutDir, framework.distMacOsAppName, "Contents", "Resources")
        : resolve(context.appOutDir, "resources");
    await stageElectronNotices(context.appOutDir, resources, framework.version);
  },
  afterPack: async (context) => {
    const actualWindowsTarget =
      context.electronPlatformName === "win32"
        ? resolveElectronBuilderWindowsTarget({
            electronPlatformName: context.electronPlatformName,
            arch: context.arch,
            configuredTargetPlatform: targetPlatform,
          })
        : null;
    await runTimedAsync("afterPack:injectHoistedRuntimeModulesIntoAsar", () =>
      injectHoistedRuntimeModulesIntoAsar(context),
    );
    await runTimedAsync("afterPack:stripPackagedSourcemapReferences", () =>
      stripPackagedSourcemapReferences(context),
    );
    runTimedSync("afterPack:assertPackagedNativeResourcePolicy", () =>
      assertPackagedNativeResourcePolicy(context),
    );
    runTimedSync("afterPack:assertPackagedNodePtyPrebuild", () =>
      assertPackagedNodePtyPrebuild(context),
    );
    if (actualWindowsTarget) {
      await runTimedAsync("afterPack:writeWindowsInstallManifest", () =>
        writeWindowsInstallManifest(context),
      );
    }
  },
  extraResources: [
    { from: resolve(workspaceRoot, noticesFileName), to: noticesFileName },
    ...(targetPlatform.os === "darwin"
      ? [
          {
            // CUA 权限浮窗的吸附数据源（CGWindowListCopyWindowInfo，不需要任何 TCC 权限）。
            // 主进程按 process.resourcesPath 解析；缺失时 watcher fail-open，浮窗仍可用
            // 只是不吸附，所以这里不做存在性断言。
            from: "resources/macos-window-bounds/zcode-window-bounds",
            to: "macos-window-bounds/zcode-window-bounds",
          },
        ]
      : []),
    {
      // 正式包不能依赖仓库目录读取社区、反馈等内置兜底配置。
      // 显式放入 resources/config，与主进程的 process.resourcesPath 解析保持一致。
      from: resolve(workspaceRoot, "config/default.json"),
      to: "config/default.json",
    },
    {
      // Provider Registry 的 ZCode Built-in Config 是静态 Provider/Model 事实的唯一内置来源。
      // 显式随包发布，避免正式 Host 回退到旧 Catalog/Preset hardcode。
      from: builtinProviderConfig.sourcePath,
      to: "config/provider/zcode-builtin.json",
    },
    {
      // 应用图标：打包后放入 resources 目录，主进程通过 process.resourcesPath 加载
      from: "build/icon.png",
      to: "icon.png",
    },
    ...(targetPlatform.os === "linux"
      ? [
          {
            // AppImage 用户级 hicolor 图标安装使用真实 512x512 资源，避免目录标称尺寸和 PNG IHDR 不一致。
            from: "build/icons/512x512.png",
            to: "icon_512x512.png",
          },
        ]
      : []),
    {
      // Windows 独立图标：开发态和打包态都统一走同一套任务栏/窗口图标资源。
      from: "build/icon_windows.png",
      to: "icon_windows.png",
    },
    ...(targetPlatform.os === "win32"
      ? [
          {
            // Windows 托盘图标：Tray 在打包态只能稳定读取 resources 下的独立资源。
            // 这里不复用窗口 PNG，避免通知区域在高 DPI 下退化成模糊缩放图。
            from: "build/icon.ico",
            to: "tray_icon.ico",
          },
        ]
      : []),
    {
      // agent 运行时资产，打包到 resources/glm。
      // 桌面端内置的是 agent 的 JS bundle（glm/zcode.cjs，由 prepare:agent-bundle 生成），
      // Host 进程用 app 自带的 Electron Node runtime（ELECTRON_RUN_AS_NODE）执行 `zcode.cjs app-server --stdio`，
      // 不再随包内置独立 Node 二进制。远端 SSH/WSL 仍走原生二进制（无 Electron）。
      from: `bundled-agents/${targetPlatform.key}/glm`,
      to: "glm",
      filter: ["**/*", "!**/*.map"],
    },
    {
      // agent shell 之前完全依赖宿主系统 PATH，GUI 启动时经常拿不到用户自己装的 rg。
      // 这里把 ripgrep 作为桌面端内置 runtime tool 打进 resources/tools，
      // 后续 host/server 把该目录追加到 PATH；用户版本优先，缺失时再由随包 rg 兜底。
      from: `bundled-tools/${targetPlatform.key}/ripgrep`,
      to: "tools/ripgrep",
      filter: ["**/*"],
    },
    ...nativeSearchReleasePlan.extraResourceToolIds.map((toolId) => ({
      from: `bundled-tools/${targetPlatform.key}/${toolId}`,
      to: `tools/${toolId}`,
      filter: ["**/*"],
    })),
  ],
  // postinstall 会先优先复用 node-pty 自带的 Windows 预编译产物，其他平台再按需 electron-rebuild。
  // 打包阶段统一复用安装时准备好的原生文件，避免 electron-builder 再触发一轮不受控的本地编译。
  npmRebuild: false,
  // OAuth deep link 协议注册（macOS 打包后需要 Info.plist 中声明 CFBundleURLTypes）
  protocols: [
    {
      // 协议处理器的展示名之前使用小写 scheme，打包产物里的协议描述无法体现产品名。
      // 展示名跟随安装包身份；scheme 仍保持 zcode，因此两个应用中最后注册者会成为默认 handler。
      name: desktopProductIdentity.productName,
      schemes: ["zcode"],
    },
  ],
  mac: {
    target: ["dmg", "zip"],
    category: "public.app-category.developer-tools",
    artifactName: buildDesktopArtifactName("mac"),
    extendInfo: {
      NSAppleEventsUsageDescription: `${desktopProductIdentity.productName} needs Apple Events access to coordinate local automation workflows with user-approved desktop apps.`,
    },
    // 预签名脚本走的是原生 codesign，要求完整的 "Developer ID Application: ..." 身份串；
    // 但 electron-builder 的 mac.identity 在 26.x 下会拒绝带此前缀的名字。
    // 这里仅对 electron-builder 侧做前缀归一化，避免本地预签名和最终 .app 签名互相打架。
    // z-code 之前只有本地未签名打包配置，CI 即使注入了证书变量，
    // electron-builder 也不会自动切到 hardened runtime / entitlement 这套发布参数。
    // 这里显式收拢到环境开关，保证本地开发不被签名配置绑死，CI 发布时再按需打开。
    identity: shouldEnableMacSigning ? macSigningIdentity : null,
    // macOS 产物采用“build 阶段签名 + 独立公证阶段”的两段式流水线。
    // 如果这里不显式关闭 electron-builder 内置 notarize，它会在 build 阶段读取 Apple 凭据后直接尝试公证，
    // 并强制要求 APPLE_APP_SPECIFIC_PASSWORD，导致 build 还没产出 DMG 就提前失败。
    notarize: false,
    hardenedRuntime: shouldEnableMacSigning,
    gatekeeperAssess: false,
    entitlements: "build/entitlements.mac.plist",
    entitlementsInherit: "build/entitlements.mac.inherit.plist",
    // runtime 可执行文件已在打包前的独立预签名阶段完成签名，
    // electron-builder 在签主 app 时若继续深度扫描这些目录，会显著拉长 macOS codesign 时长。
    // 这里按“任意前缀 + Contents/Resources”匹配绝对路径，避免 ^Contents/... 在 CI 中无法命中。
    // 命中后可跳过已预签名目录的重复签名/遍历，同时保留主 app 与框架签名。
    // CUA Helper 在独立 job 中已完成 Developer ID 签名和 notarization staple；
    // electron-builder 若再次签名嵌套 Helper 会改变 CDHash，使最终用户包中的 staple 失效。
    signIgnore: [
      "[/\\\\]Contents[/\\\\]Resources[/\\\\]glm([/\\\\]|$)",
      "[/\\\\]Contents[/\\\\]Resources[/\\\\]tools([/\\\\]|$)",
    ],
  },
  win: {
    target: ["nsis"],
    artifactName: buildDesktopArtifactName("win"),
  },
  linux: {
    target: ["AppImage", "deb", "rpm", "pacman"],
    artifactName: buildDesktopArtifactName("linux"),
    // desktop 包名是 scoped package（@zcode/desktop），electron-builder 默认会把
    // Linux executable/Icon 推成 @zcodedesktop。部分桌面环境无法按这个 icon name 命中
    // hicolor 图标，最终回退成系统齿轮。这里固定成稳定的小写名称，让 Icon=zcode
    // 与 /usr/share/icons/hicolor/*/apps/zcode.png 保持一致。
    executableName: desktopProductIdentity.linuxExecutableName,
    category: "Development",
    maintainer: "ZCode <dev@zcode.z.ai>",
  },
  deb: {
    // 生产版与 Preview 必须是两个 dpkg package；只改可执行名仍会让安装器把另一版本当成升级替换。
    packageName: desktopProductIdentity.linuxPackageName,
  },
  pacman: {
    // 与 deb/rpm 保持相同的 flavor 隔离，避免 Preview/Production 被 pacman 当作同一包覆盖。
    packageName: desktopProductIdentity.linuxPackageName,
    // 显式列出 Arch 官方仓库可解析的 Electron 运行时依赖，替换 electron-builder
    // 陈旧默认集合，避免安装阶段因已移除包名直接失败。
    depends: PACMAN_RUNTIME_DEPENDENCIES,
    // Electron Builder 默认把 pacman target 命名为 .pacman；Arch 原生包的标准扩展名是 .pkg.tar.zst。
    artifactName: buildDesktopArtifactName("linux", "pkg.tar.zst"),
  },
  rpm: {
    // 与 deb 同一约束：生产版与 Preview 必须是两个独立 rpm 包，否则 dnf 会把另一 flavor 当成升级替换。
    // rpm 面向 RHEL 8+（glibc 2.28）分发；整包 glibc 下限由 node-pty prebuild 与 bfs/ugrep 抬到 2.28，
    // Electron 41 主二进制只引用到 2.25，不会更高。fpm 产 rpm 需要构建机提供 rpmbuild 与 xz。
    packageName: desktopProductIdentity.linuxPackageName,
    // electron-builder 的 rpm 默认 Requires（gtk3/nss/libXtst 等）不包含 Electron ELF 实际
    // DT_NEEDED 的 mesa-libgbm 与 alsa-lib；rockylinux:8 最小化容器实测装完后启动报
    // libgbm.so.1 缺失。这里用 fpm 追加 -d（在默认 Requires 之后累积），不能用 depends——
    // depends 会整组替换默认 Requires 集。
    fpm: ["-d", "mesa-libgbm", "-d", "alsa-lib"],
  },
  dmg: {
    // 当前安装包携带的运行时资源（尤其 agent node_modules）体积已超过默认 DMG 估算值。
    // 之前依赖自动容量时，生成的 DMG 挂载卷只有约 1.9Gi，复制 .app 过程中会因为空间耗尽
    // 丢失 Electron Framework 主二进制，安装后启动直接报 DYLD Library missing。
    // 显式放大 DMG 容量，避免拷贝截断导致的“Framework 目录存在但核心文件缺失”。
    size: "3200m",
    // 使用自定义安装背景图。
    background: "build/dmg_background.png",
    // 安装盘图标统一使用安装专用素材，避免复用应用图标导致安装识别度不足。
    icon: "build/icon_installer.icns",
    contents: [
      // 实验性调整：为隐藏资源文件显式指定图标坐标，尽量把它们移到角落区域。
      { x: 640, y: 56, type: "file", path: ".background.tiff" },
      { x: 640, y: 56, type: "file", path: ".VolumeIcon.icns" },
      { x: 130, y: 220 },
      { x: 410, y: 220, type: "link", path: "/Applications" },
    ],
  },
  nsis: {
    oneClick: false,
    allowToChangeInstallationDirectory: true,
    // Windows 安装流程使用独立安装图标，和应用运行时图标解耦。
    installerIcon: "build/icon_installer.ico",
    uninstallerIcon: "build/icon_installer.ico",
    installerHeaderIcon: "build/icon_installer.ico",
  },
  detectUpdateChannel: false,
  publish: {
    provider: "generic",
    // 当前 OSS/CDN 对多 Range 请求返回 206，但 Content-Type 仍是 application/x-msdownload，
    // electron-updater 会因缺少 multipart/byteranges 直接回退整包下载。关闭 multiple range 后仍走差分，
    // 只是按单 Range 顺序拉取差异块，避免 Windows 用户更新时从约 15MB 退化成 300MB+ 全量包。
    useMultipleRangeRequest: false,
    // 新客户端运行时使用服务端 manifest provider；这里仅保留 electron-builder 必需的
    // generic publish 占位，避免打包产物继续携带可配置的旧 stable feed。
    url: "http://localhost:8081",
  },
};
