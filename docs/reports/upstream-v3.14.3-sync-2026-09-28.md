# 上游 v3.14.3 同步验证

## 合并范围

本地 `main` 先从 `494b673` 快进到 `rust-p0-p1` 的 `eb39660`，保留 94 个开发提交，再合并 `zai-org/ZCode:main` 的 `29628c9`。

唯一文本冲突位于 README：保留 Rust 迁移目标，另列上游版本同步记录。使用现有生成脚本更新 V4 command 和 Node projection 两个 Rust schema，未手改 Rust 实现。同步规则见 [spec](../specs/upstream-v3.14.3-sync.md)。

## 验证结果

环境：macOS arm64，Node 24.14.0，pnpm 10.33.2。

| 检查                                             | 结果                                                                                                              |
| ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| `pnpm install --frozen-lockfile`                 | 通过                                                                                                              |
| `pnpm typecheck`                                 | 合并前后均通过                                                                                                    |
| `pnpm lint`                                      | 合并前后均为 70 warning、0 error；上游新增未使用的 `BotWorkspaceRef` 导入 warning，原 `RemoteTarget` warning 消失 |
| `pnpm architecture:check --changed`              | 通过，0 violation                                                                                                 |
| `pnpm test:rust-npm`                             | 13/13 通过                                                                                                        |
| protocol/domain 两个 crate 的 `cargo test --lib` | 171/171 通过                                                                                                      |
| `pnpm test:zcode-cli-rust`                       | 生成资产、测试类型检查、Rust 根包测试和 Node/Rust 存储互读通过；Host 集成测试 300/301 通过                        |
| README、同步 spec 和两个生成 schema 的格式检查   | 通过                                                                                                              |
| `git diff --check`                               | 通过                                                                                                              |

## 已有失败与验证边界

`packages/services/tests/zcode-cli-rust-shell-lifecycle.test.ts` 的 `Rust EPIPE waits for TERM-ignoring job-control descendants before settling` 失败：关闭时预期退出码 0，实际退出码 1，stderr 为 `zcode-cli-rust: Protocol writer stopped`。

合并结果中单独复跑仍失败。随后在隔离 worktree 检出合并前的 `eb39660`，独立安装其冻结依赖、构建其 Rust 二进制，并以同一测试入口单独执行该用例，也得到同样失败。因此完整 Rust 回归并非全绿，该失败已在合并前复现；本次同步未修改对应退出逻辑或放宽断言。

没有执行真实 Electron UI、手机远控或 Windows/Linux 验证，也没有推送或发布。
