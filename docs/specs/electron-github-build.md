# Electron GitHub Actions 构建

## 目标与边界

- GitHub Actions 只构建 macOS ARM64 和 Windows x64（64 位，不是 ia32）。
- 手动触发，或推送 `v*` tag 时构建；每个平台使用独立的 GitHub 托管 runner。
- 复用 `prepare:desktop-runtime`、Desktop production build 和 `bundle:desktop`，使用当前默认的 TypeScript Agent；不切换 Rust runtime。
- Node 版本来自 `mise.toml`，pnpm 版本来自根 `package.json`，依赖安装使用 frozen lockfile。
- 使用仓库中的 native-search 归档和公网依赖源，跳过不进入安装包的 remote assets；不依赖私有 runner、共享目录或内网镜像。
- 产物仅上传 Actions artifacts：macOS `.dmg`、Windows `.exe`，以及各自的 SHA-256 校验文件。缺少安装包时失败，不创建 Release 或发布更新源。

## 所有者与执行顺序

Workflow 拥有平台矩阵、工具链安装和产物上传。现有 Desktop 脚本拥有 runtime 准备、生产构建、安装包内容与依赖校验。macOS 签名脚本只拥有本次 job 的临时钥匙串、资源签名和公证操作，不更改应用状态或用户钥匙串。

```text
同一提交
  ├─ macOS ARM64 runner → install → runtime → production build
  │    → 临时签名 / 导入证书并预签资源 → bundle 校验
  │    → 有凭据时公证并 staple DMG → SHA-256 → upload
  └─ Windows x64 runner → install → runtime → production build
       → bundle 校验 → SHA-256 → upload
```

两个平台互不取消；任一 job 失败不上传该平台的成功产物。缓存只保存依赖下载，不复用历史构建输出。

## 签名规则

- Windows 默认不做发布签名。旧私有 CI 的 vsigntool 依赖自建机器的工具及配置，不能假定 GitHub 托管 runner 可用。
- macOS 全部未配置 Apple Secrets 时，用 ad-hoc 临时签名生成 ARM64 测试包，不公证。仍使用空的隔离钥匙串，防止 electron-builder 意外匹配本机其他证书。它不等于受信任的 Developer ID 签名。
- 以下五项 Secrets 全部配置时，导入独立临时钥匙串，并从导入结果选择唯一有效的签名身份：`APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64`、`APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD`、`APPLE_ID`、`APPLE_PASSWORD`、`APPLE_TEAM_ID`。其中 `APPLE_PASSWORD` 是 Apple 专用密码。
- 只配置部分 Secrets、证书不可用、签名失败、公证不是 Accepted 或 staple 验证失败，都必须使 job 失败，不降级输出测试包。
- 签名前处理 `signIgnore` 排除的 `glm` 和 `tools` 中的 Mach-O；其余应用与原生模块由 electron-builder 签名。临时钥匙串和证书在退出时清理。
- `bundle:desktop` 显式使用 `--publish never`，避免 tag CI 触发 electron-builder 隐式发布。
- 安装包完成签名、公证后才生成校验和；不上传未公证 ZIP、钥匙串、证书或 updater 元数据。
- macOS 的 node-pty 预编译 `spawn-helper` 在所有 ASAR 重打包完成后、签名前补齐可执行权限。npm 归档中的该文件可能是 0644，ASAR 解包也会丢失 unpacked 文件的执行位；不能依赖安装后运行时修改已签名应用来修复终端。

## 验收

1. 两个平台的 bundle dry-run 使用正确架构且禁止发布；ia32 明确拒绝。
2. macOS 无凭据、完整凭据、部分凭据以及签名/公证失败路径有脚本测试；验证退出清理和错误不被吞掉。
3. workflow 通过 YAML / Actions 语法检查；每个平台只有一个 runtime 准备和 production build 阶段。
4. 每个平台打包后，用包内 Electron Node runtime 加载关键依赖并实际创建 PTY，验证原生模块和 spawn-helper 可执行，不启动应用业务或访问用户设置。本机执行 macOS ARM64 实际打包及产物验证；Windows 原生构建以 Actions 实际运行结果为准。
5. 执行 `pnpm typecheck`、`pnpm lint` 和架构检查，明确区分已有警告与本次失败。

## 参考

- [SiYuan CD](https://github.com/siyuan-note/siyuan/blob/master/.github/workflows/cd.yml)：原生平台矩阵、依赖缓存和按平台收集产物。
- 旧私有 CI 的 macOS 流程：临时钥匙串、资源预签名、打包后独立公证；这里只移植当前仓库所需步骤。
