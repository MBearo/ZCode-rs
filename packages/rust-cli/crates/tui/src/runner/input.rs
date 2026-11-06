//! 输入能力的动作（docs/specs/rust-cli/tui-input.md）：各种输入的发送、立即发送、粘贴图片。
use futures_util::FutureExt;
use rust_cli_kernel_api::session_api::Command;

use super::Loop;
use super::session::{Done, Sent};
use crate::services::ImageBytes;
use crate::ui::composer::images::Added;
use crate::ui::i18n_input;
use crate::ui::notice::HINT_FOR;

impl Loop<'_> {
    /// 发一条输入命令（`Submit`、`SubmitContent`、`Bash`）；没有会话时先建会话，建好后再发（tui.md §4.1）。
    ///
    /// 缺陷修复（R5-9）：原来创建期间的输入被丢掉（`creating` 时什么都不做）。现在先按顺序留在 `held`，会话打开、
    /// 第一条发出后再逐条走这里，由内核排队（`Done::Opened`）。
    ///
    /// 缺陷修复（2026-10-31，与 Puffin 不同）：没有模型（门道开着）时以前照样建会话，回合因为没有模型失败、消息没有留下。
    /// Puffin 把它留着、说 `Queued. It goes out once a model is connected.`，接上模型后发出。现在同 Puffin：留在 `held`，
    /// 模型接上时 [`Self::release_for_model`] 建会话。入口的缺省模型还没定下时（状态栏可能是上次的结论，performance.md
    /// §12.4）也留着，定下后再发：会话不建在没核对过的结论上。
    pub(super) fn submit(&mut self, command: Command) {
        if self.outbox.session().is_some() {
            self.send(Sent::Submit(submit_label(&command)), command);
        } else if self.app.creating
            || self.app.model_pending
            || self.app.view.selection().model.is_empty()
        {
            self.app.held.push(command);
        } else {
            self.create(command);
        }
    }

    /// 新会话的草稿有了模型（登录、密钥、`/model`、设置）：没有模型时留下的输入现在建会话发出，其余的照建会话期间留下的
    /// 输入在会话打开后按顺序发出（`Done::Opened`）。
    pub(super) fn release_for_model(&mut self) {
        if self.outbox.session().is_some()
            || self.app.creating
            || self.app.model_pending
            || self.app.view.selection().model.is_empty()
            || self.app.held.is_empty()
        {
            return;
        }
        let first = self.app.held.remove(0);
        self.create(first);
    }

    /// ctrl+enter（`chat:sendNow`，tui-input.md §11.1）：运行中是低延迟投递：先入队（插话，下一个工具边界并入），
    /// 再把前台 Bash 转到后台（工具边界随之到来），不中断当前回合。两条命令在同一个 future 里按顺序发，入队一定先于
    /// 转后台（转后台之后的工具边界一定看得到这条插话）。
    ///
    /// 修复线 L-A（用户决定 2026-10-25：照对照实现）：以前入队后中断当前回合，等着的请求被丢掉、对话里多一条中断标记。
    pub(super) fn send_now(&mut self, command: Command) {
        let session = match self.outbox.session() {
            Some(session) if self.app.running() => session.clone(),
            _ => return self.submit(command),
        };
        let moved = Command::BackgroundForeground {
            reason: rust_cli_kernel_api::env::BackgroundReason::DeliverMessage,
        };
        let (queued, moved) = (self.input_envelope(command), self.envelope(moved));
        let id = queued.id.clone();
        self.pending.push(
            async move {
                let ack = session.send(queued).await;
                if matches!(ack, rust_cli_kernel_api::session_api::Ack::Accepted { .. }) {
                    let _ = session.send(moved).await;
                }
                Done::Ack {
                    sent: Sent::Submit(String::new()),
                    ack,
                    command: Some(id),
                }
            }
            .boxed(),
        );
    }

    /// ctrl+v / alt+v：经入口注入的剪贴板服务读图片（tui 不起进程，tui-input.md §3.1）。
    pub(super) fn paste_image(&mut self) {
        let clipboard = self.options.services.clipboard.clone();
        self.pending
            .push(async move { Done::Image(clipboard.image().await) }.boxed());
    }

    /// 粘贴的路径像图片文件：读出来再决定（读不出时当文字粘贴）。
    pub(super) fn paste_image_path(&mut self, path: String, text: String) {
        let clipboard = self.options.services.clipboard.clone();
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
        let key = rust_cli_tui_kit::app::widgets::key_hints::paste_key();
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

    /// 把终端交给外部编辑器（tui-input.md §4.1）：交出期间不画帧。渲染器先离开它的备用屏（还回涂的地、打开自动换行），
    /// 收回后的第一帧重新进入并整屏画。
    pub(super) fn hand_over(&mut self) {
        if let Some(bytes) = self.renderer.exit() {
            let _ = self.screen.write(&bytes);
        }
        self.screen.suspend();
        self.suspended = true;
    }

    /// 收回终端，整屏重画。
    pub(super) fn take_back(&mut self) {
        self.screen.resume();
        self.suspended = false;
        // 整屏重画：交出时渲染器已经离开（`exit` 丢掉了基线），下一帧清屏后画每个格子；不查询光标位置（刚收回的终端上
        // `ESC[6n` 可能等不到回复）。
        self.renderer.invalidate();
        self.dirty = true;
    }

    /// ctrl+g、ctrl+x ctrl+e：用外部编辑器编辑输入（粘贴的折叠内容展开后交出去）。
    pub(super) fn external_editor(&mut self) {
        let editor = self.options.services.editor.clone();
        let text = self.app.composer.expanded_text();
        self.hand_over();
        self.pending
            .push(async move { Done::Edited(editor.edit(text).await) }.boxed());
    }

    pub(super) fn on_edited(&mut self, result: Result<String, String>) {
        self.take_back();
        match result {
            Ok(text) => self.app.composer.load_edited(&text),
            Err(reason) => {
                let lang = self.app.lang;
                self.app
                    .notify(i18n_input::editor_failed(lang, &reason), HINT_FOR);
            }
        }
    }
}

/// 输入的回执带的数据（`Sent::Submit`）：`!` 命令带命令本身（被拒绝时不再画 ⏎ 时画的块，`Transcript::drop_shell`），
/// 其余为空。
pub(super) fn submit_label(command: &Command) -> String {
    match command {
        Command::Bash { command, .. } => command.clone(),
        _ => String::new(),
    }
}
