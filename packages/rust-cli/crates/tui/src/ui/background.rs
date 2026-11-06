//! 后台命令（docs/specs/rust-cli/tui-panels.md §3；tui-tasks.md §3.2）：在跑的命令来自后台任务快照（`app.snapshots.background`，与
//! 子代理列表同一份快照，修复线 F34），任务卡的 Commands 列出它们，⏎ 在行下面展开输出的最后几行。这里只有命令的行与展开的
//! 输出的最后一次读取结果。2026-10-31（用户决定）之前这里还有 footer 的 pill、Background 面板与 Shell 详情页。
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind};
use rust_cli_kernel_api::panels::ShellDetail;

use super::app::App;

/// 展开着输出的命令：它的 ID、读取序号（runner 分配）、最后一次读到的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ShellView {
    pub id: String,
    pub seq: u64,
    pub last: Option<ShellDetail>,
    /// 内核已不认识它（结束且通知已 ack、或已停止）：不再读取。
    pub gone: bool,
}

impl ShellView {
    pub fn new(id: String) -> Self {
        Self {
            id,
            seq: 0,
            last: None,
            gone: false,
        }
    }
}

/// 输出里的一行去掉 ANSI 转义与控制字符，tab 换成 4 个空格。
pub fn plain_output(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // CSI：ESC [ … 结束字节（0x40–0x7e）；其他转义只跳过下一个字符。
                if chars.next() == Some('[') {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
            }
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

impl App {
    /// 在跑的后台 shell 与 monitor（快照的顺序）。monitor 与 shell 同列（修复线 F117，tui-tasks.md §3.2），名称是快照
    /// 条目的 description（会话按 Monitor 的 description 填）。
    pub fn shell_rows(&self) -> Vec<&BackgroundEntry> {
        self.snapshots
            .background
            .iter()
            .filter(|entry| {
                matches!(entry.kind, BackgroundKind::Shell | BackgroundKind::Monitor)
                    && entry.status == BackgroundStatus::Running
            })
            .collect()
    }

    fn peek_mut(&mut self) -> Option<&mut ShellView> {
        self.task_card_mut()?.peek.as_mut()
    }

    /// runner 给这次读取分配的序号（展开输出时）。
    pub fn shell_reading(&mut self, id: &str, seq: u64) {
        if let Some(peek) = self.peek_mut()
            && peek.id == id
        {
            peek.seq = seq;
        }
    }

    /// 读取的回复：序号与展开着的输出相同时保存；返回是否要再读一次（还在跑）。
    pub fn on_shell_detail(&mut self, id: &str, seq: u64, detail: Option<ShellDetail>) -> bool {
        let Some(peek) = self.peek_mut() else {
            return false;
        };
        if peek.id != id || peek.seq != seq {
            return false;
        }
        match detail {
            Some(detail) => {
                let running = detail.status == BackgroundStatus::Running;
                peek.last = Some(detail);
                running
            }
            None => {
                peek.gone = true;
                false
            }
        }
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
