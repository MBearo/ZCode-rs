//! 本地命令的记录（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11）：命令完成时组成记录，有会话时交给会话 actor
//! （`Command::RecordLocalCommand`），还没有会话时留在新会话的草稿里，建会话后、第一条输入之前按顺序发出。
use ratatui::backend::Backend;
use rust_cli_kernel_api::session_api::Command;
use rust_cli_kernel_api::user_command::{LocalCommandOutput, LocalCommandRecord};

use super::Loop;
use super::session::Sent;

/// 一条记录：`caveat` 为真时前面加固定说明（对照实现的缺省显示）。
pub(crate) fn record(name: &str, args: &str, stdout: String, caveat: bool) -> LocalCommandRecord {
    LocalCommandRecord {
        name: name.to_owned(),
        args: args.to_owned(),
        output: LocalCommandOutput::Stdout(stdout),
        caveat,
        meta: Vec::new(),
    }
}

impl<B: Backend> Loop<'_, B> {
    /// 交出一条记录：有会话（并且不在建会话）时马上发，否则留在新会话的草稿里。
    pub(super) fn record_command(&mut self, record: LocalCommandRecord) {
        if self.session.is_some() && !self.app.creating {
            self.send(
                Sent::Record,
                Command::RecordLocalCommand { record },
                String::new(),
            );
        } else {
            self.pending_records.push(record);
        }
    }

    /// 界面层这一轮完成的记录。
    pub(super) fn flush_records(&mut self) {
        for record in std::mem::take(&mut self.app.records) {
            self.record_command(record);
        }
    }

    /// 新会话打开：草稿里的记录先发（排在第一条输入之前）。
    pub(super) fn send_pending_records(&mut self) {
        for record in std::mem::take(&mut self.pending_records) {
            self.send(
                Sent::Record,
                Command::RecordLocalCommand { record },
                String::new(),
            );
        }
    }

    /// `/model <args>` 的切换完成：`saved` 是新模型也存成了新会话的缺省（对照实现的两种说法）。
    ///
    /// 缺陷修复：以前参数放在界面层的一个共享槽里，任何 `SetModel` 的回执都取它。`/effort` 的回执晚于下一个
    /// `/model` 时把旧模型记成 `/model` 的结果，`/effort` 先完成时清掉槽、`/model` 不记。现在参数跟着这次切换走
    /// （`Done::Catalog` 的 `pick`、`ModelDone::Chosen` 的 `args`、`Sent::PickModel` 的 `label`），只有 `/model` 的切换写记录。
    pub(super) fn record_model(&mut self, args: &str, model: &str, saved: bool) {
        let tail = if saved {
            " and saved as your default for new sessions"
        } else {
            " for this session only"
        };
        self.record_command(record(
            "model",
            args,
            format!("Set model to `{model}`{tail}"),
            true,
        ));
    }
}
