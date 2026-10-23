import { resolve } from "node:path";
import { configDefaults, defineConfig } from "vitest/config";

const processLifecycleTests = [
  "packages/services/test/processTreeTerminator.test.ts",
  "packages/services/test/zcodeAgentService.test.ts",
  "packages/services/test/zcodeAgentService.v4.test.ts",
  "packages/services/test/zcodeAgentProcessManager.test.ts",
  "packages/services/test/zcodeStdioTransport.test.ts",
];
const forkIsolatedTests = [
  ...processLifecycleTests,
  // 真实显示进程树和独立配置读者都在 5 秒内等待启动；并发调度延迟曾触发截止。
  // 放入串行组保留真实进程和原时限，避免把启动排队误判为资源隔离或读锁失败。
  "packages/desktop/test/r3RemoteProcess.test.ts",
  "packages/provider-node/test/personal-config-read-lock.test.ts",
  // 原生 fs.watch 集成在批量并发时曾等不到通知；串行验证真实接线，去重逻辑由受控事件单测覆盖。
  "packages/services/src/gen-ui/adapters/stateStorage.test.ts",
  // electron-builder 下载 NSIS 时把进程 stdio 传给子进程，不能使用 threads 的虚拟 stdio。
  "packages/desktop/test/windowsInstallerCompile.test.ts",
  "packages/desktop/test/cdnPreloadScript.test.ts",
  "packages/desktop/test/desktopRuntimeEnv.test.ts",
  "packages/desktop/test/e2eDesktopBuildCache.test.ts",
  "packages/desktop/test/e2eSummarySpecCoverage.test.ts",
  "packages/desktop/test/migrateStableFeedScript.test.ts",
  "packages/desktop/test/pluginE2EAdmissionScripts.test.ts",
  "packages/desktop/test/uploadOssScript.test.ts",
  // 发布脚本会启动真实 Node/工具子进程，批量 threads 下曾打穿 30s；沿用发布类串行隔离。
  "packages/desktop/test/notarizeMacosScript.test.ts",
  "packages/desktop/test/publishReleaseScript.test.ts",
  // 选源集成测试连续启动 Git、Bash 和 Node；并发启动排队会打穿 30s，沿用发布类串行隔离。
  "packages/web/test/remoteFrontendBuildSource.test.ts",
  // Sharp 闭包装配后要启动真实 Node 加载原生库；并行热启动曾触发其 15s 子进程限时。
  "packages/desktop/test/dev-cua-plugin-runtime.test.ts",
  // 全量并发时真实心跳与嵌套构建会被调度延迟打穿；单独运行均通过，沿用串行组保留原时限。
  "packages/desktop/test/webRemoteControlTransport.test.ts",
  "packages/web/test/webRemoteControlTransport.test.ts",
  "packages/provider-node/test/builtin-provider-build.test.ts",
  "packages/provider-node/test/builtin-provider-test-config-sync.test.ts",
  "packages/server/test/remoteDeploy.test.ts",
  // Bugfix: 该集成测试会启动数百个真实 Git 子进程；放在普通 threads 并发组会让
  // macOS 进程启动队列饥饿，表现为 git init 长时间停在 dyld_start。
  "packages/services/test/gitService.test.ts",
  // Bugfix: checkpoint 集成测试同样会连续创建真实 Git 仓库；related 全量并发时会因
  // 子进程与磁盘调度饥饿超过 30 秒，隔离串行后保持聚焦运行时的真实耗时口径。
  "packages/services/test/gitCheckpointService.test.ts",
  "packages/services/test/mcpSyncService.test.ts",
  "packages/services/test/providerRuntimeResolver.test.ts",
  "packages/services/test/runtimeToolResolver.test.ts",
  // 该集成测试会真实启动 tsx、Supervisor 与多代 Core；放在普通 threads 中时，
  // 并行 worker 的进程启动压力会让 15 秒 ready barrier 偶发超时。
  "packages/zcode-server-cli/test/runtimeUpdateIntegration.test.ts",
  // Bugfix: pdfjs-dist legacy build 在 threads worker 内偶发原生崩溃（0xC0000005，
  // 测试自身通过、worker 收尾时崩，会把整场全量跑拦腰截断）。隔离到 fork 池后崩溃
  // 波及面收敛到本文件，其余测试结果可完整收集；崩溃根因见
  // docs/windows-test-compatibility.md。
  "packages/ui/test/pdfJsCMapSupport.test.ts",
];

export default defineConfig({
  // 测试固定注入事件上报端点，使遥测上报链路在单测中可验证；真实构建由 scripts/build-time-config.mjs 决定。
  define: {
    __ZCODE_TELEMETRY_REPORT_ENDPOINT__: JSON.stringify("https://zcode.z.ai/api/v1/event/report"),
  },
  resolve: {
    alias: {
      "@": resolve(__dirname, "packages/ui/src"),
      "@desktop": resolve(__dirname, "packages/desktop/src"),
      "@web": resolve(__dirname, "packages/web/src"),
      // 间接 import desktopNetworkTelemetry 等的单测无需真实 ARMS；需 vi.fn 的用例在文件内 vi.mock 覆盖
      // Bugfix: 具体的 /browser 子路径必须排在包根别名前，否则会被根别名拼成不存在的 stub.ts/browser。
      "@arms/rum-electron/browser": resolve(
        __dirname,
        "packages/desktop/test/mocks/armsRumElectronStub.ts",
      ),
      "@arms/rum-electron": resolve(
        __dirname,
        "packages/desktop/test/mocks/armsRumElectronStub.ts",
      ),
    },
  },
  test: {
    setupFiles: ["./vitest.setup.ts"],
    // 官方 affected 图不额外追踪 setupFiles；公共 setup 必须触发全量选择，再由入口跳过 UT 并成功退出。
    forceRerunTriggers: [
      ...configDefaults.forceRerunTriggers,
      // 默认配置 glob 的尾部 /** 在当前版本不能匹配部分配置文件，补充文件模式。
      "**/{vitest,vite}.config.*",
      "**/vitest.setup.ts",
    ],
    // 修复原因：全量 run 的 stdout 会把**通过**测试里的 console 输出一并转发——services 层
    // 用 createServiceLogger 的用例在绿态下也要刷几千行多行日志，全量输出因此膨胀到
    // ~3.3MB，超过 CI/工作流门禁对单条命令 256KB 的 stdout 上限，导致门禁整条命令被判
    // 「无法执行」而非测试失败。passed-only 只隐藏通过测试的 console，失败用例的日志与
    // 断言 diff 原样保留（诊断能力不降级）；全绿时输出收敛到 ~140KB。
    silent: "passed-only",
    // 修复原因：全量单测并发跑 Git、zip、Electron 模块导入时，默认 5s 超时在低配或高负载机器上
    // 容易被提前打穿（spawn git 子进程、模块 import 变慢 → flaky 超时）；中断后的文件句柄清理在
    // Windows 上还会触发 EPERM/ENOTEMPTY。此前只给 Windows 放宽，但 macOS/Linux 低配机同样会因
    // 内存抖动（swap thrashing）拖慢 spawn，故统一降低并发并放宽超时。
    hookTimeout: 30_000,
    maxWorkers: 4,
    testTimeout: 30_000,
    // 修复原因：900+ 文件全部使用隔离 fork 时，macOS 会在尾部因连续创建进程而随机
    // 启动 worker 超时。普通用例使用 threads；会修改 cwd/umask、回收 POSIX 进程树或
    // 密集 spawn shell 的用例最后串行 fork，避免跨用例误杀、进程级状态竞争和系统
    // spawn 队列饥饿。
    projects: [
      {
        extends: true,
        test: {
          name: "unit",
          include: [
            "packages/*/test/**/*.test.ts",
            // 架构治理要求 managed 模块自带 contract.test.ts（见 architecture-policy.yaml），放在模块目录内。
            "packages/*/src/**/contract.test.ts",
            // Gen UI 目录回归位于源码旁，默认入口也必须执行，避免只在手动定向运行时生效。
            "packages/shared/src/node/genUiPaths.test.ts",
            "packages/services/src/gen-ui/**/*.test.ts",
            "scripts/assert-safe-cua-helper-zip.test.mjs",
            "scripts/pre-push-rust-cli.test.mjs",
            "scripts/ci/ci-telemetry-keyword-guard.test.mjs",
          ],
          exclude: ["packages/desktop/test/e2e/**", "packages/zcode-cua/**", ...forkIsolatedTests],
          // Node 24/macOS 的 threads 全量运行可触发 V8 SIGSEGV；诊断时切换进程隔离，测试范围不变。
          pool: process.env.ZCODE_TEST_POOL === "forks" ? "forks" : "threads",
          maxWorkers: process.env.ZCODE_TEST_POOL === "forks" ? 2 : 4,
          sequence: { groupOrder: 0 },
        },
      },
      {
        extends: true,
        test: {
          name: "fork-isolated",
          include: forkIsolatedTests,
          pool: "forks",
          fileParallelism: false,
          maxWorkers: 1,
          sequence: { groupOrder: 1 },
        },
      },
      "packages/zcode-cua/vitest.config.ts",
    ],
    coverage: {
      provider: "v8",
      include: ["packages/*/src/**/*.ts"],
    },
  },
});
