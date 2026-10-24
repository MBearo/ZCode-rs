//! 输入与动作（docs/specs/rust-cli/tui.md §5、§6）：终端事件交给界面状态，界面产生的 [`Action`] 在这里
//! 变成内核命令或终端操作。模型与模式有会话时以内核事件为准，没有会话时只改新会话的草稿。
use std::time::Instant;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::Command;

use super::Loop;
use super::session::Sent;
use crate::ui::app::Action;

/// 没有对话时的说明（titles.md §8.3）。
const NO_NAME: &str =
    "Could not generate a name: no conversation context yet. Usage: /rename <name>";
use crate::ui::i18n;

impl<B: Backend> Loop<'_, B> {
    pub(super) fn on_input(&mut self, event: Event) -> Result<()> {
        let now = Instant::now();
        let tick = tokio::time::Instant::now();
        self.app.notify.input(tick);
        // 提问框的空闲计时（工作线 F72）：按键、粘贴与鼠标点击重新计时，焦点变化照终端的报告。
        match &event {
            Event::Key(_) | Event::Paste(_) => self.app.afk.input(tick),
            Event::Mouse(mouse) if mouse.kind != MouseEventKind::Moved => self.app.afk.input(tick),
            Event::FocusGained => self.app.afk.focus(true),
            Event::FocusLost => self.app.afk.focus(false),
            _ => {}
        }
        // Windows 的 paste burst（tui-input.md §9.2）：鼠标操作（移动除外）与粘贴之前先交出缓存的文字。
        let mut actions = match &event {
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved => Vec::new(),
            Event::Mouse(_) | Event::Paste(_) => self.app.flush_burst_before_input(),
            _ => Vec::new(),
        };
        match event {
            Event::Key(key) => {
                self.cancel_loops_on(&key);
                actions.extend(self.app.on_terminal_key(key, now));
            }
            Event::Paste(text) => actions.extend(self.app.on_paste(&text)),
            Event::Mouse(mouse) => actions.extend(self.app.on_mouse(mouse, now)),
            // 尺寸变了：全屏视图的选区（屏幕坐标）不再对应原来的文字（tui-select.md §2.1）。
            Event::Resize(..) => self.app.screen_selection = None,
            Event::FocusGained | Event::FocusLost => self.app.focus_changed(),
        }
        self.dirty = true;
        for action in actions {
            self.act(action)?;
        }
        self.fetch_files(now);
        Ok(())
    }

    /// 到了 paste burst 的冲刷时刻（tui-input.md §9.2）：交出的粘贴或扣住的字符照常处理。
    pub(super) fn on_burst_due(&mut self) -> Result<()> {
        let now = Instant::now();
        self.dirty = true;
        for action in self.app.flush_burst(now) {
            self.act(action)?;
        }
        self.fetch_files(now);
        Ok(())
    }

    /// 空闲时 esc / ctrl+c 也取消本会话的 loop 唤醒（tools-more.md §18.1）。有没有唤醒由内核判断，
    /// 回执不提示；按键照常往下处理（TUI 不持有任务表的投影，D41）。运行中（画面上）的中断由按键本身发出。
    fn cancel_loops_on(&mut self, key: &KeyEvent) {
        let cancel = match key.code {
            KeyCode::Esc => key.modifiers.is_empty(),
            KeyCode::Char('c') => key.modifiers.contains(KeyModifiers::CONTROL),
            _ => false,
        };
        let idle = self.session.is_some()
            && !self.app.running()
            && self.app.overlay.is_none()
            && self.app.interaction.is_none()
            && self.app.composer.popup.is_none();
        if cancel && idle && key.kind != KeyEventKind::Release {
            // 缺陷修复（F45）：按 enter 之后、回合画出来之前，视图还是空闲，esc 走这里；内核这时可能已经开了回合，同一个
            // `Interrupt` 以用户中断中止它。以前只有运行中的路径记下用户中断，这个回合被中止了却不撤回，对话里留下输入与
            // 中断标记（tui-input.md §8）。这里同样记下，撤不撤回由 actor 按它的事实判断；没有回合时这次什么都不中止，
            // 标记在下一个回合结束时取走——那个回合若因别的原因中断（ctrl+enter、运行中的 `/rewind`、退出），界面一侧的
            // 条件或 actor 的判断都不满足（restore.rs）。
            self.user_cancel = true;
            self.send(Sent::CancelLoops, Command::Interrupt, String::new());
        }
    }

    pub(super) fn act(&mut self, action: Action) -> Result<()> {
        let lang = self.app.lang;
        // 运行中的 `/clear`、`/resume` 等回合结束再执行（修复线 F38，tui.md §6）：排进队列、不中断。
        if self.defer_until_idle(&action) {
            return Ok(());
        }
        match action {
            Action::Submit { text, delivery } => {
                self.app.notify.submitted();
                if let Some(recorder) = &self.options.recorder {
                    recorder.record(&text, self.now_ms());
                }
                self.submit(Command::Submit {
                    text,
                    attachments: Vec::new(),
                    delivery,
                });
            }
            Action::SubmitContent { content, delivery } => {
                let text = content
                    .iter()
                    .find_map(|block| match block {
                        rust_cli_kernel_api::message::ContentBlock::Text { text } => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                if let Some(recorder) = &self.options.recorder {
                    recorder.record(&text, self.now_ms());
                }
                self.submit(Command::SubmitContent { content, delivery });
            }
            Action::PasteImage => self.paste_image(),
            Action::ExternalEditor => self.external_editor(),
            Action::OpenMemory { path, display } => self.open_memory(path, display),
            Action::PasteImagePath { path, text } => self.paste_image_path(path, text),
            Action::Bash { command, delivery } => {
                // 历史里带 `!`，载入时据此回到 bash 模式。
                if let Some(recorder) = &self.options.recorder {
                    recorder.record(&format!("!{command}"), self.now_ms());
                }
                self.submit(Command::Bash { command, delivery });
            }
            Action::SendNow { text, delivery } => {
                if let Some(recorder) = &self.options.recorder {
                    recorder.record(&text, self.now_ms());
                }
                self.send_now(Command::Submit {
                    text,
                    attachments: Vec::new(),
                    delivery,
                });
            }
            // esc / ctrl+c 的中断：回合结束时据此判断要不要撤回（修复线 F38，restore.rs）。
            Action::Interrupt => {
                self.user_cancel = true;
                self.send(Sent::Interrupt, Command::Interrupt, String::new());
            }
            Action::ResolvePeer { message, approve } => self.send(
                Sent::Resolve,
                Command::ResolvePeerMessage { message, approve },
                String::new(),
            ),
            // 旧会话的询问（agents.md §4.1.2，工作线 F72）经内核的进程级入口回答，回执同会话的回答。
            Action::Resolve {
                interaction,
                answer,
            } if self.app.detached_title(&interaction).is_some() => {
                let kernel = self.kernel.clone();
                self.pending.push(
                    async move {
                        let ack = match kernel.resolve_detached(&interaction, answer).await {
                            Ok(()) => rust_cli_kernel_api::session_api::Ack::Accepted {
                                result: serde_json::Value::Null,
                            },
                            Err(error) => rust_cli_kernel_api::session_api::Ack::Rejected {
                                reason: error.to_string(),
                            },
                        };
                        super::session::Done::Ack {
                            sent: Sent::Resolve,
                            label: String::new(),
                            ack,
                        }
                    }
                    .boxed(),
                );
            }
            Action::Resolve {
                interaction,
                answer,
            } => self.send(
                Sent::Resolve,
                Command::ResolveInteraction {
                    interaction,
                    answer,
                },
                String::new(),
            ),
            Action::SetModel(selection) => self.set_model(selection),
            Action::ChooseModel(selection) => self.choose_model(selection),
            Action::SetMode(mode) => self.set_mode(mode),
            Action::SetPlan(enabled) => self.set_plan(enabled),
            Action::Compact(instructions) => {
                if self.session.is_some() {
                    self.send(
                        Sent::Compact,
                        Command::Compact { instructions },
                        String::new(),
                    );
                } else {
                    self.app.info(i18n::not_implemented(lang, "compact"));
                }
            }
            Action::OpenModels => self.catalog(None),
            Action::PickModel(name) => self.catalog(Some(name)),
            Action::Clear => self.clear()?,
            Action::Copy(text) => {
                self.copy_text(text);
                self.app.info(self.app.strings.copied);
            }
            Action::CopySelection { clear } => self.copy_after_draw = Some(clear),
            Action::Redraw => self
                .terminal
                .clear()
                .map_err(|error| anyhow::anyhow!("clearing the terminal: {error}"))?,
            Action::OpenRewind => self.open_rewind(),
            Action::Rewind {
                target,
                scope,
                text,
            } => self.rewind(target, scope, text),
            Action::OpenResume => self.open_resume(false),
            Action::Resume(id) => self.switch(
                rust_cli_kernel_api::ids::SessionId::new(id),
                rust_cli_kernel_api::session_api::StartSource::Resume,
                None,
                true,
            ),
            Action::Rename(title) => {
                if self.session.is_some() {
                    let command = Command::Rename {
                        title: title.clone(),
                    };
                    self.send(Sent::Rename, command, title);
                } else {
                    self.app.info(i18n::not_implemented(lang, "rename"));
                }
            }
            // 还没有会话时没有对话可起名：直接给出说明，不发命令（titles.md §8.3）。
            Action::GenerateName => {
                if self.session.is_some() {
                    self.send(Sent::GenerateName, Command::GenerateName, String::new());
                } else {
                    self.app.info(NO_NAME.to_owned());
                }
            }
            Action::Context { all } => self.context_usage(all),
            Action::SetLocale(locale) => self.save_locale(locale),
            Action::Login(family) => self.login(family),
            Action::CancelLogin => self.cancel_login(),
            Action::ConfigureKey { family, key } => self.configure_key(family, key),
            Action::Logout => self.logout(),
            Action::ShowGoal => self.show_goal(),
            Action::OpenFork => self.open_fork(),
            Action::Fork { input } => self.fork(input),
            Action::Quit => self.app.quit = true,
            Action::Manage(manage) => self.manage(manage)?,
            Action::Panel(panel) => self.panel(panel),
            Action::Projects(request) => self.projects(request),
            Action::Machines(request) => self.machine_request(request),
        }
        Ok(())
    }

    pub(super) fn set_model(&mut self, selection: ModelSelection) {
        let label = selection.model.clone();
        if self.session.is_some() {
            self.send(Sent::SetModel, Command::SetModel { selection }, label);
        } else {
            let execution = self.app.view.execution();
            self.app.view.set_draft(selection, execution);
            self.app.info(i18n::model_set(self.app.lang, &label));
        }
    }

    fn set_mode(&mut self, mode: PermissionMode) {
        if self.session.is_some() {
            self.send(Sent::SetMode, Command::SetMode { mode }, String::new());
        } else {
            let selection = self.app.view.selection().clone();
            let execution = self.app.view.execution().with_mode(mode);
            self.app.view.set_draft(selection, execution);
        }
    }

    /// plan 开关（修复线 F20）：有会话时 `SetPlan`，以 `PlanChanged` 为准；没有会话时改新会话的草稿。
    pub(super) fn set_plan(&mut self, enabled: bool) {
        if self.session.is_some() {
            self.send(Sent::SetPlan, Command::SetPlan { enabled }, String::new());
        } else {
            let selection = self.app.view.selection().clone();
            let execution = self.app.view.execution().with_plan(enabled);
            self.app.view.set_draft(selection, execution);
        }
    }

    /// `/clear`：运行中先中断；离开当前会话，清屏，下一次发送建新会话（会话还留在存储里）。
    pub(super) fn clear(&mut self) -> Result<()> {
        self.clear_into(None)
    }

    /// 同 `/clear`；给了工作区（← 项目页的 `n`，tui-panels.md §2.4）时新会话的草稿换到那里，提示写上路径。
    pub(super) fn clear_into(
        &mut self,
        workspace: Option<(rust_cli_kernel_api::env::WorkspaceRef, String)>,
    ) -> Result<()> {
        if self.app.running() {
            self.send(Sent::Interrupt, Command::Interrupt, String::new());
        }
        // SessionEnd(clear)；下一次发送建的会话 SessionStart 来源是 clear。
        self.leave(rust_cli_kernel_api::session_api::EndReason::Clear);
        self.next_start = rust_cli_kernel_api::session_api::StartSource::Clear;
        self.app.reset();
        self.cache.clear();
        self.terminal
            .clear()
            .map_err(|error| anyhow::anyhow!("clearing the terminal: {error}"))?;
        match workspace {
            Some((workspace, label)) => {
                self.app.view.set_draft_workspace(workspace);
                self.app
                    .info(crate::ui::i18n_projects::started_in(self.app.lang, &label));
            }
            None => self.app.info(self.app.strings.cleared),
        }
        Ok(())
    }

    /// 画完一帧后复制选区（选中的文字在绘制时算出）。只有空白时不复制；`clear` 时复制后清掉选区。
    pub(super) fn copy_selection(&mut self, clear: bool) {
        let Some(text) = self.app.hits.selected.clone() else {
            return;
        };
        self.app.mark_copied();
        if !text.trim().is_empty() {
            self.copy_text(text);
        }
        if clear {
            self.app.clear_selection();
        }
        self.dirty = true;
    }

    /// 剪贴板的唯一入口（F25，tui-select.md §3.2）：终端写 OSC 52（`Screen::copy`），入口的 `Clipboard` 服务写系统剪贴板
    /// 与 tmux 缓冲区，按它回答的途径提示。没有注入服务时只有 OSC 52。
    ///
    /// 缺陷修复（F25）：原来只写 OSC 52，不支持 OSC 52 的终端（macOS Terminal.app、没开 `set-clipboard` 的 tmux）里
    /// 剪贴板不变，提示却说已发送。
    pub(super) fn copy_text(&mut self, text: String) {
        self.screen.copy(&text);
        match self.options.services.clipboard.clone() {
            Some(clipboard) => self.pending.push(
                async move {
                    let route = clipboard.copy_text(text.clone()).await;
                    super::session::Done::Copied { route, text }
                }
                .boxed(),
            ),
            None => self.copied(crate::ui::clipboard::NO_SERVICE, &text),
        }
    }

    /// 复制完成：显示提示。
    pub(super) fn copied(&mut self, route: crate::services::CopyRoute, text: &str) {
        let notice = crate::ui::clipboard::notice(route, text, &self.options.vars, self.app.lang);
        self.app.copied = Some((notice, Instant::now()));
        self.dirty = true;
    }
}
