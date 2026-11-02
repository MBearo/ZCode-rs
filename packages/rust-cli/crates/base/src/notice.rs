//! 后台命令与 Monitor 的终态通知的入参（工作线 F134，modularity.md §7 F131-2）。
//!
//! 文字在 prompts 的 `notification`；从执行端口的通知换算成它只在 kernel-api 的 `ShellNotice::task_notice` 一处（以前
//! 会话与子代理各写一份）。定义在这里是因为 prompts 与 kernel-api 只共同依赖 base（同 `reminder`，F58）；prompts 的
//! `notification::TaskNotice` 重新导出它。

/// 一条终态通知要写的内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskNotice<'a> {
    /// 任务 ID（`<task-id>`）。
    pub task: &'a str,
    /// 启动它的工具调用（`<tool-use-id>`）；没有时不写这一行。
    pub tool_use: Option<&'a str>,
    /// 启动时的说明（Bash 的 `description`、Monitor 的 `description`）。
    pub description: &'a str,
    /// 输出文件（`<output-file>`）。
    pub output: &'a str,
    /// `<status>`：`completed`、`failed` 或 `killed`。
    pub status: &'a str,
    pub exit_code: Option<i32>,
    /// 命令被停掉（取消、中断、超时）：命令的说明写 `was stopped`。
    pub stopped: bool,
    /// Monitor 的通知：说明按 Monitor 的写法。
    pub monitor: bool,
    /// 输出文件的字节数（Monitor 的说明分“有没有输出”）。
    pub output_bytes: u64,
}
