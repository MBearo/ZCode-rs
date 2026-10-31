//! 进程的退出码与信号编号（POSIX shell 的约定）。

/// 命令找到了但不能执行（没有执行权限等）。
pub const NOT_EXECUTABLE: i32 = 126;
/// 命令找不到。
pub const NOT_FOUND: i32 = 127;
/// 被信号结束时 shell 报的退出码是 `SIGNAL_BASE + 信号编号`。
pub const SIGNAL_BASE: i32 = 128;
/// ssh 自己出错（连不上、认证失败）时的退出码。
pub const SSH_ERROR: i32 = 255;

/// 信号编号（POSIX）。
pub const SIGHUP: i32 = 1;
pub const SIGKILL: i32 = 9;
pub const SIGTERM: i32 = 15;

/// 被信号 `signal` 结束时 shell 报的退出码。
pub const fn signal_exit(signal: i32) -> i32 {
    SIGNAL_BASE + signal
}
