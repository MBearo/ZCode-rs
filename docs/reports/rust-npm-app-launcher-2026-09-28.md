# Rust npm App launcher 验收记录

## 基线

- 已安装原版 ZCode **3.14.3**，macOS ARM64，Electron 41.0.3；没有构建或修改当前开源分支的 App。
- App 源码取原产品仓库 tag `v3.14.3`，commit `ab4d5e6ba0bc1d7c684d4159427fc6a1d6cf58c9`。冻结 resolver 测试记录了文件路径、完整源文件 SHA-256 和提取的函数。
- Rust 来自当前 Cargo workspace，版本 0.1.0。Node 24.14.0 / pnpm 10.33.2。
- 本地产物由开发工作区构建，包含开工前已有的插件 schema 改动；这些无关改动未纳入本次提交。本次产物用于验收，没有发布 npm。

## 已执行的检查

| 检查                                                                                                 | 结果                                      |
| ---------------------------------------------------------------------------------------------------- | ----------------------------------------- |
| `node scripts/check-workspace-freshness.mjs`                                                         | 通过                                      |
| `cargo build --release --locked --manifest-path apps/zcode-cli-rust/Cargo.toml --bin zcode-cli-rust` | 通过，设置 `CARGO_INCREMENTAL=0`          |
| `pnpm test:rust-npm`                                                                                 | 13 / 13 通过                              |
| `pnpm typecheck`                                                                                     | 通过                                      |
| `pnpm lint`                                                                                          | 0 错误，70 条已有警告；新增文件没有警告   |
| `pnpm architecture:check --changed`                                                                  | 0 违规，0 新增违规                        |
| 改动文件 `oxfmt --check`、`git diff --check`                                                         | 通过                                      |
| npm pack + 全新目录离线安装两份 tgz                                                                  | 通过，`--ignore-scripts` 下无需下载或编译 |
| 消费者目录 `npm exec --offline -- zcode-rust app doctor`                                             | 识别原版 App、native 0.1.0、darwin-arm64  |
| 已有 App 时 `app launch`                                                                             | exit 1，提示完整退出；未创建本次启动目录  |

自动化测试覆盖原版 packaged resolver、存储 Worker、透明字节流、EOF、退出码、信号、桥接被强杀后的子进程 EOF、损坏 binary、并发安装、独立启动目录、含空格路径、ASAR 元数据读取、错误版本、环境净化，以及 App 改写进程标题后的运行实例检测。

## 原版 App 端到端

使用独立 HOME、`ZCODE_DESKTOP_HOME_DIR`、Electron userData/sessionData、数据库与 Rust 目录；模型为本机 Chat Completions fixture。测试 harness 直接调用 launcher 模块启动隔离实例，以便不关闭用户已运行的原版 App。产品 CLI 的已有实例拒绝行为另行验证。

1. 通过桥接启动原版 App，回执包含 `storage-bridge-loaded` 和 `agent-started`；实际进程树是原 Host → JS bridge → Rust。
2. 在 App UI 发出 `RUST_LAUNCHER_3143_OK`，渲染 `answer: RUST_LAUNCHER_3143_OK`。
3. 退出测试 App，从不包含桥接入口的目录启动同一安装包，用原内置 Node 打开同一数据库、同一任务。历史可见，`NODE_ROLLBACK_3143_OK` 获得完整回复。
4. 退出并重新以最新管道桥接启动 Rust，同一任务显示两轮历史，`RUST_RESUME_3143_OK` 获得完整回复。
5. 发出 `use tool RUST_READ_3143_OK`，UI 展示 Read / note.txt 并完成回复。隔离数据库里的工具状态为 `completed`，输出包含测试文件的 `RUST_BRIDGE_READ_TOOL_OK`。
6. 使用离线安装的 npm 包执行 `binarySource`、`installBinary`、`prepareLaunch`，将临时消费者的整个 `node_modules` 移走后，再启动原版 App。历史恢复成功，`PACKAGED_WITHOUT_NPM_CACHE_OK` 获得完整回复；Rust executable 位于稳定的版本目录。
7. 退出测试 App 后桥接记录 `agent-exited` / code 0，确认测试 Rust 和桥接进程已退出。

初次验收（更名前）的打包产物为 `dist-release/rust-npm/zcode-rust-0.1.0.tgz` 和 `zcode-rust-darwin-arm64-0.1.0.tgz`。native SHA-256：`03303aebebd2e24cd46df1ef39513a430861c8683ede8ca9a816a54e04e5eaad`。两份包的文件清单仅包含启动器或 executable、元数据及许可证，不携带测试、账号或开发机配置。

## npm 包名更新验收

入口包改为 `@mbears/zcode-rs`，平台包为 `@mbears/zcode-rs-<platform>`，版本仍为 0.1.0。打包脚本从入口 manifest 读取包名，`--scope` 只覆盖 scope。命令行入口仍为 `zcode-rust`。

- 基于包含上游同步提交 `ed9f925` 的源码重新完成 release 构建，native SHA-256 为 `e48506181d5897bcc14640d7bb0c586a0cef1576837cb344138ba6113ac3cbe9`。
- 默认打包生成 `dist-release/rust-npm/mbears-zcode-rs-0.1.0.tgz` 与 `mbears-zcode-rs-darwin-arm64-0.1.0.tgz`，文件清单只含启动器或二进制、元数据及许可证。
- 在全新消费者目录通过 `npm install --offline --ignore-scripts` 安装两份 tgz；入口包的六个平台依赖均使用相同名称前缀和精确版本。安装器解析正确的平台包、校验并复制 binary，执行 `--version` 返回 0.1.0。
- `npx --offline @mbears/zcode-rs@0.1.0 app doctor` 通过。用临时 `@rename-check` scope 重复打包、离线安装和 doctor 检查也通过；临时产物已清理。
- `pnpm test:rust-npm` 13/13 通过；`pnpm typecheck`、架构检查、改动文件格式检查及 `git diff --check` 通过；Lint 为 70 warning、0 error。

本轮验证包名与打包链路，没有发布 npm，也没有重新执行上面的原版 App UI 端到端验收或 Windows/Linux 验收；不将其视为完整 runtime 的发布验收。

## npm 公开发布验收

2026-09-28，经用户授权，从源码提交 `2a9920c` 发布以下公开包：

- 入口包：`@mbears/zcode-rs@0.1.0`。
- macOS Apple Silicon 平台包：`@mbears/zcode-rs-darwin-arm64@0.1.0`。

两次 `npm publish --access public --tag beta` 均成功。Registry 实际为两包同时建立 `beta` 和 `latest` 标签，均指向 `0.1.0`；尝试移除平台包的 `latest` 时返回 HTTP 400，最终保留并核实了这两个标签。

- 匿名读取两个版本的 metadata、下载公开 tarball 均成功，SHA-512 integrity 与本地发布包完全一致。
- 入口包发布后曾出现完整包索引 HTTP 404；后续索引恢复，并在空用户配置、全新 npm 缓存和消费者目录下通过 `npm install @mbears/zcode-rs@beta --ignore-scripts --no-audit --no-fund`，实际安装入口包和对应平台包各一份。
- 公开安装的 native SHA-256 为 `e48506181d5897bcc14640d7bb0c586a0cef1576837cb344138ba6113ac3cbe9`，与本地 release 构建一致。
- 消费者目录执行 `node_modules/.bin/zcode-rust app doctor` 成功，识别已安装 ZCode 3.14.3、平台 `darwin-arm64` 和 runtime 0.1.0；临时消费者目录及缓存已清理。
- 本次只发布并验证 macOS ARM64 平台包；其余五个平台依赖尚未发布。本次没有再次启动原版 App 或运行完整 UI 端到端验收。

## Windows x64 补充验收

产物来自源码提交 `72ebed94751c4c65613b571e1ee2671d1307eee3`，由 [Windows CI](https://github.com/MBearo/ZCode-rs/actions/runs/36379599843) 在 Windows Server 2022 x64、Node 24.14.0、Rust 1.95.0 上构建。target 为 `x86_64-pc-windows-msvc`，使用 `--release --locked` 和 `RUSTFLAGS="-C target-feature=+crt-static"`。

- 首次 Windows 测试发现配置用例硬编码 POSIX 路径，实际 Windows 路径为反斜杠。将用例改为各系统真实绝对路径后，本地目标用例通过，Windows 协议/领域测试 **171/171** 通过；没有修改配置解析的运行时行为。
- Windows launcher 测试 **9 通过、4 跳过、0 失败**；跳过项为 macOS 进程名检测及 POSIX 进程/信号用例。本地 macOS launcher 测试仍为 **13/13**。
- Windows 上从 tgz 安装到全新消费者目录，通过平台包选择、PE x64 检查、SHA-256、稳定目录复制和真实 exe `--version`（0.1.0），并通过带空格路径的 `app doctor` 元数据 fixture。
- 平台包 `@mbears/zcode-rs-win32-x64@0.1.0` 仅含 executable、`runtime.json`、`package.json` 和许可证；压缩后 15,318,806 字节。native SHA-256 为 `d0704f4b8587e77e8d5e0740117deecd819eb26438b20cd02ffb52c63df1886a`。
- CI 的 `dumpbin` 及本地 PE 导入表复核均通过，仅依赖 Windows 系统 DLL，无 VC runtime 或额外压缩库 DLL 依赖。
- 本地 `pnpm typecheck`、架构检查、格式检查及 `git diff --check` 通过；Lint 70 warning、0 error。Windows Rust release 构建有 2 条已有平台条件代码的 unused 警告。
- npm publish dry-run 通过；浏览器身份验证后，`@mbears/zcode-rs-win32-x64@0.1.0` 公开发布成功，`beta` 与 `latest` 均指向 0.1.0。入口包保持已发布的 0.1.0，无需重新发布。
- 匿名读取版本 metadata 和下载公开 tarball 成功，SHA-512 integrity 与 CI 产物一致。发布初期完整包索引返回 HTTP 404，首次 Windows 公开安装未拉到 optional 平台包；索引恢复后，[公开安装验证](https://github.com/MBearo/ZCode-rs/actions/runs/36381809932) 通过：Windows 全新消费者目录、空用户配置和全新缓存从 npm 按 `@mbears/zcode-rs@0.1.0` 安装，自动获得 Windows x64 平台包，并通过相同的 native SHA-256、稳定目录安装、exe 版本和 `app doctor` fixture 检查。

Windows 这轮没有运行真实 ZCode 桌面 UI、Host taskkill 或真实会话端到端测试；`app doctor` 的 App 元数据 fixture 不作为这些行为通过的证据。

## 已确认的限制

- 仅绑定 3.14.3。原图标继续使用原内置 CLI；每次需要 Rust 时使用 launcher。远端 runtime 不随本地切换。
- 原版 Host 不传原始 `--cwd`。`/var` 与 `/private/var` 等符号链接别名可触发 workspace identity mismatch；保留别名和真实路径两份 workspace 还可能导致同一物理目录的 owner 冲突。首版只支持真实工作区路径，没有改写身份或合并用户数据。
- Windows x64 已完成原生构建与上述 executable/启动器验证，桌面 UI 和真实会话验收仍未覆盖。Windows ARM64、Linux、macOS x64 未完成对应系统运行验收。
- 模型使用本机 fixture，未验证线上账号鉴权、手机远控、SSH/WSL、所有工具、所有历史数据形态或 Rust/Node 的完全功能等价。

实现与所有权见 [spec](../specs/rust-npm-app-launcher.md)，命令和发布顺序见 [launcher README](../../apps/zcode-cli-rust/npm/README.md)。
