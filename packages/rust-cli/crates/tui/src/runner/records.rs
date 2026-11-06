//! 本地命令的记录（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11）：命令完成时组成记录，有会话时交给会话 actor
//! （`Command::RecordLocalCommand`），还没有会话时留在新会话的草稿里，建会话后、第一条输入之前按顺序发出。
use rust_cli_kernel_api::session_api::Command;
use rust_cli_kernel_api::user_command::{LocalCommandOutput, LocalCommandRecord};

use super::Loop;
use super::session::Sent;

/// `/rename 名字` 的命令：改名与它的记录（对照实现的 system 显示：命令输入、结果与给模型的提醒，修复线 L-A，
/// tui-manage.md §11.2）。记录跟着改名由会话 actor 提交，改名被拒绝时记录也不在（titles.md §4）。
pub(super) fn rename(title: &str) -> Command {
    let mut record = record("rename", title, renamed(title), false);
    record.meta = vec![rust_cli_prompts::commands::rename_named(title)];
    Command::RenameRecorded {
        title: title.to_owned(),
        record,
    }
}

/// 改名成功的提示（对照实现的原文）。
pub(super) fn renamed(title: &str) -> String {
    format!("Session renamed to: {title}")
}

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

impl Loop<'_> {
    /// 交出一条记录：有会话（并且不在建会话）时马上发，否则留在新会话的草稿里。
    pub(super) fn record_command(&mut self, record: LocalCommandRecord) {
        let command = Command::RecordLocalCommand { record };
        if self.outbox.session().is_some() && !self.app.creating {
            self.send(Sent::Record, command);
        } else {
            self.outbox.hold(Sent::Record, command);
        }
    }

    /// 界面层这一轮完成的记录。
    pub(super) fn flush_records(&mut self) {
        for record in std::mem::take(&mut self.app.records) {
            self.record_command(record);
        }
    }

    /// 新会话打开：草稿里的命令先发（排在第一条输入之前）。
    pub(super) fn send_held(&mut self) {
        for (sent, command) in self.outbox.take_held() {
            self.send(sent, command);
        }
    }

    /// 按名字的切换完成（2026-10-31 起只有 `/login` 之后切到登录给出的模型；`/model 词` 只打开选择器）：`saved` 是新模型
    /// 也存成了新会话的缺省（对照实现的两种说法）。
    ///
    /// 缺陷修复：以前参数放在界面层的一个共享槽里，任何 `SetModel` 的回执都取它。`/effort` 的回执晚于下一个
    /// `/model` 时把旧模型记成 `/model` 的结果，`/effort` 先完成时清掉槽、`/model` 不记。现在参数跟着这次切换走
    /// （`Done::Catalog` 的 `pick`、`ModelDone::Chosen` 的 `args`）。
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
