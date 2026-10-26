//! L28 的管理命令在 runner 里的部分（docs/specs/rust-cli/tui-display.md §1、§3、§4）：问内核、按回执更新界面。
//! 后台任务与子代理列表是同一份快照（修复线 F34），查询与回执在 `panels.rs`。
use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::manage::McpServerInfo;
use rust_cli_kernel_api::permission::{
    PermissionRulesChange, PermissionRulesQuery, RuleChange, SourcedRule,
};
use rust_cli_kernel_api::provider::ModelCatalog;
use rust_cli_kernel_api::session_api::{Ack, Command, Delivery, KernelError, KernelEvent};

use super::Loop;
use super::session::{Done, Sent};
use crate::ui::i18n;
use crate::ui::manage::Manage;

/// 管理命令的异步结果。
pub(super) enum ManageDone {
    /// 停止的回执。
    Stopped {
        label: String,
        ack: Ack,
    },
    /// `/effort` 要的模型目录。
    Effort {
        catalog: ModelCatalog,
        args: String,
    },
    Mcp(Vec<McpServerInfo>),
    Status(Vec<McpServerInfo>),
    Hooks(Vec<rust_cli_kernel_api::manage::HookEntry>),
    /// `/plan 描述`：模式切换的回执到了之后才发输入（两条命令有先后）。
    Planned {
        ack: Ack,
        text: String,
    },
    /// `/permissions` 的列表（tui-manage.md §1）。
    PermissionRules(Result<Vec<SourcedRule>, KernelError>),
    /// `/permissions` 的修改回执。
    RuleChanged {
        change: RuleChange,
        result: Result<Vec<SourcedRule>, KernelError>,
    },
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
    /// `ui.theme` 写回完成（tui-manage.md §2.1）。
    ThemeSaved {
        theme: crate::services::UiTheme,
        result: Result<String, String>,
    },
    /// `/effort` 的缺省保存完成（tui-manage.md §2.2）。
    EffortSaved {
        level: Option<String>,
        result: Result<String, String>,
        args: String,
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

impl<B: Backend> Loop<'_, B> {
    pub(super) fn manage(&mut self, manage: Manage) -> anyhow::Result<()> {
        match manage {
            // `/tasks`：打开 Background 面板（与 pill 同一个），顺便重取一次快照（tui-panels.md §3.1）。
            Manage::Tasks => {
                self.app.open_background();
                self.refresh_tasks();
            }
            Manage::StopTask { id, label } => {
                let Some(session) = self.session.clone() else {
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
            Manage::Effort(args) => match self.options.models.clone() {
                Some(models) => self.pending.push(
                    async move {
                        let catalog = models.catalog().await;
                        Done::Manage(ManageDone::Effort { catalog, args })
                    }
                    .boxed(),
                ),
                None => {
                    if let Some(action) = self.app.effort(&[], &args) {
                        self.act(action)?;
                    }
                }
            },
            Manage::Usage { stats } => self.open_usage(stats),
            Manage::UsageRange { range, seq } => self.usage_range(range, seq),
            Manage::Mcp | Manage::Status => {
                let kernel = self.kernel.clone();
                let status = manage == Manage::Status;
                self.pending.push(
                    async move {
                        let servers = kernel.mcp_servers().await;
                        Done::Manage(if status {
                            ManageDone::Status(servers)
                        } else {
                            ManageDone::Mcp(servers)
                        })
                    }
                    .boxed(),
                );
            }
            // 先改名，再离开（tui-display.md §4.6）。改名的命令先进 actor 的队列。
            Manage::ClearNamed(name) => {
                let name_args = name.clone();
                self.send(
                    Sent::ClearRename,
                    Command::Rename { title: name },
                    String::new(),
                );
                self.clear_as(&name_args)?;
            }
            Manage::PlanPrompt(text) => match self.session.clone() {
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
                    self.create(Command::Submit {
                        text,
                        attachments: Vec::new(),
                        delivery: Delivery::Queue,
                    });
                }
            },
            Manage::Hooks => {
                let kernel = self.kernel.clone();
                self.pending.push(
                    async move { Done::Manage(ManageDone::Hooks(kernel.hooks().await)) }.boxed(),
                );
            }
            Manage::Permissions => {
                let kernel = self.kernel.clone();
                let query = PermissionRulesQuery {
                    workspace: self.app.view.workspace().clone(),
                    session: self.session.as_ref().map(|session| session.id().clone()),
                };
                self.pending.push(
                    async move {
                        Done::Manage(ManageDone::PermissionRules(
                            kernel.permission_rules(query).await,
                        ))
                    }
                    .boxed(),
                );
            }
            // 页面在回执到之前停在“保存中”，同时只有一次修改在途（tui-manage.md §1.1）。
            Manage::ChangeRule(change) => {
                let kernel = self.kernel.clone();
                let request = PermissionRulesChange {
                    workspace: self.app.view.workspace().clone(),
                    session: self.session.as_ref().map(|session| session.id().clone()),
                    change: change.clone(),
                };
                self.pending.push(
                    async move {
                        let result = kernel.change_permission_rules(request).await;
                        Done::Manage(ManageDone::RuleChanged { change, result })
                    }
                    .boxed(),
                );
            }
            // 先切到 plan，再看计划文件；有内容才交给编辑器。
            Manage::PlanOpen => {
                // 修复线 F20：只打开 plan 开关，权限模式不变。
                let entering = !self.app.view.plan();
                if entering {
                    self.act(crate::ui::app::Action::SetPlan(true))?;
                }
                match self.session.clone() {
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
                    None => self
                        .app
                        .info(crate::ui::manage::no_plan(self.app.lang, entering)),
                }
            }
            Manage::ChangeDirectory(path) => {
                let kernel = self.kernel.clone();
                let request = rust_cli_kernel_api::manage::ChangeDirectory {
                    workspace: self.app.view.workspace().clone(),
                    session: self.session.as_ref().map(|session| session.id().clone()),
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
            Manage::ExportCopy => {
                let (text, _) = crate::ui::export::conversation_text(&self.app, self.now_ms());
                self.copy_text(text);
                self.app.info(crate::ui::export::copied(self.app.lang));
            }
            // 先写回，成功后再换调色板（与 `/locale` 相同的顺序）；同时只有一次写入在途。
            Manage::SetTheme(theme) => match self.options.services.preferences.clone() {
                Some(store) => {
                    if self.theme_writing {
                        self.theme_next = Some(theme);
                        return Ok(());
                    }
                    self.theme_writing = true;
                    let saving = store.save_theme(theme);
                    self.pending.push(
                        async move {
                            let result = saving.await;
                            Done::Manage(ManageDone::ThemeSaved { theme, result })
                        }
                        .boxed(),
                    );
                }
                None => {
                    self.app.apply_theme(theme);
                    self.app
                        .info(crate::ui::slash_theme::theme_set(self.app.lang, theme));
                    self.manage(Manage::Repaint)?;
                }
            },
            // 本会话先换档位（以 `ModelChanged` 为准），再存成新会话的缺省（与 Node 的顺序相同）。
            Manage::EffortChosen {
                selection,
                level,
                args,
            } => {
                // 档位的切换不是 `/model`：不写 `/model` 的记录（`/effort` 的记录在 `effort_done`）。
                self.act(crate::ui::app::Action::SetModel(selection.clone()))?;
                match self.options.services.preferences.clone() {
                    Some(store) => {
                        let saving = store.save_default_model(selection);
                        self.pending.push(
                            async move {
                                let result = saving.await;
                                Done::Manage(ManageDone::EffortSaved {
                                    level,
                                    result,
                                    args,
                                })
                            }
                            .boxed(),
                        );
                    }
                    None => self.app.effort_done(level.as_deref(), None, &args),
                }
            }
            Manage::Repaint => {
                self.cache.clear();
                self.terminal
                    .clear()
                    .map_err(|error| anyhow::anyhow!("clearing the terminal: {error}"))?;
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
                    let saved = match (run.persist(), &preferences) {
                        (Some(enabled), Some(store)) => {
                            store.save_mcp_enabled(name.clone(), enabled).await
                        }
                        _ => Ok(None),
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
            ManageDone::Effort { catalog, args } => {
                if let Some(action) = self.app.effort(&catalog.models, &args) {
                    self.act(action)?;
                }
            }
            ManageDone::Mcp(servers) => self.app.open_mcp(&servers),
            ManageDone::Hooks(entries) => self.app.open_hooks(&entries),
            ManageDone::Status(servers) => {
                let summary = crate::ui::manage::mcp_summary(&servers, lang);
                self.app.status(summary);
            }
            ManageDone::PermissionRules(rules) => match rules {
                Ok(rules) => self.app.open_permissions(rules),
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            ManageDone::PlanFile { entering, file } => match file {
                Ok(file) if file.exists => {
                    let Some(editor) = self.options.services.editor.clone() else {
                        self.app
                            .error(crate::ui::i18n_input::editor_unavailable(lang).to_owned());
                        return Ok(());
                    };
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
                Ok(_) => self.app.info(crate::ui::manage::no_plan(lang, entering)),
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
                    if self.session.is_none() {
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
                Ok(path) => self.app.info(crate::ui::export::exported(lang, &path)),
                Err(error) => self
                    .app
                    .error(crate::ui::export::export_failed(lang, &error.to_string())),
            },
            ManageDone::ThemeSaved { theme, result } => {
                self.theme_writing = false;
                match result {
                    Ok(_) => {
                        self.app.apply_theme(theme);
                        self.app
                            .info(crate::ui::slash_theme::theme_set(lang, theme));
                        self.manage(Manage::Repaint)?;
                    }
                    Err(reason) => self
                        .app
                        .error(crate::ui::slash_theme::theme_failed(lang, &reason)),
                }
                if let Some(next) = self.theme_next.take() {
                    self.manage(Manage::SetTheme(next))?;
                }
            }
            ManageDone::EffortSaved {
                level,
                result,
                args,
            } => {
                self.app.effort_done(level.as_deref(), Some(result), &args);
            }
            ManageDone::RuleChanged { change, result } => self
                .app
                .permissions_changed(&change, result.map_err(|error| error.to_string())),
            ManageDone::Planned { ack, text } => match ack {
                Ack::Accepted { .. } | Ack::Duplicate => self.send(
                    Sent::Submit,
                    Command::Submit {
                        text,
                        attachments: Vec::new(),
                        delivery: Delivery::Queue,
                    },
                    String::new(),
                ),
                Ack::Rejected { reason } => self.app.error(i18n::rejected(lang, &reason)),
            },
        }
        Ok(())
    }
}
