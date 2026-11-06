//! L28 的管理命令在 runner 里的部分（docs/specs/rust-cli/tui-display.md §1、§3、§4）：问内核、按回执更新界面。
//! 后台任务与子代理列表是同一份快照（修复线 F34），查询与回执在 `panels.rs`。
use futures_util::FutureExt;
use rust_cli_kernel_api::manage::McpServerInfo;
use rust_cli_kernel_api::session_api::{Ack, Command, Delivery, KernelError, KernelEvent};

use super::Loop;
use super::session::{Done, Sent};
use crate::ui::i18n;
use crate::ui::manage::Manage;

/// 管理命令的异步结果。
pub(super) enum ManageDone {
    /// 停止的回执。
    Stopped { label: String, ack: Ack },
    /// `/plan 描述`：模式切换的回执到了之后才发输入（两条命令有先后）。
    Planned { ack: Ack, text: String },
    /// `/plan open` 的计划文件（tui-manage.md §8）；`entering` 是这次刚切到 plan。
    PlanFile {
        entering: bool,
        file: Result<rust_cli_kernel_api::manage::PlanFile, KernelError>,
    },
    /// 编辑器关掉了计划文件。
    PlanOpened {
        path: String,
        result: Result<(), String>,
    },
    /// `/cd` 的回执（tui-manage.md §7）。
    /// `/cd` 的结果与命令的参数（命令记录用，修复线 L-A）。
    DirectoryChanged {
        path: String,
        result: Result<rust_cli_kernel_api::manage::DirectoryChanged, KernelError>,
    },
    /// `/mcp` 控制的前置快照（tui-manage.md §6.2）。
    McpPlanned {
        args: String,
        servers: Vec<McpServerInfo>,
    },
    /// `/mcp` 控制的回执：写配置失败的服务器与改了连接的服务器状态。
    McpControlled {
        run: crate::ui::mcp_control::Run,
        infos: Result<Vec<McpServerInfo>, KernelError>,
        failed: Vec<(String, String)>,
    },
    /// `/diff` 的未提交改动（tui-manage.md §4）。
    Diff(Result<rust_cli_kernel_api::env_git::GitDiff, KernelError>),
    /// `/export` 的写入回执（tui-manage.md §3）。
    Exported(Result<String, KernelError>),
    /// `theme.json` 写完（tui-kit.md §3.4）。
    ThemeSaved {
        choice: rust_cli_tui_kit::theme::scheme::ThemeChoice,
        result: Result<(), String>,
    },
    /// `/autocompact` 的写入回执：`Ok(Some(原因))` 存下了但运行的会话不按它（auto_compact.rs）。
    AutoCompactSaved {
        at: Option<u64>,
        result: Result<Option<String>, String>,
    },
}

/// 会让后台任务变化的事件：工具结束（可能启动了后台命令或子代理）、后台工作的终态、回合结束。
pub(super) fn touches_background(event: &KernelEvent) -> bool {
    matches!(
        event,
        KernelEvent::ToolDone { .. }
            | KernelEvent::BackgroundWork { .. }
            | KernelEvent::TurnFinished { .. }
    )
}

impl Loop<'_> {
    pub(super) fn manage(&mut self, manage: Manage) -> anyhow::Result<()> {
        match manage {
            // `/tasks`：打开任务卡，顺便重取一次快照（tui-tasks.md §3.1）。
            Manage::Tasks => {
                self.app.open_tasks();
                self.refresh_tasks();
            }
            Manage::StopTask { id, label } => {
                let Some(session) = self.outbox.session().cloned() else {
                    return Ok(());
                };
                let envelope = self.envelope(Command::StopTask { task: id });
                self.pending.push(
                    async move {
                        let ack = session.send(envelope).await;
                        Done::Manage(ManageDone::Stopped { label, ack })
                    }
                    .boxed(),
                );
            }
            Manage::Usage => self.open_usage(),
            Manage::UsageRange { range, seq } => self.usage_range(range, seq),
            // 先改名，再离开（tui-display.md §4.6）。改名的命令先进 actor 的队列。
            Manage::ClearNamed(name) => {
                let name_args = name.clone();
                self.send(Sent::QuietRename, Command::Rename { title: name });
                self.clear_as(&name_args)?;
            }
            Manage::PlanPrompt(text) => match self.outbox.session().cloned() {
                Some(session) => {
                    let mode = self.envelope(Command::SetPlan { enabled: true });
                    self.pending.push(
                        async move {
                            let ack = session.send(mode).await;
                            Done::Manage(ManageDone::Planned { ack, text })
                        }
                        .boxed(),
                    );
                }
                // 没有会话：新会话直接打开 plan 建（草稿，权限模式不变）。
                None => {
                    let selection = self.app.view.selection().clone();
                    let execution = self.app.view.execution().with_plan(true);
                    self.app.view.set_draft(selection, execution);
                    // 修 bug（F140）：以前直接建会话，没有模型或缺省模型还没定下时也建（回合因为没有模型失败）；现在同
                    // 输入框的发送走 `submit`，该留下时留下。
                    self.submit(Command::Submit {
                        text,
                        attachments: Vec::new(),
                        delivery: Delivery::Queue,
                    });
                }
            },
            // 先切到 plan，再看计划文件；有内容才交给编辑器。
            Manage::PlanOpen => {
                // 修复线 F20：只打开 plan 开关，权限模式不变。
                let entering = !self.app.view.plan();
                if entering {
                    self.act(crate::ui::app::Action::SetPlan(true))?;
                }
                match self.outbox.session().cloned() {
                    Some(session) => {
                        let kernel = self.kernel.clone();
                        let query = rust_cli_kernel_api::manage::PlanFileQuery {
                            workspace: self.app.view.workspace().clone(),
                            session: session.id().clone(),
                        };
                        self.pending.push(
                            async move {
                                let file = kernel.plan_file(query).await;
                                Done::Manage(ManageDone::PlanFile { entering, file })
                            }
                            .boxed(),
                        );
                    }
                    None => {
                        if let Some(says) = crate::ui::manage::no_plan(self.app.lang, entering) {
                            self.app.info(says);
                        }
                    }
                }
            }
            Manage::ChangeDirectory(path) => {
                let kernel = self.kernel.clone();
                let request = rust_cli_kernel_api::manage::ChangeDirectory {
                    workspace: self.app.view.workspace().clone(),
                    session: self.outbox.session().map(|session| session.id().clone()),
                    path: path.clone(),
                };
                self.pending.push(
                    async move {
                        Done::Manage(ManageDone::DirectoryChanged {
                            path,
                            result: kernel.change_directory(request).await,
                        })
                    }
                    .boxed(),
                );
            }
            Manage::McpControl(args) => {
                let kernel = self.kernel.clone();
                self.pending.push(
                    async move {
                        let servers = kernel.mcp_servers().await;
                        Done::Manage(ManageDone::McpPlanned { args, servers })
                    }
                    .boxed(),
                );
            }
            Manage::Diff => {
                let kernel = self.kernel.clone();
                let workspace = self.app.view.workspace().clone();
                self.pending.push(
                    async move {
                        Done::Manage(ManageDone::Diff(kernel.uncommitted_diff(&workspace).await))
                    }
                    .boxed(),
                );
            }
            Manage::Export(path) => {
                let (text, _) = crate::ui::export::conversation_text(&self.app, self.now_ms());
                let kernel = self.kernel.clone();
                let file = rust_cli_kernel_api::manage::ExportFile {
                    workspace: self.app.view.workspace().clone(),
                    path,
                    text,
                };
                self.pending.push(
                    async move { Done::Manage(ManageDone::Exported(kernel.write_export(file).await)) }
                        .boxed(),
                );
            }
            // 缺省文件名带时间：按注入的时钟拼（F7：以前界面层直接读系统时间）。
            Manage::ExportName => {
                let now = self.now_ms();
                let offset = self.options.clock.utc_offset_minutes(now);
                self.app.open_export_name(now, offset);
            }
            // 与拖选同一个剪贴板入口（`copy_text`：OSC 52 与系统剪贴板，tui-select.md §3.2）。
            Manage::AutoCompact(at) => self.auto_compact(at),
            Manage::ExportCopy => {
                let (text, _) = crate::ui::export::conversation_text(&self.app, self.now_ms());
                // 复制的反馈同拖选（Puffin `copied_says`：`Copied N lines`）。
                self.app.info(crate::ui::footer::copied_says(
                    self.app.lang,
                    text.trim_end_matches('\n'),
                ));
                self.copy_text(text);
            }
            // 墨色已经换上（同 Puffin：写不成时主题仍然换了），这里重画并写 `theme.json`（tui-kit.md §3.4）；同时只有一次
            // 写入在途，在途期间又选的只留最后一个（`latest.rs`）。没有存储时只对本次运行有效。
            Manage::SetTheme(choice) => {
                self.manage(Manage::Repaint)?;
                match self.options.services.stores.clone() {
                    Some(stores) => {
                        let Some(choice) = self.theme.start(choice) else {
                            return Ok(());
                        };
                        let saving =
                            stores.write(crate::services::StoreFile::Theme, choice.encode());
                        self.pending.push(
                            async move {
                                let result = saving.await;
                                Done::Manage(ManageDone::ThemeSaved { choice, result })
                            }
                            .boxed(),
                        );
                    }
                    None => self
                        .app
                        .info(crate::ui::slash_theme::theme_set(self.app.lang, choice)),
                }
            }
            Manage::Repaint => {
                self.cache.clear();
                self.renderer.invalidate();
            }
        }
        Ok(())
    }

    /// `/mcp enable/disable`：先逐个写配置（tui-manage.md §6.1），写成功的再交给内核改连接；reconnect 直接改。
    fn mcp_run(&mut self, run: crate::ui::mcp_control::Run) {
        let kernel = self.kernel.clone();
        let preferences = self.options.services.preferences.clone();
        self.pending.push(
            async move {
                let mut failed = Vec::new();
                let mut changed = Vec::new();
                for name in &run.names {
                    let saved = match run.persist() {
                        Some(enabled) => preferences.save_mcp_enabled(name.clone(), enabled).await,
                        None => Ok(None),
                    };
                    match saved {
                        Ok(_) => changed.push(name.clone()),
                        Err(reason) => failed.push((name.clone(), reason)),
                    }
                }
                let infos = if changed.is_empty() {
                    Ok(Vec::new())
                } else {
                    let control = rust_cli_kernel_api::manage::McpControl {
                        action: run.action,
                        servers: changed,
                    };
                    kernel.mcp_control(control).await
                };
                Done::Manage(ManageDone::McpControlled { run, infos, failed })
            }
            .boxed(),
        );
    }

    pub(super) fn on_manage(&mut self, done: ManageDone) -> anyhow::Result<()> {
        let lang = self.app.lang;
        match done {
            ManageDone::Stopped { label, ack } => {
                match ack {
                    Ack::Accepted { .. } | Ack::Duplicate => self.app.info(match lang {
                        i18n::Lang::En => format!("Stopped {label}"),
                        i18n::Lang::Zh => format!("已停止 {label}"),
                    }),
                    Ack::Rejected { reason } => self.app.error(reason),
                }
                // 面板、footer 与子代理列表随新的快照一起更新（修复线 F34：同一份快照）。
                self.refresh_tasks();
            }
            ManageDone::PlanFile { entering, file } => match file {
                Ok(file) if file.exists => {
                    let editor = self.options.services.editor.clone();
                    self.hand_over();
                    let path = file.path;
                    self.pending.push(
                        async move {
                            let result = editor.open(path.clone()).await;
                            Done::Manage(ManageDone::PlanOpened { path, result })
                        }
                        .boxed(),
                    );
                }
                Ok(_) => {
                    if let Some(says) = crate::ui::manage::no_plan(lang, entering) {
                        self.app.info(says);
                    }
                }
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            ManageDone::PlanOpened { path, result } => {
                self.take_back();
                match result {
                    Ok(()) => self.app.info(match lang {
                        i18n::Lang::En => format!("Opened plan in editor: {path}"),
                        i18n::Lang::Zh => format!("已在编辑器里打开计划：{path}"),
                    }),
                    Err(reason) => self.app.error(reason),
                }
            }
            // 有会话时工作区随 `WorkspaceChanged` 换（actor 在回执之前发出）；没有会话时换新会话的草稿。修复（R1-3）：
            // 以前这里改 TUI 自存的工作区副本，EnterWorktree / ExitWorktree 不经这里，副本就过时了。
            ManageDone::DirectoryChanged { path, result } => match result {
                Ok(moved) => {
                    if self.outbox.session().is_none() {
                        let mut workspace = self.app.view.workspace().clone();
                        workspace.root = moved.root.clone();
                        self.app.view.set_draft_workspace(workspace);
                        // 有会话时 runtime 与说明文字一起记；还没有会话时记在新会话的草稿里（修复线 L-A）。
                        self.record_command(super::records::record(
                            "cd",
                            &path,
                            format!(
                                "Moved to {}",
                                rust_cli_kernel_api::user_command::bold(&moved.display)
                            ),
                            false,
                        ));
                    }
                    self.app.info(match lang {
                        i18n::Lang::En => format!("Moved to {}", moved.display),
                        i18n::Lang::Zh => format!("已移到 {}", moved.display),
                    });
                }
                Err(error) => self.app.info(error.to_string()),
            },
            ManageDone::McpPlanned { args, servers } => {
                match crate::ui::mcp_control::plan(&args, &servers, lang) {
                    crate::ui::mcp_control::Plan::Say(text) => self.app.info(text),
                    crate::ui::mcp_control::Plan::Run(run) => self.mcp_run(run),
                }
            }
            ManageDone::McpControlled { run, infos, failed } => match infos {
                Ok(infos) => self
                    .app
                    .info(crate::ui::mcp_control::result(&run, &infos, &failed, lang)),
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            ManageDone::Diff(diff) => self
                .app
                .diff_loaded(diff.map_err(|error| error.to_string())),
            ManageDone::Exported(result) => match result {
                Ok(path) => {
                    let shown = self.app.tilde(&path);
                    self.app.info(crate::ui::export::exported(lang, &shown));
                }
                Err(error) => self
                    .app
                    .error(crate::ui::export::export_failed(lang, &error.to_string())),
            },
            ManageDone::ThemeSaved { choice, result } => {
                let next = self.theme.finish();
                match result {
                    Ok(()) => self
                        .app
                        .info(crate::ui::slash_theme::theme_set(lang, choice)),
                    Err(reason) => self
                        .app
                        .error(crate::ui::slash_theme::theme_failed(lang, &reason)),
                }
                if let Some(next) = next {
                    self.manage(Manage::SetTheme(next))?;
                }
            }
            ManageDone::AutoCompactSaved { at, result } => self.auto_compact_saved(at, result),
            ManageDone::Planned { ack, text } => match ack {
                Ack::Accepted { .. } | Ack::Duplicate => self.send(
                    Sent::Submit(String::new()),
                    Command::Submit {
                        text,
                        attachments: Vec::new(),
                        delivery: Delivery::Queue,
                    },
                ),
                Ack::Rejected { reason } => self.app.error(i18n::rejected(lang, &reason)),
            },
        }
        Ok(())
    }
}
