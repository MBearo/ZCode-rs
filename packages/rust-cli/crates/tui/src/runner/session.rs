//! 会话操作（docs/specs/rust-cli/tui.md §4.1）：建会话、发命令、取快照、读模型目录。每个操作是一个
//! future，结果作为 [`Done`] 回到事件循环；状态只在事件循环里改。
use std::sync::Arc;

use futures_util::FutureExt;
use rust_cli_kernel_api::provider::ModelCatalog;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CommandResult, CreateSession, KernelError, SessionHandle,
    SessionView, StartSource,
};
use rust_cli_kernel_api::session_follow::follow;

use super::{Loop, idle};
use crate::ui::i18n;
use crate::view::Transcript;

/// 发出的命令与回执要用的数据（回执的提示用）。
///
/// 架构整理 U2：以前每种命令另带一个含义不定的 `label: String`（25 处里 20 处传空串，其余各是模型名、要放回输入框的
/// 文字、新名字或参数），回执用一串带守卫的分支按种类与 `label` 判断；现在数据在变体里，回执是一个穷举的 `match`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Sent {
    /// 一条输入：`!` 命令时是它的文字（被拒绝时拿掉 ⏎ 时画的块，Puffin `Msg::ShellStarted` 出错时），其余为空。
    Submit(String),
    /// ⌫ 取回排队的输入：它的文字，出队成功后放回输入框；已经发出时说内核给的原因。
    Dequeue(String),
    Interrupt,
    /// 回答一个请求：它的 ID（被拒绝时那个请求的回答重新可选）。
    Resolve(String),
    /// 换模型：回执后提示这个模型名。
    SetModel(String),
    SetMode,
    SetPlan,
    Compact,
    /// rewind：对话被回退时要放回输入框的文字（只回退代码时为 None）。
    Rewind(Option<String>),
    /// `/rename`：回执后提示新名字。
    Rename(String),
    /// 空闲时 esc / ctrl+c 取消 loop 唤醒（tools-more.md §18.1）：回执不提示。
    CancelLoops,
    /// 不带名字的 `/rename`：回执的文本（`Renamed` 的 `message` 或被拒绝的原因）原样显示（titles.md §8.6）。
    GenerateName,
    /// `/fork`：回执是分出的新会话（`Forked`），到达后切过去。
    Fork,
    /// 挑选回退的点之后的 ⏎（rewind_pick.rs）：回执是 fork 出的会话，跟着它；被拒绝时回到挑选。
    RewindFork,
    /// 成功不提示、失败照常报错的改名：`/clear 名字` 给旧会话改名（tui-display.md §4.6），新会话建好后发出草稿里的改名
    /// （titles.md §4，输入时已经提示过）。
    QuietRename,
    /// esc 撤回（修复线 F38）：回执的 `Restored` 的消息放回输入框；条件不满足被拒绝时不提示。
    RestoreCancelled,
    /// 本地命令的记录（修复线 L-A）：回执不提示。
    Record,
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
        ack: Ack,
        /// 回执的是哪个信封（输入的回执让 `view/in_flight.rs` 的那一条离开或记下收下了）；进程级的回答没有信封。
        command: Option<rust_cli_kernel_api::ids::CommandId>,
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
    /// 入口的缺省模型定下了（performance.md §12.3）：`guess` 是定下之前草稿用的暂定值（§12.4）。
    DefaultModel {
        guess: rust_cli_kernel_api::model::ModelSelection,
        settled: rust_cli_kernel_api::model::ModelSelection,
    },
    /// 入口注入的服务（界面语言等，`services.rs`）。
    Service(super::services::ServiceDone),
    /// 内核给出的 skill 命令（斜杠补全与发送前的判断，tui.md §6）。
    Commands(Vec<rust_cli_kernel_api::user_command::UserCommand>),
    /// `@` 面板的回答（`at_files.rs`，tui-input.md §1.2）。
    At(super::at_files::AtDone),
    /// 剪贴板里的图片（tui-input.md §3.1）。
    Image(Result<Option<crate::services::ImageBytes>, String>),
    /// 文字复制完了：入口回答的途径（tui-select.md §3.2）。
    Copied {
        route: crate::services::CopyRoute,
        text: String,
    },
    /// 外部编辑器结束（tui-input.md §4.1）：编辑后的文字或失败的原因。
    Edited(Result<String, String>),
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
    /// 机器服务的回执（`machines.rs`，remote-exec.md §21.3）。
    Machines(super::machines::MachinesDone),
    /// 设置页的读写（`settings.rs`，tui-kit.md §4.2「设置」）。
    Settings(super::settings::SettingsDone),
    /// `/usage` 的卡与详情页问到的（usage_plans.rs）。
    PlanUsage(crate::ui::usage_plans::PlanAnswer),
}

/// 执行环境路径的分隔符：工作区根是盘符或 `\\` 开头时是 `\`（Windows），否则是 `/`。
pub(super) fn path_separator(root: &str) -> char {
    let drive = matches!(root.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
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

impl Loop<'_> {
    /// 建信封；输入命令（`in_flight::input_of`）同时交给界面，按键的那一帧就画出来（`App::sending`）。
    pub(super) fn input_envelope(&mut self, command: Command) -> CommandEnvelope {
        let envelope = self.envelope(command);
        if let Some((text, delivery)) = crate::view::in_flight::input_of(&envelope.command) {
            self.app.sending(envelope.id.clone(), text, delivery);
        }
        envelope
    }

    /// 发一条命令；没有会话时不发（调用方先判断）。`sent` 带着回执提示要用的数据（例如模型名）。
    pub(super) fn send(&mut self, sent: Sent, command: Command) {
        let Some(session) = self.outbox.session().cloned() else {
            return;
        };
        let envelope = self.input_envelope(command);
        let id = envelope.id.clone();
        self.pending.push(
            async move {
                let ack = session.send(envelope).await;
                Done::Ack {
                    sent,
                    ack,
                    command: Some(id),
                }
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
        let Some(session) = self.outbox.session().cloned() else {
            return;
        };
        self.events = idle();
        self.pending
            .push(async move { Done::View(Box::new(session.view().await)) }.boxed());
    }

    /// 读模型目录（`pick`：`/login` 之后按名字切到登录给出的模型）。没有目录时（入口用显式的 Anthropic 端点）只能提示。
    pub(super) fn catalog(&mut self, pick: Option<String>) {
        let Some(models) = self.options.models.clone() else {
            self.app.open_models(&[]);
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

    pub(super) fn on_done(&mut self, done: Done) {
        self.dirty = true;
        let lang = self.app.lang;
        match done {
            Done::Opened { handle, command } => {
                self.app.creating = false;
                // 新会话从头订阅：SessionCreated 之后的全部事件。
                self.attach(handle, 0);
                self.attach_panels();
                // 还没有会话时完成的本地命令要发的命令（记录与 `/rename 名字` 的改名）排在第一条输入之前（修复线 L-A，
                // tui-manage.md §11.3）：改名先到，actor 先记下 custom，首条输入不当标题（titles.md §4）。
                self.send_held();
                let sent = Sent::Submit(super::input::submit_label(&command));
                self.send(sent, command);
                // 创建期间留下的输入：按顺序发出（发送 future 按加入顺序首次轮询，命令按这个顺序进 actor）。
                for held in std::mem::take(&mut self.app.held) {
                    self.submit(held);
                }
            }
            Done::OpenFailed(error) => {
                self.app.creating = false;
                self.app.held.clear();
                self.app.view.drop_shells();
                self.app.error(i18n::rejected(lang, &error.to_string()));
            }
            Done::Ack { sent, ack, command } => {
                if let Some(command) = &command {
                    let accepted = !matches!(ack, Ack::Rejected { .. });
                    self.app.view.in_flight_mut().acked(command, accepted);
                }
                self.on_ack(sent, ack);
            }
            Done::Rewinds(entries) => match entries {
                Ok(entries) => self.app.open_rewind(entries),
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            Done::Switched(switched) => self.on_switched(*switched),
            Done::DefaultModel { guess, settled } => self.on_default_model(&guess, settled),
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
            Done::Machines(done) => self.on_machines(done),
            Done::Settings(done) => self.on_settings(done),
            Done::PlanUsage(answer) => {
                let now = self.now_ms();
                if let Some(action) = self.app.plan_answered(answer, now)
                    && let Err(error) = self.act(action)
                {
                    self.app.error(error.to_string());
                }
            }
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
            Done::ImageFile { image, text } => self.on_image_file(image, &text),
            Done::At(done) => self.on_at(done),
            Done::View(view) => match *view {
                Ok(view) => {
                    let session = self.outbox.session().cloned();
                    let last = view.last_seq;
                    // 还没回执的输入接着留着（`view/in_flight.rs`）：它们的事实在快照之后到。
                    let carried = self.app.view.in_flight().clone().carried(&view.queue);
                    self.app.view = Transcript::from_view(view, self.now_ms());
                    *self.app.view.in_flight_mut() = carried;
                    self.cache.clear();
                    self.app.sync_interaction(std::time::Instant::now());
                    if let Some(session) = session {
                        self.events = follow(session, last);
                    }
                }
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            Done::Catalog { catalog, pick } => match pick {
                None => self.app.open_models(&catalog.models),
                Some(name) => match crate::ui::resolve_model(&catalog.models, &name) {
                    Ok(selection) => self.choose_model(selection, Some(name), true),
                    Err(rust_cli_kernel_api::model_name::LookupError::Missing) => {
                        self.app.info(i18n::unknown_model(lang, &name))
                    }
                    Err(error) => self.app.info(i18n::ambiguous_model(lang, &error)),
                },
            },
        }
    }

    /// 一条命令的回执（以前在 `on_done` 里）：一个穷举的 `match`（架构整理 U2）。
    fn on_ack(&mut self, sent: Sent, ack: Ack) {
        let lang = self.app.lang;
        match (sent, ack) {
            // 中断与取消唤醒的回执不提示：内核只在没有东西可中断时拒绝（空闲时没有唤醒；内核已经结束回合、
            // `TurnFinished` 还没画出来时按 esc）。缺陷修复（F45）：以前运行中的中断被拒绝时显示 `Not done: no turn
            // is running`（tui-input.md §8）。
            (Sent::CancelLoops | Sent::Interrupt | Sent::Record, _) => {}
            // esc 撤回：条件不满足被拒绝时不提示。
            (
                Sent::RestoreCancelled,
                Ack::Accepted {
                    result: CommandResult::Restored { message, .. },
                },
            ) => self.restored(&message),
            (Sent::RestoreCancelled, Ack::Accepted { .. } | Ack::Rejected { .. } | Ack::Duplicate) => {}
            // ⌫ 取回：出队成功后文字放回输入框；已经发出时说内核给的原因。
            (Sent::Dequeue(text), Ack::Accepted { .. }) => {
                self.app.taking = None;
                self.app.composer.restore_input(&text, Vec::new());
            }
            (Sent::Dequeue(_), Ack::Rejected { reason }) => {
                self.app.taking = None;
                self.app.info(reason);
            }
            (Sent::Dequeue(_), Ack::Duplicate) => self.app.taking = None,
            (Sent::RewindFork, Ack::Rejected { reason }) => self.app.rewound(Err(reason)),
            (
                Sent::RewindFork,
                Ack::Accepted {
                    result: CommandResult::Forked(forked),
                },
            ) => self.rewind_forked(&forked),
            // 选择器在失败时显示 `Failed to restore …`。
            (Sent::Rewind(_), Ack::Rejected { reason }) => {
                self.app.error(format!("Failed to restore:\n{reason}"));
            }
            (Sent::GenerateName, Ack::Rejected { reason }) => self.app.info(reason),
            // 回答被拒绝：Puffin 的 `Could not answer: 原因`，那个请求的回答重新可选（tui-kit.md §4.2「请求」）。
            (Sent::Resolve(ask), Ack::Rejected { reason }) => self.app.answer_failed(&ask, &reason),
            // `!` 命令被拒绝：⏎ 时画的块不会被接替，拿掉（Puffin `Msg::ShellStarted` 出错时）。
            (Sent::Submit(shell), Ack::Rejected { reason }) => {
                if !shell.is_empty() {
                    self.app.view.drop_shell(&shell);
                }
                self.app.error(i18n::rejected(lang, &reason));
            }
            (
                Sent::SetModel(_)
                | Sent::SetMode
                | Sent::SetPlan
                | Sent::Compact
                | Sent::Rename(_)
                | Sent::Fork
                | Sent::QuietRename,
                Ack::Rejected { reason },
            ) => self.app.error(i18n::rejected(lang, &reason)),
            (
                Sent::GenerateName,
                Ack::Accepted {
                    result: CommandResult::Renamed { message, .. },
                },
            ) => self.app.info(message),
            (Sent::SetModel(model), Ack::Accepted { .. }) => {
                self.app.info(i18n::model_set(lang, &model));
            }
            // 对话被回退：把那条消息放回输入框。
            (Sent::Rewind(Some(text)), Ack::Accepted { .. }) => self.app.composer.set_text(&text),
            // 记录随 `RenameRecorded` 由 actor 提交（titles.md §4）。
            (Sent::Rename(title), Ack::Accepted { .. }) => {
                self.app.info(super::records::renamed(&title));
            }
            (
                Sent::Fork,
                Ack::Accepted {
                    result: CommandResult::Forked(forked),
                },
            ) => self.forked(&forked),
            (
                Sent::Submit(_)
                | Sent::Resolve(_)
                | Sent::SetMode
                | Sent::SetPlan
                | Sent::Compact
                | Sent::Rewind(None)
                | Sent::QuietRename
                // 回执不是这个命令的那种结果（内核不会给出）：不提示。
                | Sent::GenerateName
                | Sent::Fork
                | Sent::RewindFork,
                Ack::Accepted { .. },
            )
            | (
                Sent::Submit(_)
                | Sent::Resolve(_)
                | Sent::SetModel(_)
                | Sent::SetMode
                | Sent::SetPlan
                | Sent::Compact
                | Sent::Rewind(_)
                | Sent::Rename(_)
                | Sent::GenerateName
                | Sent::Fork
                | Sent::RewindFork
                | Sent::QuietRename,
                Ack::Duplicate,
            ) => {}
        }
    }
}
