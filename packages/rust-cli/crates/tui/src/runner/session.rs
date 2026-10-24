//! 会话操作（docs/specs/rust-cli/tui.md §4.1）：建会话、发命令、取快照、读模型目录。每个操作是一个
//! future，结果作为 [`Done`] 回到事件循环；状态只在事件循环里改。
use std::sync::Arc;

use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::provider::ModelCatalog;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, KernelError, SessionHandle, SessionView,
    StartSource,
};

use super::{Loop, idle};
use crate::ui::i18n;
use crate::view::Transcript;

/// 命令的种类（回执的提示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sent {
    Submit,
    Interrupt,
    Resolve,
    SetModel,
    SetMode,
    SetPlan,
    Compact,
    /// rewind；回执的 `label` 是要放回输入框的文字（只回退代码时为空）。
    Rewind,
    /// `/rename`：回执后提示新名字（`label`）。
    Rename,
    /// 空闲时 esc / ctrl+c 取消 loop 唤醒（tools-more.md §18.1）：回执不提示。
    CancelLoops,
    /// 不带名字的 `/rename`：回执的文本（`result.message` 或 `reason`）原样显示（titles.md §8.6）。
    GenerateName,
    /// `/fork`：回执的 `result` 是 `{session, response, …}`，到达后切到新会话。
    Fork,
    /// `/clear 名字` 给旧会话改名（tui-display.md §4.6）：成功不提示，失败照常报错。
    ClearRename,
    /// esc 撤回（修复线 F38）：回执的 `result.message` 放回输入框；条件不满足被拒绝时不提示。
    RestoreCancelled,
}

/// 异步操作的结果。
pub(super) enum Done {
    /// 新会话已建好并打开；接着发第一条输入（`Submit`、`SubmitContent` 或 `Bash`）。
    Opened {
        handle: Arc<dyn SessionHandle>,
        command: Command,
    },
    OpenFailed(KernelError),
    Ack {
        sent: Sent,
        label: String,
        ack: Ack,
    },
    /// 快照（`HistoryRewritten` 之后）。装箱：快照比其他变体大得多。
    View(Box<Result<SessionView, KernelError>>),
    /// 模型目录到达；`pick` 不为空时按名称选模型，否则打开列表。
    Catalog {
        catalog: ModelCatalog,
        pick: Option<String>,
    },
    /// rewind 选择器的条目。
    Rewinds(Result<Vec<crate::ui::rewind::Entry>, KernelError>),
    /// 切到了另一个会话（装箱：快照很大）。
    Switched(Box<Result<super::lifecycle::Switched, KernelError>>),
    /// 离开会话时 SessionEnd 给出的提示。
    Notices(Vec<String>),
    /// 入口注入的服务（界面语言等，`services.rs`）。
    Service(super::services::ServiceDone),
    /// 内核给出的 skill 命令（斜杠补全与发送前的判断，tui.md §6）。
    Commands(Vec<rust_cli_kernel_api::user_command::UserCommand>),
    /// `@` 补全的文件清单（tui-input.md §1.2）；取失败时为 None（保留旧索引）。
    Files(Option<Vec<String>>),
    /// 剪贴板里的图片（tui-input.md §3.1）。
    Image(Result<Option<crate::services::ImageBytes>, String>),
    /// 文字复制完了：入口回答的途径（tui-select.md §3.2）。
    Copied {
        route: crate::services::CopyRoute,
        text: String,
    },
    /// 外部编辑器结束（tui-input.md §4.1）：编辑后的文字或失败的原因。
    Edited(Result<String, String>),
    /// `/memory` 的文件已用编辑器打开过（tui-input.md §4.2）。
    MemoryOpened {
        display: String,
        result: Result<(), String>,
    },
    /// 粘贴的路径读出的图片；不是图片时把 `text` 当文字粘贴。
    ImageFile {
        image: Option<crate::services::ImageBytes>,
        text: String,
    },
    /// `/goal` 面板与 `/fork` 选择器的数据（fork_goal.rs）。
    ForkGoal(super::fork_goal::ForkGoalDone),
    /// L28 的管理命令（`manage.rs`）。
    Manage(super::manage::ManageDone),
    /// L30 的面板（`panels.rs`）。
    Panel(super::panels::PanelDone),
    /// ← 项目页与 `/resume` 页（`projects.rs`，修复线 F29）。
    Projects(super::projects::ProjectsDone),
    /// `/usage` 的查询（usage.md §4）。
    Usage(super::usage::UsageDone),
    /// `/model` 的切换回执与默认模型的写回（`model_default.rs`）。
    Model(super::model_default::ModelDone),
    /// 退出信号之后发了中断：actor 回复时回合是否还在跑（session-lifecycle.md §3.5）。
    Stopping {
        running: bool,
    },
}

/// 执行环境路径的分隔符：工作区根是盘符或 `\\` 开头时是 `\`（Windows），否则是 `/`。
fn path_separator(root: &str) -> char {
    let bytes = root.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if drive || root.starts_with("\\\\") {
        '\\'
    } else {
        '/'
    }
}

/// 命令 ID 经注入的 Ids（F7，审查 R2-11：以前直接 `Uuid::new_v4`；本机实现仍是不带前缀的 uuid）。
pub(super) fn envelope(
    ids: &dyn rust_cli_kernel_api::clock::Ids,
    command: Command,
) -> CommandEnvelope {
    CommandEnvelope {
        id: ids.command(),
        command,
    }
}

impl<B: Backend> Loop<'_, B> {
    /// 发一条命令；没有会话时不发（调用方先判断）。`label` 是回执提示里的内容（例如模型名）。
    pub(super) fn send(&mut self, sent: Sent, command: Command, label: String) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let envelope = self.envelope(command);
        self.pending.push(
            async move {
                let ack = session.send(envelope).await;
                Done::Ack { sent, label, ack }
            }
            .boxed(),
        );
    }

    /// 第一次发送：建会话、打开，再发第一条输入（tui.md §4.1）。创建期间 `creating` 为真。
    pub(super) fn create(&mut self, command: Command) {
        self.app.creating = true;
        let kernel = self.kernel.clone();
        // `/clear` 之后的新会话：SessionStart 的来源是 clear（session-lifecycle.md §3.2）。
        let source = std::mem::replace(&mut self.next_start, StartSource::Startup);
        let request = CreateSession {
            id: None,
            workspace: self.app.view.workspace().clone(),
            selection: self.app.view.selection().clone(),
            mode: self.app.view.mode(),
            plan: self.app.view.plan(),
        };
        self.pending.push(
            async move {
                let opened = async {
                    let id = kernel.create_session_as(request, source).await?;
                    kernel.open_session(&id).await
                };
                match opened.await {
                    Ok(handle) => Done::Opened { handle, command },
                    Err(error) => Done::OpenFailed(error),
                }
            }
            .boxed(),
        );
    }

    /// 历史被改写：丢弃视图，取快照后从快照的 seq 重新订阅。等快照期间不接收事件。
    pub(super) fn refetch(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.events = idle();
        self.pending
            .push(async move { Done::View(Box::new(session.view().await)) }.boxed());
    }

    /// 读模型目录。没有目录时（入口用显式的 Anthropic 端点）：`/model 名称` 直接换成这个模型，
    /// `/model` 只能提示。
    pub(super) fn catalog(&mut self, pick: Option<String>) {
        let Some(models) = self.options.models.clone() else {
            match pick {
                Some(model) => {
                    let provider = self.app.view.selection().provider.clone();
                    self.set_model(rust_cli_kernel_api::model::ModelSelection {
                        provider,
                        model,
                        reasoning: None,
                        connection: None,
                    });
                }
                None => self.app.info(self.app.strings.no_models),
            }
            return;
        };
        self.pending.push(
            async move {
                Done::Catalog {
                    catalog: models.catalog().await,
                    pick,
                }
            }
            .boxed(),
        );
    }

    /// 取 skill 命令的清单（启动时与每个回合结束时；skill 文件可能在回合里被改）。
    pub(super) fn fetch_commands(&mut self) {
        let kernel = self.kernel.clone();
        let workspace = self.app.view.workspace().clone();
        self.pending
            .push(async move { Done::Commands(kernel.user_commands(&workspace).await) }.boxed());
    }

    /// `@` 补全：出现新的 `@` 词且索引过期时取一次清单（tui-input.md §1.2；在途时不重复取）。
    pub(super) fn fetch_files(&mut self, now: std::time::Instant) {
        if !self.app.composer.files.due(now) {
            return;
        }
        let kernel = self.kernel.clone();
        let workspace = self.app.view.workspace().clone();
        self.pending.push(
            async move { Done::Files(kernel.workspace_files(&workspace).await.ok()) }.boxed(),
        );
    }

    pub(super) fn on_done(&mut self, done: Done) {
        self.dirty = true;
        let lang = self.app.lang;
        match done {
            Done::Opened { handle, command } => {
                self.app.creating = false;
                self.app.session = Some(handle.id().as_str().to_owned());
                // 新会话从头订阅：SessionCreated 之后的全部事件。
                self.events = handle.subscribe(Some(0));
                self.session = Some(handle);
                self.attach_panels();
                self.send(Sent::Submit, command, String::new());
                // 创建期间留下的输入：按顺序发出（发送 future 按加入顺序首次轮询，命令按这个顺序进 actor）。
                for held in std::mem::take(&mut self.app.held) {
                    self.submit(held);
                }
            }
            Done::OpenFailed(error) => {
                self.app.creating = false;
                self.app.held.clear();
                self.app.error(i18n::rejected(lang, &error.to_string()));
            }
            Done::Ack { sent, label, ack } => match ack {
                // 中断与取消唤醒的回执不提示：内核只在没有东西可中断时拒绝（空闲时没有唤醒；内核已经结束回合、
                // `TurnFinished` 还没画出来时按 esc）。缺陷修复（F45）：以前运行中的中断被拒绝时显示 `Not done: no turn
                // is running`（tui-input.md §8）。
                _ if matches!(sent, Sent::CancelLoops | Sent::Interrupt) => {}
                Ack::Accepted { result } if sent == Sent::RestoreCancelled => {
                    self.restored(&result)
                }
                _ if sent == Sent::RestoreCancelled => {}
                // 选择器在失败时显示 `Failed to restore …`。
                Ack::Rejected { reason } if sent == Sent::Rewind => {
                    self.app.error(format!("Failed to restore:\n{reason}"));
                }
                Ack::Rejected { reason } if sent == Sent::GenerateName => self.app.info(reason),
                Ack::Rejected { reason } => self.app.error(i18n::rejected(lang, &reason)),
                Ack::Accepted { result } if sent == Sent::GenerateName => {
                    self.app
                        .info(result["message"].as_str().unwrap_or_default().to_owned());
                }
                Ack::Accepted { .. } if sent == Sent::SetModel => {
                    self.app.info(i18n::model_set(lang, &label));
                }
                // 对话被回退：把那条消息放回输入框。
                Ack::Accepted { .. } if sent == Sent::Rewind && !label.is_empty() => {
                    self.app.composer.set_text(&label);
                }
                Ack::Accepted { .. } if sent == Sent::ClearRename => {}
                Ack::Accepted { .. } if sent == Sent::Rename => {
                    self.app.info(format!("Session renamed to: {label}"));
                }
                Ack::Accepted { result } if sent == Sent::Fork => self.forked(&result),
                Ack::Accepted { .. } | Ack::Duplicate => {}
            },
            Done::Rewinds(entries) => match entries {
                Ok(entries) => self.app.open_rewind(entries),
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            Done::Switched(switched) => self.on_switched(*switched),
            Done::Notices(notices) => {
                for notice in notices {
                    self.app.info(notice);
                }
            }
            Done::Service(done) => self.on_service(done),
            Done::ForkGoal(done) => self.on_fork_goal(done),
            Done::Manage(done) => {
                if let Err(error) = self.on_manage(done) {
                    self.app.error(error.to_string());
                }
            }
            Done::Panel(done) => self.on_panel(done),
            Done::Projects(done) => self.on_projects(done),
            Done::Usage(done) => self.on_usage(done),
            Done::Model(done) => self.on_model(done),
            // 回合已经不在跑：直接离开；还在跑时等它的 TurnFinished（`on_event`）。
            Done::Stopping { running } => self.app.quit |= !running,
            Done::Commands(commands) => {
                // 清单第一次到之前提交的命令在这里发出（tui.md §6，修复线 F13）。
                if let Some(action) = self.app.commands_arrived(commands)
                    && let Err(error) = self.act(action)
                {
                    self.app.error(error.to_string());
                }
            }
            Done::Image(result) => self.on_image(result),
            Done::Copied { route, text } => self.copied(route, &text),
            Done::Edited(result) => self.on_edited(result),
            Done::MemoryOpened { display, result } => self.on_memory_opened(&display, result),
            Done::ImageFile { image, text } => self.on_image_file(image, &text),
            Done::Files(files) => {
                let separator = path_separator(self.app.workspace());
                self.app
                    .composer
                    .set_files(files, separator, std::time::Instant::now());
            }
            Done::View(view) => match *view {
                Ok(view) => {
                    let session = self.session.clone();
                    let last = view.last_seq;
                    self.app.view = Transcript::from_view(view, self.now_ms());
                    self.cache.clear();
                    self.app.sync_interaction(std::time::Instant::now());
                    if let Some(session) = session {
                        self.events = session.subscribe(Some(last));
                    }
                }
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            Done::Catalog { catalog, pick } => match pick {
                None => self.app.open_models(&catalog.models),
                Some(name) => match crate::ui::resolve_model(&catalog.models, &name) {
                    Ok(selection) => self.choose_model(selection),
                    Err(rust_cli_platform::model_name::LookupError::Missing) => {
                        self.app.info(i18n::unknown_model(lang, &name))
                    }
                    Err(error) => self.app.info(i18n::ambiguous_model(lang, &error)),
                },
            },
        }
    }
}
