//! 输入与动作（docs/specs/rust-cli/tui.md §5、§6）：终端事件交给界面状态，界面产生的 [`Action`] 在这里
//! 变成内核命令或终端操作。模型与模式有会话时以内核事件为准，没有会话时只改新会话的草稿。
use std::time::Instant;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use futures_util::FutureExt;
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

impl Loop<'_> {
    pub(super) fn on_input(&mut self, event: Event) -> Result<()> {
        let now = Instant::now();
        let tick = tokio::time::Instant::now();
        self.app.notify.input(tick);
        // 提问框的空闲计时（工作线 F72）：按键、粘贴与鼠标点击重新计时，焦点变化照终端的报告。
        match &event {
            Event::Key(_) | Event::Paste(_) => self.app.afk.input(tick),
            Event::Mouse(mouse) if mouse.kind != MouseEventKind::Moved => self.app.afk.input(tick),
            Event::FocusGained | Event::FocusLost => {
                let focused = matches!(event, Event::FocusGained);
                self.app.afk.focus(focused);
                self.cockpit_focus(focused);
                let open = !crate::ui::interaction_view::open_asks(&self.app).is_empty();
                self.app.notify.focus(focused, open);
            }
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
                // 与界面同一个归一（tui-input.md §9.1）：Windows 的 AltGr 字符不是 ctrl+c。
                self.cancel_loops_on(&crate::ui::keyboard::normalize(key, self.options.os));
                actions.extend(self.app.on_terminal_key(key, now));
            }
            Event::Paste(text) => actions.extend(self.app.on_paste(&text)),
            Event::Mouse(mouse) => actions.extend(self.app.on_mouse(mouse, now)),
            // 尺寸变了：全屏视图的选区（屏幕坐标）不再对应原来的文字（tui-select.md §2.1）。
            Event::Resize(..) => self.app.pointer.screen_selection = None,
            Event::FocusGained | Event::FocusLost => self.app.focus_changed(),
        }
        self.dirty = true;
        for action in actions {
            self.act(action)?;
        }
        self.fetch_at();
        Ok(())
    }

    /// 到了 paste burst 的冲刷时刻（tui-input.md §9.2）：交出的粘贴或扣住的字符照常处理。
    pub(super) fn on_burst_due(&mut self) -> Result<()> {
        let now = Instant::now();
        self.dirty = true;
        for action in self.app.flush_burst(now) {
            self.act(action)?;
        }
        self.fetch_at();
        Ok(())
    }

    /// 空闲时 esc / ctrl+c 也取消本会话的 loop 唤醒（tools-more.md §18.1）。有没有唤醒由内核判断，
    /// 回执不提示；按键照常往下处理（TUI 不持有任务表的投影，D41）。运行中（画面上）的中断由按键本身发出。
    fn cancel_loops_on(&mut self, key: &KeyEvent) {
        let cancel = match key.code {
            KeyCode::Esc => key.modifiers.is_empty(),
            KeyCode::Char('c') => crate::ui::keyboard::ctrl(key),
            _ => false,
        };
        let idle = self.outbox.session().is_some()
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
            self.send(Sent::CancelLoops, Command::Interrupt);
        }
    }

    /// 发出的文字：目录里有 SKILL.md 路径的技能的 `$名字` 写成桌面端的链接（`skill_mention::to_wire`）。
    fn mention_wire(&self, text: &str) -> String {
        let catalog = self.app.composer.mentions.catalog.as_deref().unwrap_or(&[]);
        crate::ui::skill_mention::to_wire(text, catalog)
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
                // 历史里记打出的草稿；发出的是 `$` 提及的链接写法（tui-input.md §1.4）。
                self.submit(Command::Submit {
                    text: self.mention_wire(&text),
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
                    text: self.mention_wire(&text),
                    attachments: Vec::new(),
                    delivery,
                });
            }
            Action::BackgroundForeground => self.send(
                Sent::Record,
                Command::BackgroundForeground {
                    reason: rust_cli_kernel_api::env::BackgroundReason::User,
                },
            ),
            // 只投递排着的插话（F105）：与有文字时的立即发送同一个原因，只是不先入队。
            Action::DeliverQueued => self.send(
                Sent::Record,
                Command::BackgroundForeground {
                    reason: rust_cli_kernel_api::env::BackgroundReason::DeliverMessage,
                },
            ),
            // esc / ctrl+c 的中断：回合结束时据此判断要不要撤回（修复线 F38，restore.rs）。
            Action::TakeBack { command, text } => {
                self.send(Sent::Dequeue(text), Command::Dequeue { command });
            }
            Action::Interrupt => {
                self.user_cancel = true;
                self.send(Sent::Interrupt, Command::Interrupt);
            }
            Action::Many(actions) => {
                for action in actions {
                    self.act(action)?;
                }
            }
            Action::ResolvePeer { message, approve } => self.send(
                Sent::Resolve(String::new()),
                Command::ResolvePeerMessage { message, approve },
            ),
            // 旧会话的询问（agents.md §4.1.2，工作线 F72）经内核的进程级入口回答，回执同会话的回答。
            Action::Resolve {
                interaction,
                answer,
            } if self.app.detached_title(&interaction).is_some() => {
                let kernel = self.kernel.clone();
                // 回执带请求的 id：被拒绝时那个请求的回答重新可选（`App::answer_failed`）。
                let id = interaction.to_string();
                self.pending.push(
                    async move {
                        let ack = match kernel.resolve_detached(&interaction, answer).await {
                            Ok(()) => rust_cli_kernel_api::session_api::Ack::Accepted {
                                result: rust_cli_kernel_api::session_api::CommandResult::Done,
                            },
                            Err(error) => rust_cli_kernel_api::session_api::Ack::Rejected {
                                reason: error.to_string(),
                            },
                        };
                        super::session::Done::Ack {
                            sent: Sent::Resolve(id),
                            ack,
                            command: None,
                        }
                    }
                    .boxed(),
                );
            }
            Action::Resolve {
                interaction,
                answer,
            } => {
                // 回执带请求的 id：被拒绝时那个请求的回答重新可选（`App::answer_failed`）。
                let id = interaction.to_string();
                self.send(
                    Sent::Resolve(id),
                    Command::ResolveInteraction {
                        interaction,
                        answer,
                    },
                )
            }
            Action::ChooseModel { selection, auto } => self.choose_model(selection, None, !auto),
            Action::SetMode(mode) => self.set_mode(mode),
            Action::SetPlan(enabled) => self.set_plan(enabled),
            Action::Compact(instructions) => {
                if self.outbox.session().is_some() {
                    self.send(Sent::Compact, Command::Compact { instructions });
                } else {
                    self.app.info(i18n::not_implemented(lang, "compact"));
                }
            }
            Action::OpenModels => self.catalog(None),
            // 完成时写命令记录（修复线 L-A）；选择器的选择与取消不记（tui-manage.md D27）。
            Action::Clear => self.clear()?,
            // 修复（门道的 `c`）：以前这里一律说 `Copied the last reply`，复制登录网址时也说；这句搬到 `/copy`，门道照
            // Puffin 在槽里说 `Copied the link`。
            Action::Copy(text) => self.copy_text(text),
            Action::CopySelection { clear } => self.copy_after_draw = Some(clear),
            // 整屏重画：丢掉渲染器的基线，下一帧清屏后画每个格子（tui-kit.md §3.3）。
            Action::Redraw => self.renderer.invalidate(),
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
            // 修复：以前没有会话时一律提示 `/rename is not implemented yet.`。还没有会话（启动后、`/clear` 之后、建会话
            // 期间）时改名是新会话草稿里的一条命令，会话建好后在第一条输入之前发出（titles.md §4）；名字写在视图的标题上
            // 只为显示。切换会话期间（`Outbox::switching`，从空白草稿发起的也算）切换完成时不发草稿里的命令，视图也会被
            // 换掉，所以只提示。
            // 改名与它的记录是同一条命令（`RenameRecorded`）：改名被拒绝时记录也不在（MR !3465 评审 CR-01）。
            Action::Rename(title) => {
                let command = super::records::rename(&title);
                if self.outbox.session().is_some() {
                    self.send(Sent::Rename(title), command);
                } else if self.outbox.switching() {
                    self.app.info(i18n::session_opening(lang));
                } else {
                    self.app.info(super::records::renamed(&title));
                    self.app.view.set_draft_title(title);
                    self.outbox.hold(Sent::QuietRename, command);
                }
            }
            // 还没有会话时没有对话可起名：直接给出说明，不发命令（titles.md §8.3）。
            Action::GenerateName => {
                if self.outbox.session().is_some() {
                    self.send(Sent::GenerateName, Command::GenerateName);
                } else if self.outbox.switching() {
                    self.app.info(i18n::session_opening(lang));
                } else {
                    self.app.info(NO_NAME.to_owned());
                }
            }
            // 卡与页打开时：`/status` 打开卡的同时打开了 Session 标签，事实一起问（不显示 Session 时什么也不做）。
            Action::CountContext => {
                self.count_context();
                self.session_facts();
            }
            Action::SessionFacts => self.session_facts(),
            Action::SetLocale(locale) => self.save_locale(locale),
            Action::Login(family) => self.login(family),
            Action::CancelLogin => self.cancel_login(),
            Action::ConfigureKey { family, key } => self.configure_key(family, key),
            Action::ShowGoal => self.show_goal(),
            Action::OpenFork => self.open_fork(),
            Action::Fork { input } => self.fork(input),
            // 挑选回退的点的 fork（tui-pages 接上 fork 与切换会话；接上之前回到挑选，说没能回去）。
            Action::RewindFork { message } => self.rewind_fork(&message),
            Action::Quit => self.app.quit = true,
            Action::Manage(manage) => self.manage(manage)?,
            Action::Panel(panel) => self.panel(panel),
            Action::Projects(request) => self.projects(request),
            Action::Machines(request) => self.machine_request(request),
            Action::Settings(request) => self.settings_request(request),
            Action::PlanUsage(asks) => self.plan_usage(asks),
            Action::UsageBack { row, provider } => self.usage_back(row, provider),
        }
        Ok(())
    }

    pub(super) fn set_model(&mut self, selection: ModelSelection) {
        let label = selection.model.clone();
        if self.outbox.session().is_some() {
            self.send(Sent::SetModel(label), Command::SetModel { selection });
        } else {
            let execution = self.app.view.execution();
            self.app.view.set_draft(selection, execution);
            self.app.info(i18n::model_set(self.app.lang, &label));
            self.release_for_model();
        }
    }

    fn set_mode(&mut self, mode: PermissionMode) {
        if self.outbox.session().is_some() {
            self.send(Sent::SetMode, Command::SetMode { mode });
        } else {
            let selection = self.app.view.selection().clone();
            let execution = self.app.view.execution().with_mode(mode);
            self.app.view.set_draft(selection, execution);
        }
    }

    /// plan 开关（修复线 F20）：有会话时 `SetPlan`，以 `PlanChanged` 为准；没有会话时改新会话的草稿。
    pub(super) fn set_plan(&mut self, enabled: bool) {
        if self.outbox.session().is_some() {
            self.send(Sent::SetPlan, Command::SetPlan { enabled });
        } else {
            let selection = self.app.view.selection().clone();
            let execution = self.app.view.execution().with_plan(enabled);
            self.app.view.set_draft(selection, execution);
        }
    }

    /// `/clear`：运行中先中断；离开当前会话，清屏，下一次发送建新会话（会话还留在存储里）。
    pub(super) fn clear(&mut self) -> Result<()> {
        self.clear_as("")
    }

    /// 同 `/clear`；`args` 是命令的参数（`/clear 名字`）。新会话的对话从这次 `/clear` 的记录开始（对照实现：固定说明、命令输入
    /// 与空的输出，修复线 L-A，tui-manage.md §11.2）；之前留在草稿里的命令在 `clear_into` 里随旧草稿清掉。
    pub(super) fn clear_as(&mut self, args: &str) -> Result<()> {
        self.clear_into(None);
        let record = super::records::record("clear", args, String::new(), true);
        self.outbox
            .hold(Sent::Record, Command::RecordLocalCommand { record });
        Ok(())
    }

    /// 离开现在的会话（`/clear` 的前半）。给了工作区（← 的看板上的 ⌃b，tui-panels.md §2.4）时新会话的草稿换到那里（`/cd`
    /// 没有会话时的那条路径），提示写上路径。
    pub(super) fn clear_into(
        &mut self,
        workspace: Option<(rust_cli_kernel_api::env::WorkspaceRef, String)>,
    ) {
        if self.app.running() {
            self.send(Sent::Interrupt, Command::Interrupt);
        }
        // SessionEnd(clear)；下一次发送建的会话 SessionStart 来源是 clear。
        self.leave(rust_cli_kernel_api::session_api::EndReason::Clear);
        self.next_start = rust_cli_kernel_api::session_api::StartSource::Clear;
        self.app.reset();
        // 草稿里的命令（记录、`/rename` 的改名）属于被换掉的草稿，一起清掉。修复（MR !3465 评审 CR-01）：以前只有
        // `/clear` 清，← 看板的 ⌃b 也走这里却不清，旧草稿的改名在 ⌃b 建的会话里照样发出。
        self.outbox.clear_held();
        self.cache.clear();
        self.renderer.invalidate();
        match workspace {
            Some((workspace, label)) => {
                self.app.view.set_draft_workspace(workspace);
                self.app
                    .info(crate::ui::i18n_projects::started_in(self.app.lang, &label));
            }
            None => self.app.info(self.app.strings.cleared),
        }
    }

    /// 画完一帧后复制选区（选中的文字在绘制时算出）。只有空白时不复制；`clear` 时复制后清掉选区。
    pub(super) fn copy_selection(&mut self, clear: bool) {
        let Some(text) = self.app.pointer.hits.selected.clone() else {
            return;
        };
        self.app.mark_copied();
        if !text.trim().is_empty() {
            self.copy_text(text);
        }
        if clear {
            self.app.pointer.clear_selection();
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
        let clipboard = self.options.services.clipboard.clone();
        self.pending.push(
            async move {
                let route = clipboard.copy_text(text.clone()).await;
                super::session::Done::Copied { route, text }
            }
            .boxed(),
        );
    }

    /// 复制完成：槽里是 `Copied N characters`（选区还在时，`footer::slot`），哪条途径都一样（Puffin 只这样说）。全屏视图
    /// 照旧在最后一行的右侧画提示。输入框上方安静的一行（旧界面的能力，Puffin 没有，tui-kit.md §4.3）：OSC 52 的粘贴在
    /// VS Code 1.123/1.124 会乱码时的警告，与不是系统剪贴板时途径的说明（tmux 缓冲区 `paste with prefix + ]`、只有 OSC 52
    /// 时 `hold <键> while selecting`，`clipboard::route_line`）。
    ///
    /// 缺陷修复（tui-transcript，2026-10-31）：Puffin 化时途径的说明随旧的临时提示一起删了，复制到 tmux 缓冲区或只发了
    /// OSC 52 时用户不知道怎样粘贴回来（HEAD 的能力）；现在照 Puffin 最近的样子写在安静的一行，下一次发送时清掉。
    pub(super) fn copied(&mut self, route: crate::services::CopyRoute, text: &str) {
        let vars = &self.options.vars;
        let notice = crate::ui::clipboard::notice(route, text, vars, self.app.lang);
        let warning = notice
            .text
            .split(" · ")
            .next()
            .filter(|w| w.starts_with('⚠'))
            .map(str::to_owned);
        let lines: Vec<String> = warning
            .into_iter()
            .chain(crate::ui::clipboard::route_line(route, vars, self.app.lang))
            .collect();
        // 接在命令说的话（`Copied the last reply`）下面；选区复制时它们自己成行。
        if !lines.is_empty() {
            let said = match self.app.said.quiet.take() {
                Some(was) => format!("{was}\n{}", lines.join("\n")),
                None => lines.join("\n"),
            };
            self.app.said.quiet = Some(said);
        }
        self.app.pointer.copied = Some((notice, Instant::now()));
        self.dirty = true;
    }
}
