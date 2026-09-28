# ZCode Rust App launcher

启动已安装的 **ZCode 3.14.3**，用 Rust 执行本地 Agent。App 原文件保持不变。兼容基线是正式产品 tag `v3.14.3`（`ab4d5e6ba0`），不是开源分支的 Desktop 代码。

## 使用

发布后：

```sh
npx @mbears/zcode-rs@beta app doctor
npx @mbears/zcode-rs@beta app launch
npx @mbears/zcode-rs@beta app status
```

先完整退出 ZCode。`launch` 自动把对应平台 binary 安装到 `~/.zcode/runtimes/rust-launcher`，从专用目录启动原 App；不会中断已有 App。`status` 展示最近启动和桥接回执，不将历史 PID 当成存活保证。需要 Node >= 22.16 来运行安装命令；之后 App 使用固定安装目录，不依赖 npm 缓存。

支持 `--app <ZCode.app 或可执行文件>`、`--runtime-dir <目录>`。macOS 自动查找 `/Applications/ZCode.app`；Windows 查找常用安装目录；Linux 需要已解包的安装目录，通过 `--app` 指定 executable，不直接支持 AppImage。

从原图标正常打开仍然是内置 CLI。回退方法：完整退出当前 App，再从原图标启动。不会给原图标安装永久切换设置。未知 App 版本拒绝启动；升级 App 后先等待相应兼容版本。远端 SSH/WSL runtime 不随本地切换。

工作区路径当前要求使用真实路径，不能使用指向同一目录的符号链接别名。3.14.3 Host 不显式传原始 `--cwd`，这类路径可能导致 Rust workspace identity mismatch。已发现该限制的测试记录必须随发布保留。

Rust 的功能范围见仓库 `apps/zcode-cli-rust/README.md`；能启动不代表已实现内置 CLI 的全部功能。平台包提供下载选择，不代表所有平台已经通过实机验收。

## 本地构建与打包

在仓库根目录：

```sh
CARGO_INCREMENTAL=0 cargo build --release --locked --manifest-path apps/zcode-cli-rust/Cargo.toml --bin zcode-cli-rust
node apps/zcode-cli-rust/npm/bin.mjs app launch --binary apps/zcode-cli-rust/target/release/zcode-cli-rust
node scripts/pack-zcode-cli-rust-npm.mjs --binary apps/zcode-cli-rust/target/release/zcode-cli-rust
```

Windows binary 带 `.exe`。交叉构建产物用 `--platform` 指定目标，必须在对应 OS 上验证后发布。输出在 `dist-release/rust-npm`，含入口包及所选平台包的 `.tgz`。平台包直接携带 executable、SHA-256 manifest 与许可证，无 postinstall 下载或本机编译。

默认入口包为 `@mbears/zcode-rs`，平台包为 `@mbears/zcode-rs-<platform>`；例如 macOS ARM64 的 0.1.0 产物是 `mbears-zcode-rs-0.1.0.tgz` 和 `mbears-zcode-rs-darwin-arm64-0.1.0.tgz`。如需其他发布归属，追加 `--scope @your-scope`，包名仍为 `zcode-rs`。安装后的命令行入口为 `zcode-rust`。

发布时先发布已经验证的平台包，再发布入口包，使用 `npm publish <tgz> --access public --tag beta`。所有包版本一致，入口 optionalDependencies 固定精确版本；不支持的平台或漏装 optional dependencies 会给出明确错误。构建脚本不会执行发布。

例如发布 macOS ARM64 的 0.1.0：

```sh
npm publish ./dist-release/rust-npm/mbears-zcode-rs-darwin-arm64-0.1.0.tgz --access public --tag beta --registry=https://registry.npmjs.org/
npm publish ./dist-release/rust-npm/mbears-zcode-rs-0.1.0.tgz --access public --tag beta --registry=https://registry.npmjs.org/
```

## 启动边界

3.14.3 会优先从 Host cwd 向上查找 `apps/zcode-cli/packages/cli/dist/zcode.cjs`。安装器把桥接文件放到自己的启动目录，通过这个版本已有的路径选择进入。存储准备仍由原版 CLI 在 Host Worker 内完成；正式会话由桥接启动 Rust，透明转发 stdio，保留进程组与 Host 环境。不是公开稳定的插件接口，因此严格绑定 App 版本，并要求真实安装包回归。
