//! esc 撤回刚发的输入（修复线 F38，docs/specs/rust-cli/tui-input.md §8、session-lifecycle.md §5.5）
//! 与运行中的 `/clear`、`/resume` 等回合结束再执行（tui.md §6；排进队列、不中断）。
use anyhow::Result;
use ratatui::backend::Backend;
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, Message};
use rust_cli_kernel_api::session_api::Command;
use serde_json::Value;

use super::Loop;
use super::session::Sent;
use crate::ui::app::Action;
use crate::ui::manage::Manage;

/// 运行中要等回合结束的本地命令显示的文字（同排队的输入，`❯ /clear`）；不用等的动作为 None。
fn after_turn_label(action: &Action) -> Option<String> {
    match action {
        Action::Clear => Some("/clear".to_owned()),
        Action::Manage(Manage::ClearNamed(name)) => Some(format!("/clear {name}")),
        Action::OpenResume => Some("/resume".to_owned()),
        Action::Resume(id) => Some(format!("/resume {id}")),
        _ => None,
    }
}

impl<B: Backend> Loop<'_, B> {
    /// 回合结束（`TurnFinished`）：这次中断是 esc / ctrl+c 发的（只认 user-cancel）、回合以 `Interrupted` 结束、
    /// 界面一侧的条件都满足时（输入框为空、没有留着的输入与本地命令、视图的队列为空、不在看子代理、没有等着的
    /// `/rewind`、不在退出），请内核撤回；内核一侧的条件（空闲、队列、有没有回答）由 actor 一次判断（§5.5）。
    pub(super) fn after_cancel(&mut self, interrupted: bool) {
        let cancelled = std::mem::take(&mut self.user_cancel);
        let app = &self.app;
        let quiet = app.composer.is_empty()
            && app.held.is_empty()
            && app.after_turn.is_empty()
            && app.view.queue().is_empty()
            && app.viewing.is_none();
        if cancelled && interrupted && quiet && self.deferred.is_none() && !self.stopping {
            self.send(
                Sent::RestoreCancelled,
                Command::RestoreCancelled,
                String::new(),
            );
        }
    }

    /// 撤回的回执：这条输入放回输入框（文字、bash 模式、`[Image #N]` 与图片），去掉刚记下的
    /// 历史。对话由随后的 `HistoryRewritten` 重新取快照。
    pub(super) fn restored(&mut self, result: &Value) {
        let Ok(message) = serde_json::from_value::<Message>(result["message"].clone()) else {
            return;
        };
        let Message::User(user) = &message else {
            return;
        };
        let text = crate::ui::session_rows::user_text(&message).unwrap_or_default();
        let images = user
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Image {
                    source: ImageSource::Base64 { media_type, data },
                } => Some((media_type.clone(), data.clone())),
                _ => None,
            })
            .collect();
        self.app.composer.restore_input(&text, images);
        self.app.composer.forget_submitted();
    }

    /// 运行中的 `/clear`、`/resume`：排到回合结束之后，显示在排队的输入后面；排了返回真。
    pub(super) fn defer_until_idle(&mut self, action: &Action) -> bool {
        if !self.app.running() {
            return false;
        }
        let Some(label) = after_turn_label(action) else {
            return false;
        };
        self.app.after_turn.push((label, action.clone()));
        true
    }

    /// 回合结束（`TurnFinished`，修复线 F45 起只在这时调用）、内核的队列与留着的输入都空了：按顺序执行等着的本地命令
    /// （按顺序处理）。
    pub(super) fn run_after_turn(&mut self) -> Result<()> {
        while !self.app.after_turn.is_empty()
            && !self.app.running()
            && !self.app.creating
            && self.app.held.is_empty()
            && self.app.view.queue().is_empty()
        {
            let (_, action) = self.app.after_turn.remove(0);
            self.act(action)?;
        }
        Ok(())
    }
}
