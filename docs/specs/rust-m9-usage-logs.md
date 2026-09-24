# Rust M9：用量与日志保留

依据：`apps/zcode-cli/packages/adapters/src/logging/retention.ts`、`bootstrap/src/log-retention.ts`；架构见 `rust-p0-p1-architecture.md` §5.16–5.17。用量部分在后续子项补充。

## 1. 日志保留（M9.1）

- 目录：`ZCODE_LOG_DIR`，缺省为 `~/.zcode/cli/log`（与写入相同）。
- 文件：只处理本 runtime 的日文件 `zcode-rust-YYYY-MM-DD.jsonl`；名称必须严格为 4-2-2 位数字且是合法日期（例如 `2026-02-30` 不处理）。Node 的 `zcode-YYYY-MM-DD.jsonl` 由 Node 自己清理，Rust 不触碰。
- 规则：保留 7 天。截止日期为本地日期 `今天 - 7 + 1`，日期早于截止日期的文件删除；目录不存在视为完成。
- 时机：app-server 启动 60 秒后执行一次，后台任务，不阻塞请求与退出。
- 日志：调度时 `info`（`log.retention.cleanup.scheduled`），完成时 `debug`（`log.retention.cleanup.completed`，含扫描、删除与失败数量），读取目录或删除失败时 `warn`（`log.retention.cleanup.failed` / `log.retention.delete.failed`，只记录文件名与错误类别，不记录路径以外的内容）。
- 验收：单元测试覆盖截止日期边界、非法与他人文件名、删除失败不影响其余文件。
