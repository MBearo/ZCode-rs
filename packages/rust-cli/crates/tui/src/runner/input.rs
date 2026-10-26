//! 输入能力的动作（docs/specs/rust-cli/tui-input.md）：各种输入的发送、立即发送、粘贴图片。
use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::session_api::Command;

use super::Loop;
use super::session::{Done, Sent};
use crate::services::ImageBytes;
use crate::ui::composer::images::Added;
use crate::ui::i18n_input;
use crate::ui::notice::HINT_FOR;

impl<B: Backend> Loop<'_, B> {
    /// 发一条输入命令（`Submit`、`SubmitContent`、`Bash`）；没有会话时先建会话，建好后再发（tui.md §4.1）。
    ///
    /// 缺陷修复（R5-9）：原来创建期间的输入被丢掉（`creating` 时什么都不做）。现在先按顺序留在 `held`，会话打开、
    /// 第一条发出后再逐条走这里，由内核排队（`Done::Opened`）。
    pub(super) fn submit(&mut self, command: Command) {
        if self.session.is_some() {
            self.send(Sent::Submit, command, String::new());
        } else if self.app.creating {
            self.app.held.push(command);
        } else {
            self.create(command);
        }
    }

    /// ctrl+enter（`chat:sendNow`，tui-input.md §11.1）：运行中是低延迟投递：先入队（插话，下一个工具边界并入），
    /// 再把前台 Bash 转到后台（工具边界随之到来），不中断当前回合。两条命令在同一个 future 里按顺序发，入队一定先于
    /// 转后台（转后台之后的工具边界一定看得到这条插话）。
    ///
    /// 修复线 L-A（用户决定 2026-10-25：照对照实现）：以前入队后中断当前回合，等着的请求被丢掉、对话里多一条中断标记。
    pub(super) fn send_now(&mut self, command: Command) {
        let session = match &self.session {
            Some(session) if self.app.running() => session.clone(),
            _ => return self.submit(command),
        };
        let moved = Command::BackgroundForeground {
            reason: rust_cli_kernel_api::env::BackgroundReason::DeliverMessage,
        };
        let (queued, moved) = (self.envelope(command), self.envelope(moved));
        self.pending.push(
            async move {
                let ack = session.send(queued).await;
                if matches!(ack, rust_cli_kernel_api::session_api::Ack::Accepted { .. }) {
                    let _ = session.send(moved).await;
                }
                Done::Ack {
                    sent: Sent::Submit,
                    label: String::new(),
                    ack,
                }
            }
            .boxed(),
        );
    }

    /// ctrl+v / alt+v：经入口注入的剪贴板服务读图片（tui 不起进程，tui-input.md §3.1）。
    pub(super) fn paste_image(&mut self) {
        let Some(clipboard) = self.options.services.clipboard.clone() else {
            let lang = self.app.lang;
            self.app
                .notify(i18n_input::clipboard_unavailable(lang), HINT_FOR);
            return;
        };
        self.pending
            .push(async move { Done::Image(clipboard.image().await) }.boxed());
    }

    /// 粘贴的路径像图片文件：读出来再决定（读不出时当文字粘贴）。
    pub(super) fn paste_image_path(&mut self, path: String, text: String) {
        let Some(clipboard) = self.options.services.clipboard.clone() else {
            self.app.paste_plain(&text);
            return;
        };
        self.pending.push(
            async move {
                let image = clipboard.image_file(path).await.ok().flatten();
                Done::ImageFile { image, text }
            }
            .boxed(),
        );
    }

    pub(super) fn on_image(&mut self, result: Result<Option<ImageBytes>, String>) {
        let lang = self.app.lang;
        let key = crate::ui::shortcuts::image_paste_chord();
        let notice = match result {
            Ok(Some(image)) => match self.app.composer.add_image(&image.bytes) {
                Added::Inserted(_) => return self.app.composer.refresh_popup(),
                Added::NotImage => i18n_input::no_image(lang, key),
                Added::TooLarge => i18n_input::image_too_large(lang),
            },
            Ok(None) => i18n_input::no_image(lang, key),
            Err(reason) => i18n_input::clipboard_failed(lang, &reason),
        };
        self.app.notify(notice, HINT_FOR);
    }

    pub(super) fn on_image_file(&mut self, image: Option<ImageBytes>, text: &str) {
        let added = image.map(|image| self.app.composer.add_image(&image.bytes));
        match added {
            Some(Added::Inserted(_)) => self.app.composer.refresh_popup(),
            Some(Added::TooLarge) => {
                let lang = self.app.lang;
                self.app.notify(i18n_input::image_too_large(lang), HINT_FOR);
            }
            Some(Added::NotImage) | None => self.app.paste_plain(text),
        }
    }

    /// 把终端交给外部编辑器（tui-input.md §4.1）：交出期间不画帧。
    pub(super) fn hand_over(&mut self) {
        self.screen.suspend();
        self.suspended = true;
    }

    /// 收回终端，整屏重画。
    pub(super) fn take_back(&mut self) {
        self.screen.resume();
        self.suspended = false;
        // 整屏重画：清屏并丢掉后台缓冲。不用 `Terminal::clear`：它先查询光标位置（`ESC[6n`），刚收回的终端
        // 上这个查询可能等不到回复（PTY 冒烟测试里两秒后失败，后台缓冲没有重置，只画了变化的格子）。
        if let Ok(size) = self.terminal.size() {
            let _ = self
                .terminal
                .resize(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        }
        self.dirty = true;
    }

    /// ctrl+g、ctrl+x ctrl+e：用外部编辑器编辑输入（粘贴的折叠内容展开后交出去）。
    pub(super) fn external_editor(&mut self) {
        let Some(editor) = self.options.services.editor.clone() else {
            let lang = self.app.lang;
            self.app
                .notify(i18n_input::editor_unavailable(lang), HINT_FOR);
            return;
        };
        let text = self.app.composer.expanded_text();
        self.hand_over();
        self.pending
            .push(async move { Done::Edited(editor.edit(text).await) }.boxed());
    }

    pub(super) fn on_edited(&mut self, result: Result<String, String>) {
        self.take_back();
        match result {
            Ok(text) => self.app.composer.from_editor(&text),
            Err(reason) => {
                let lang = self.app.lang;
                self.app
                    .notify(i18n_input::editor_failed(lang, &reason), HINT_FOR);
            }
        }
    }

    /// `/memory` 选中的文件（tui-input.md §4.2）。
    pub(super) fn open_memory(&mut self, path: String, display: String) {
        let Some(editor) = self.options.services.editor.clone() else {
            let lang = self.app.lang;
            self.app
                .error(i18n_input::editor_unavailable(lang).to_owned());
            return;
        };
        self.hand_over();
        self.pending.push(
            async move {
                let result = editor.open(path).await;
                Done::MemoryOpened { display, result }
            }
            .boxed(),
        );
    }

    /// 打开记忆文件后的提示原文。
    pub(super) fn on_memory_opened(&mut self, display: &str, result: Result<(), String>) {
        self.take_back();
        match result {
            Ok(()) => {
                let source = self
                    .options
                    .services
                    .editor
                    .as_ref()
                    .and_then(|e| e.source());
                let hint = match source {
                    Some((name, value)) => format!(
                        "> Using {name}=\"{value}\". To change editor, set $EDITOR or $VISUAL environment variable."
                    ),
                    None => {
                        "> To use a different editor, set the $EDITOR or $VISUAL environment variable."
                            .to_owned()
                    }
                };
                self.app
                    .info(format!("Opened memory file at {display}\n\n{hint}"));
            }
            Err(reason) => self
                .app
                .error(format!("Error opening memory file: {reason}")),
        }
    }
}
