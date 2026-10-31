//! Linux 的 `/proc/<pid>/stat`（proc(5)）：只解析文字，读文件在调用方（fs 的锁、proc 的账本）。

use std::fmt::Display;

/// 开机以来的秒数（第一个数）。
pub const UPTIME_PATH: &str = "/proc/uptime";
/// 每秒的时钟滴答（`sysconf(_SC_CLK_TCK)`；Linux 上固定是 100，Node 的锁回收也这样取）。
pub const USER_HZ: u64 = 100;
/// 字段的编号（proc(5) 从 1 数）：状态、开机以来的启动滴答。
pub const STATE: usize = 3;
pub const STARTTIME: usize = 22;
/// 最后一个 `)` 之后的第一个字段的编号（前面是 pid 与带括号的 comm）。
const FIRST_AFTER_COMM: usize = 3;

/// 进程的 stat 文件。
pub fn stat_path(pid: impl Display) -> String {
    format!("/proc/{pid}/stat")
}

/// stat 文字的第 `number` 个字段。comm 可能含空格与括号，所以从最后一个 `)` 之后数；`number` 在 comm 之前时为 None。
pub fn field(stat: &str, number: usize) -> Option<&str> {
    let rest = stat.get(stat.rfind(')')? + ") ".len()..)?;
    rest.split(' ').nth(number.checked_sub(FIRST_AFTER_COMM)?)
}
