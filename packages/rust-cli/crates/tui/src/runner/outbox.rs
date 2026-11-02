//! 发往会话的出口（架构整理 U10，docs/specs/rust-cli/tui.md §4.1）：当前会话的句柄（唯一一份）与 runner 一侧“稍后再发”
//! 的东西。
//!
//! 以前句柄存在 `Loop.session`，界面另存一份会话 ID（`App.session`），打开与切换各在两处同时写；等回合结束的 rewind 与
//! 还没有会话时的命令记录是事件循环上另外两个字段。现在句柄只在这里，经 [`Loop::attach`](super::Loop) 一处写入（同时写
//! 界面上的会话 ID 与订阅）；界面上的会话 ID 是“画面上的会话”，切换期间句柄已经交出、画面还是旧会话，所以两者不总相同。
use std::sync::Arc;

use rust_cli_kernel_api::session_api::{Command, SessionHandle};
use rust_cli_kernel_api::user_command::LocalCommandRecord;

use super::session::Sent;

#[derive(Default)]
pub(super) struct Outbox {
    /// 当前会话；还没有会话、建会话期间、切换期间（旧的已交出、新的还没打开）为 None。
    session: Option<Arc<dyn SessionHandle>>,
    /// 运行中确认的 rewind：回合结束后发出（回执要用的数据与命令）。
    deferred: Option<(Sent, Command)>,
    /// 还没有会话时（启动后、`/clear` 之后）完成的本地命令的记录：建会话后、第一条输入之前按顺序发出（修复线 L-A，
    /// tui-manage.md §11.3；新会话草稿的一部分）。
    records: Vec<LocalCommandRecord>,
}

impl Outbox {
    pub(super) fn session(&self) -> Option<&Arc<dyn SessionHandle>> {
        self.session.as_ref()
    }

    pub(super) fn attach(&mut self, handle: Arc<dyn SessionHandle>) {
        self.session = Some(handle);
    }

    /// 交出当前会话的句柄（由调用方关闭）。
    pub(super) fn detach(&mut self) -> Option<Arc<dyn SessionHandle>> {
        self.session.take()
    }

    pub(super) fn defer(&mut self, sent: Sent, command: Command) {
        self.deferred = Some((sent, command));
    }

    pub(super) fn take_deferred(&mut self) -> Option<(Sent, Command)> {
        self.deferred.take()
    }

    pub(super) fn has_deferred(&self) -> bool {
        self.deferred.is_some()
    }

    pub(super) fn hold_record(&mut self, record: LocalCommandRecord) {
        self.records.push(record);
    }

    pub(super) fn take_records(&mut self) -> Vec<LocalCommandRecord> {
        std::mem::take(&mut self.records)
    }

    /// `/clear`：之前留在草稿里的记录随旧对话一起清掉。
    pub(super) fn clear_records(&mut self) {
        self.records.clear();
    }
}
