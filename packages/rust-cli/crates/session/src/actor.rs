//! 会话 actor 的循环：命令、主循环的提交请求、回合收尾都在这一个任务里按序处理（会话状态的
//! 唯一 owner）。回合内的逻辑见 [`crate::turn`]。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use rust_cli_base::Task;
use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::agents::AgentSignal;
use rust_cli_kernel_api::checkpoint::PendingCheckpoints;
use rust_cli_kernel_api::hooks::HookOutcome;
use rust_cli_kernel_api::ids::{
    CommandId, InteractionId, MessageId, OwnerEpoch, RunId, ToolUseId, TurnId,
};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, OrchestrationError, QueryError, QueryStop,
};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, Delivery, KernelEvent, QueuedInput, SessionView,
};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionMeta, TurnAnchor, WorkspaceCheckpoint};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::SessionDeps;
use crate::agent_signals::AgentStream;
use crate::cron::CronStream;
use crate::log::EventLog;
use crate::notices::{Notices, next_notice};
use crate::sink::SinkRequest;
use crate::title::{TitleState, title_done};
use crate::turn::TurnInput;

pub(crate) enum Request {
    Command {
        envelope: CommandEnvelope,
        reply: oneshot::Sender<Ack>,
    },
    View {
        reply: oneshot::Sender<SessionView>,
    },
    Meta {
        reply: oneshot::Sender<SessionMeta>,
    },
    RewindOptions {
        reply: oneshot::Sender<crate::rewind::Snapshot>,
    },
    Goal {
        reply: oneshot::Sender<rust_cli_kernel_api::goal::GoalView>,
    },
    /// 关闭会话（`SessionHandle::close`）：中止前台工作，等它收尾后回复并退出（spec §3.3）。`reason` 是中止在跑的
    /// 回合用的原因：退出是 `Shutdown`，`/clear`、`/resume` 是 `User`（修复线 F38）。
    Close {
        reply: oneshot::Sender<()>,
        reason: AbortReason,
    },
    /// 会话级的询问（`Interactions::lasting`，agents.md §4.1.1）：不属于哪个回合，回合结束与中断都不取消它。
    Ask {
        request: InteractionRequest,
        reply: oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>,
    },
    /// 撤回会话级的询问（工具被取消；与 `Ask` 走同一个通道，所以总排在它的 `Ask` 之后）。
    CancelAsk {
        tool_use: ToolUseId,
    },
}

/// 待回答的交互属于谁：回合里的询问随回合结束取消；会话级的询问只随会话关闭取消。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AskScope {
    Turn,
    Session,
}

/// 不属于回合的后台工作（SessionStart、代码回退）的结果，回到 actor 后才提交（spec §2）。
pub(crate) enum JobDone {
    Startup(Box<rust_cli_kernel_api::hooks::HookOutcome>),
    Rewind(Box<crate::rewind::Finished>),
}

pub(crate) struct Job {
    pub(crate) task: Task<JobDone>,
    /// SessionStart 可以被中断；回退事务不能中途放弃，不取消。
    pub(crate) cancel: Option<CancellationToken>,
}

/// 等待用户回答的交互：已持久化为待回答事实。
pub(crate) struct Pending {
    pub(crate) id: InteractionId,
    pub(crate) request: InteractionRequest,
    pub(crate) reply: oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>,
    /// 权限询问的 Notification 计时；交互被回答或取消（丢弃）时取消。
    pub(crate) _notify: Option<tokio_util::sync::DropGuard>,
    pub(crate) scope: AskScope,
}

pub(crate) struct Running {
    pub(crate) turn: TurnId,
    /// 这次运行（主循环的回合）；压缩没有（它的子循环自己取）。会话标题的请求用它（工作线 F64）。
    pub(crate) run: Option<RunId>,
    pub(crate) abort: Abort,
    pub(crate) requests: mpsc::Receiver<SinkRequest>,
    pub(crate) task: Task<Result<QueryStop, QueryError>>,
    /// 提交失败的原因；有值时本回合以失败收尾，之后的提交一律拒绝。
    pub(crate) failure: Option<String>,
    /// 开启本回合的 user 消息：checkpoint 的 `messageId`（spec §4.2）。
    pub(crate) prompt: Option<MessageId>,
    /// 开启本回合的命令；提交 `prompt` 时写进批次（Node `anchor.sourceCommandId`）。
    pub(crate) command: Option<CommandId>,
}

enum Next {
    Request(Request),
    Sink(Option<SinkRequest>),
    Notice(rust_cli_kernel_api::machine_notices::MachineNotice),
    Agent(AgentSignal),
    Cron,
    Job(Result<JobDone, rust_cli_base::TaskError>),
    Title(crate::title::TitleDone),
    Name(Result<Option<String>, rust_cli_base::TaskError>),
    /// 跨会话的入站（消息或回执，cross-session.md §2.4）。
    Peer(rust_cli_kernel_api::peers::PeerSignal),
    /// 扣住的消息里最早的到期时刻到了（醒来后按时钟判断）。
    PeerDeadline,
    /// Monitor 的输出事件与截止时刻（工作线 F83，monitors.rs）。
    Monitor(
        rust_cli_kernel_api::ids::ShellTaskId,
        Result<rust_cli_kernel_api::env::CommandEvent, rust_cli_kernel_api::env::EnvError>,
    ),
    MonitorDue,
}

/// 正在运行的回合的提交请求；没有回合时一直等。
async fn sink_request(running: &mut Option<Running>) -> Option<SinkRequest> {
    match running {
        Some(running) => running.requests.recv().await,
        None => std::future::pending().await,
    }
}

/// 后台工作的结果；没有时一直等。
async fn job_done(job: &mut Option<Job>) -> Result<JobDone, rust_cli_base::TaskError> {
    match job {
        Some(job) => (&mut job.task).await,
        None => std::future::pending().await,
    }
}

pub(crate) struct Actor {
    pub(crate) deps: SessionDeps,
    pub(crate) log: Arc<EventLog>,
    pub(crate) meta: SessionMeta,
    pub(crate) messages: Vec<Message>,
    /// 冷读取时只供显示的、最后一个压缩点之前的消息（工作线 F102，compact.md §5.1）：打开后不变，只进 `view()`。
    /// 模型侧的一切（主循环、压缩、token、标题、rewind、fork、goal 与任务的恢复）只看 `messages`。
    pub(crate) earlier: Vec<Message>,
    pub(crate) selection: ModelSelection,
    /// 执行状态：权限模式 × plan 开关（修复线 F20，shell-permissions.md §4.6）。判决用的模式是 [`Self::mode`]。
    pub(crate) execution: rust_cli_kernel_api::permission::ExecutionState,
    /// 计划模式的两个会话标志（已退出计划模式、待发退出提示）：与 `mode` 同一个
    /// owner，改模式时更新，提交 reentry / exit 时清掉；不持久化（context.md §13.4）。
    pub(crate) plan_flags: rust_cli_kernel_api::plan::PlanModeFlags,
    pub(crate) queue: VecDeque<QueuedInput>,
    /// next 通道：插话输入与两种后台通知的到达顺序（优先级 `next`，lane.rs）。
    pub(crate) lane: VecDeque<crate::lane::Lane>,
    /// 已从任务表取走、还没开回合的定时任务（排在通道与 `Queue` 输入之后）。
    pub(crate) cron_fires: VecDeque<rust_cli_kernel_api::cron::CronFire>,
    pub(crate) pending: Vec<Pending>,
    pub(crate) running: Option<Running>,
    pub(crate) job: Option<Job>,
    /// 没有被代码回退撤销的 checkpoint，按写入顺序（spec §4）。
    pub(crate) checkpoints: Vec<WorkspaceCheckpoint>,
    /// 工具已结束、结果还没提交的 checkpoint（`ToolFinished` 的 display）。
    pub(crate) unsettled: PendingCheckpoints,
    /// 工具已结束、结果还没提交的子代理关联（Agent 工具的 display，checkpoints.rs）。
    pub(crate) links: HashMap<ToolUseId, Fact>,
    /// 并入点跑过 UPS 而留下的插话（只在回合结束后取）与记下的结果（steering.rs）。
    pub(crate) screened: HashMap<CommandId, Option<Box<HookOutcome>>>,
    /// 排队输入里的 bash 模式命令与带内容块的输入（bash.rs，tui-input.md §2.2、§3.2）。
    pub(crate) queued_kinds: HashMap<CommandId, crate::bash::QueuedKind>,
    /// 历史被改写的次数（`HistoryRewritten.generation`）。
    pub(crate) generation: u64,
    /// 会话标题的生成（title.rs）与不带参数的 `/rename` 的起名任务（rename.rs）。
    pub(crate) titles: TitleState,
    /// `/loop` 哨兵展开的状态（loop_tick.rs）：不持久化，主线程压缩后清空。
    pub(crate) loop_ticks: crate::loop_tick::LoopTicks,
    pub(crate) naming: Option<crate::rename::NameJob>,
    /// `/goal` 的目标（goal.rs）：提交 `goal_status` 后才改，冷加载从活动历史恢复。
    pub(crate) goal: crate::goal::Goals,
    /// 回合或压缩在跑时到达的本地命令记录（修复线 L-A，local_record.rs）：收尾后按到达顺序提交。
    pub(crate) held_records: Vec<rust_cli_kernel_api::user_command::LocalCommandRecord>,
    seen: HashSet<CommandId>,
    /// 当前轮（开启它的真实用户输入与轮次 ID）：随每个批次交给存储（node-store.md §4，修复线 F6）；冷加载时取
    /// `LoadedSession.turn`。只在 `commit` 里改，锁不跨 await。
    turn: std::sync::Mutex<Option<TurnAnchor>>,
    /// 续写时补在内存里、还没落库的消息，按历史里的顺序：末尾未结调用的中断结果（session-lifecycle.md §6.3，工作线
    /// F81）与 `No response requested.`（§6.3.1，工作线 F77）。第一批带消息的提交把它们排在最前面，成功后清掉；
    /// `StoreResumeSentinel` 把它们单独提交（F79）。只在 `commit` 与 `store_resume_sentinel` 里改，锁不跨 await。
    resume_pending: std::sync::Mutex<Vec<MessageId>>,
    /// 正在关闭：前台工作收尾后回复它并退出循环；期间不开新回合、不接新命令（spec §3.3）。
    pub(crate) closing: Option<oneshot::Sender<()>>,
    /// 收到了 `Shutdown`（进程要退出，修复线 F38，spec §3.5）：之后不再开回合。
    pub(crate) shutting_down: bool,
    stream: Notices,
    agent_stream: AgentStream,
    cron_stream: CronStream,
    /// 跨会话的入站流与状态（peers.rs）。
    peer_stream: crate::watched::Watched<rust_cli_kernel_api::peers::PeerSignal>,
    pub(crate) peers: crate::peers::PeerState,
    /// Monitor 的切行、批、限流与终态的合成（工作线 F83，monitors.rs）：只在 actor 里。
    pub(crate) monitors: crate::monitors::Monitors,
    /// 自己的请求通道（弱引用）：交给回合的输出端，给出会话级的询问入口；不让 actor 因此不退出。
    asks: mpsc::WeakSender<Request>,
}

pub(crate) fn rejected(reason: impl Into<String>) -> Ack {
    Ack::Rejected {
        reason: reason.into(),
    }
}

pub(crate) fn accepted() -> Ack {
    Ack::Accepted {
        result: serde_json::Value::Null,
    }
}

impl Actor {
    /// `resume_pending`：续写时补在内存里的消息（`resume::settle` 的中断结果与 `resume_sentinel::insert` 的占位，按
    /// 历史里的顺序），随第一批带消息的提交落库。
    pub(crate) fn new(
        deps: SessionDeps,
        log: Arc<EventLog>,
        state: crate::SessionState,
        asks: mpsc::WeakSender<Request>,
        resume_pending: Vec<MessageId>,
    ) -> Self {
        // 本机与连上的远端机器的通知（R3，remote-exec.md §20.7.2）。
        let stream = Notices::new(rust_cli_kernel_api::machine_notices::all_shell_notices(
            deps.machines.clone(),
            state.meta.id.clone(),
        ));
        let agent_stream = AgentStream::new(deps.agents.signals(&state.meta.id));
        let cron_stream = CronStream::new(deps.cron.wakeups(&state.meta.id));
        let peer_stream = crate::watched::Watched::new(deps.peers.port.signals(&state.meta.id));
        let start = state.start;
        let goal = crate::goal::Goals::restore(&state.messages, deps.clock.now_ms());
        let mut actor = Self {
            deps,
            log,
            meta: state.meta,
            messages: state.messages,
            earlier: state.earlier,
            selection: state.selection,
            execution: state.mode,
            plan_flags: Default::default(),
            queue: VecDeque::new(),
            lane: VecDeque::new(),
            cron_fires: VecDeque::new(),
            pending: Vec::new(),
            running: None,
            job: None,
            checkpoints: state.checkpoints,
            unsettled: PendingCheckpoints::default(),
            links: HashMap::new(),
            screened: HashMap::new(),
            queued_kinds: HashMap::new(),
            generation: 0,
            titles: TitleState::default(),
            loop_ticks: Default::default(),
            naming: None,
            goal,
            held_records: Vec::new(),
            seen: HashSet::new(),
            turn: std::sync::Mutex::new(state.turn),
            resume_pending: std::sync::Mutex::new(resume_pending),
            closing: None,
            shutting_down: false,
            stream,
            agent_stream,
            cron_stream,
            peer_stream,
            peers: crate::peers::PeerState::default(),
            monitors: crate::monitors::Monitors::default(),
            asks,
        };
        actor.start_session(start);
        actor
    }

    /// 回合（或命令、压缩）的输出端：请求经 `sender` 交给 actor；会话级的询问入口经自己的请求通道。
    pub(crate) fn sink(&self, sender: mpsc::Sender<SinkRequest>) -> crate::sink::ActorSink {
        crate::sink::ActorSink {
            sender,
            asks: self.asks.clone(),
            session: self.meta.id.clone(),
            detached: self.deps.detached.clone(),
        }
    }

    /// 有回合或后台工作在跑：新输入排队，回退 / fork 拒绝。
    pub(crate) fn busy(&self) -> bool {
        self.running.is_some() || self.job.is_some()
    }

    pub(crate) async fn run(mut self, mut requests: mpsc::Receiver<Request>) {
        // 跨会话：绑定为主会话并从库里恢复（cross-session.md §2.2）；在处理第一条命令之前。
        self.peers_open().await;
        loop {
            // 修复（R1-1）：以前这里在“所有句柄都已释放”（`recv` 返回 None）时退出，但内核的会话表一直持有
            // 句柄，这个分支永远到不了——关闭后 actor 还活着，收到后台通知就自己开回合。现在由 `Request::Close`
            // 显式关闭：前台工作收尾后在这里退出。句柄只在会话表释放作用域之后才会全部释放，那时 actor 已被中止，
            // 所以 `recv` 的 None 不需要分支（`Some` 模式不匹配时 select! 只是停用这一支）。
            if !self.busy()
                && let Some(reply) = self.closing.take()
            {
                self.peers_close().await;
                let _ = reply.send(());
                return;
            }
            let now_ms = self.deps.clock.now_ms();
            let monitor_due = self.monitors.next_deadline();
            let next = tokio::select! {
                Some(request) = requests.recv() => Next::Request(request),
                request = sink_request(&mut self.running) => Next::Sink(request),
                notice = next_notice(&mut self.stream) => Next::Notice(notice),
                signal = self.agent_stream.next() => Next::Agent(signal),
                () = self.cron_stream.next() => Next::Cron,
                done = job_done(&mut self.job) => Next::Job(done),
                done = title_done(&mut self.titles) => Next::Title(done),
                done = crate::rename::name_done(&mut self.naming) => Next::Name(done),
                signal = self.peer_stream.next() => Next::Peer(signal),
                () = crate::peers::deadline(&self.peers, now_ms) => Next::PeerDeadline,
                (task, event) = crate::monitors::next_event(&mut self.monitors) => Next::Monitor(task, event),
                () = crate::monitors::deadline(monitor_due, now_ms) => Next::MonitorDue,
            };
            match next {
                Next::Request(request) => self.handle(request).await,
                Next::Sink(Some(request)) => self.on_sink(request).await,
                Next::Notice(notice) => self.on_notice(notice).await,
                Next::Agent(signal) => self.on_agent_signal(signal).await,
                Next::Cron => self.on_cron_wakeup().await,
                Next::Job(done) => self.on_job(done).await,
                Next::Title(done) => self.on_title(done).await,
                Next::Name(result) => self.on_name(result).await,
                Next::Peer(signal) => self.on_peer_signal(signal).await,
                Next::PeerDeadline => self.peers_expire(true).await,
                Next::Monitor(task, event) => self.on_monitor_event(task, event).await,
                Next::MonitorDue => self.on_monitor_due().await,
                // 主循环的输出端已全部释放：它已经返回，收尾本回合。
                Next::Sink(None) => self.finish_turn().await,
            }
            // 登记里的 busy / idle 只看 actor 自己的状态，变化时才写（cross-session.md §2.1）。
            let status = if self.busy() {
                rust_cli_kernel_api::peers::PeerStatus::Busy
            } else {
                rust_cli_kernel_api::peers::PeerStatus::Idle
            };
            self.peer_status(status).await;
        }
    }

    /// 关闭（spec §3.3，`/clear`、`/resume` 的前台任务处理）：中止在跑的回合与可取消的后台工作，取消待回答的
    /// 交互；排队的输入与通知不再开回合（通知没有 ack，留在执行环境与 agents 里，会话再打开时送达）。
    async fn close(&mut self, reply: oneshot::Sender<()>, reason: AbortReason) {
        tracing::info!(event = "session.closing", session = %self.meta.id, busy = self.busy(), "session closing");
        self.closing = Some(reply);
        // `/clear`、`/resume`（用户中止）之后旧会话的后台子代理照样询问（agents.md §4.1.2，工作线 F72）：先登记这个会话
        // 与它的标题，下面取消的会话级询问与之后的询问都转到进程级的入口。退出程序时不登记。
        if reason == AbortReason::User
            && let Some(detached) = &self.deps.detached
        {
            detached.closed(&self.meta.id, &self.meta.title);
        }
        if let Some(running) = &self.running {
            running.abort.abort(reason);
        }
        if let Some(cancel) = self.job.as_ref().and_then(|job| job.cancel.as_ref()) {
            cancel.cancel();
        }
        self.cancel_pending().await;
    }

    async fn handle(&mut self, request: Request) {
        match request {
            Request::View { reply } => {
                let _ = reply.send(self.view().await);
            }
            Request::Meta { reply } => {
                let _ = reply.send(self.meta.clone());
            }
            Request::RewindOptions { reply } => {
                let _ = reply.send(self.rewind_snapshot());
            }
            Request::Goal { reply } => {
                let _ = reply.send(self.goal_view());
            }
            Request::Close { reply, reason } => self.close(reply, reason).await,
            // 关闭中、要退出时不再登记询问：丢下回复，询问方按会话已关闭处理。
            Request::Ask { reply, .. } if self.closing.is_some() || self.shutting_down => {
                drop(reply);
            }
            Request::Ask { request, reply } => self.ask(request, reply, AskScope::Session).await,
            Request::CancelAsk { tool_use } => self.cancel_ask(&tool_use).await,
            // 关闭中不接新命令：会话即将退出。
            Request::Command { reply, .. } if self.closing.is_some() => {
                let _ = reply.send(rejected("the session is closed"));
            }
            Request::Command { envelope, reply } => {
                if !self.seen.insert(envelope.id.clone()) {
                    let _ = reply.send(Ack::Duplicate);
                    return;
                }
                // 代码回退的回执要等事务结束（spec §5.1），起名的回执要等名字（titles.md §8.2）。
                match envelope.command {
                    Command::Rewind { target, scope } => {
                        return self.rewind(envelope.id, target, scope, reply).await;
                    }
                    Command::GenerateName => return self.generate_name(envelope.id, reply),
                    _ => {}
                }
                let ack = self.command(envelope).await;
                let _ = reply.send(ack);
            }
        }
    }

    async fn command(&mut self, envelope: CommandEnvelope) -> Ack {
        let CommandEnvelope { id, command } = envelope;
        match command {
            Command::Submit { text, delivery, .. } => {
                if self.busy() {
                    // 插话（Steer）进 next 通道，由主循环在下一个工具边界领取（steering.rs），其余等回合结束。
                    if delivery == Delivery::Steer {
                        self.lane.push_back(crate::lane::Lane::Input(id.clone()));
                    }
                    self.queue.push_back(QueuedInput {
                        command: id,
                        text,
                        delivery,
                    });
                    self.emit_queue();
                    return accepted();
                }
                match self
                    .start_turn(TurnInput::Human {
                        command: id,
                        text,
                        screened: None,
                        more: Vec::new(),
                    })
                    .await
                {
                    Ok(()) => accepted(),
                    Err(reason) => rejected(reason),
                }
            }
            Command::Interrupt => self.interrupt().await,
            Command::Shutdown => self.shutdown().await,
            Command::RestoreCancelled => self.restore_cancelled(id).await,
            Command::ResolveInteraction {
                interaction,
                answer,
            } => self.resolve(id, interaction, answer).await,
            Command::SetModel { selection } => {
                let fact = Fact::ModelSelected {
                    selection: selection.clone(),
                };
                if let Err(reason) = self.commit(Some(id), vec![fact]).await {
                    return rejected(reason);
                }
                self.selection = selection.clone();
                self.log.append(KernelEvent::ModelChanged { selection });
                // F27：有效窗口随模型变，上下文提示与 footer 的 ctx 按新模型重算（按当前模型算）；修前要等下一条回复。
                self.context_changed();
                accepted()
            }
            Command::SetMode { mode } => match self.user_mode(id, mode).await {
                Ok(()) => accepted(),
                Err(reason) => rejected(reason),
            },
            Command::SetPlan { enabled } => match self.user_plan(id, enabled).await {
                Ok(()) => accepted(),
                Err(reason) => rejected(reason),
            },
            Command::Compact { instructions } => match self.start_compaction(id, instructions) {
                Ok(()) => accepted(),
                Err(reason) => rejected(reason),
            },
            Command::Fork { at } => self.fork(id, at).await,
            Command::ForkFromInput { input } => self.fork_from_input(input).await,
            Command::Rename { title } => self.rename(id, title).await,
            Command::StopTask { task } => self.stop_task(task).await,
            Command::ResolvePeerMessage { message, approve } => {
                match self.resolve_peer(&message, approve).await {
                    Ok(()) => accepted(),
                    Err(reason) => rejected(reason),
                }
            }
            Command::SetWorkspace { root, note } => self.set_workspace(id, root, note).await,
            Command::ChangeDirectory { root } => self.change_directory(id, root, None).await,
            Command::ChangeDirectoryRecorded { root, record } => {
                self.change_directory(id, root, Some(record)).await
            }
            Command::RecordLocalCommand { record } => self.record_local_command(id, record).await,
            Command::BackgroundForeground { reason } => self.background_foreground(reason).await,
            Command::StoreResumeSentinel => self.store_resume_sentinel(id).await,
            // 在 `handle` 里处理（回执要等事务结束或名字）。
            Command::Rewind { .. } | Command::GenerateName => {
                rejected("this command is handled by the actor loop")
            }
            Command::Bash {
                command: line,
                delivery: _,
            } => {
                self.bash_command(id, line);
                accepted()
            }
            Command::SubmitContent {
                content,
                delivery: _,
            } => match self.content_command(id, content).await {
                Ok(()) => accepted(),
                Err(reason) => rejected(reason),
            },
        }
    }

    pub(crate) async fn commit(
        &self,
        command: Option<CommandId>,
        facts: Vec<Fact>,
    ) -> Result<(), String> {
        // 性能修复（修复线 F6，审查 R5-3）：当前轮由 actor 给出，存储不再为 assistant 与每个用量行各从库里推导一次
        // （那条查询要倒着扫本提示的全部消息）。批次里有新的人类输入时这里换轮次，提交成功后才记下。
        let current = self.turn_anchor();
        let turn = TurnAnchor::after(current.as_ref(), &facts, || self.deps.ids.turn());
        let pending = self.pending_resume(&facts);
        let stored_pending = !pending.is_empty();
        let facts = pending.into_iter().chain(facts).collect();
        let batch = FactBatch {
            owner_epoch: OwnerEpoch::LOCAL,
            command,
            facts,
            turn: turn.clone(),
        };
        self.deps
            .store
            .commit(&self.meta.id, batch)
            .await
            .map_err(|error| error.to_string())?;
        *self
            .turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = turn;
        if stored_pending {
            self.resume_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
        Ok(())
    }

    /// 续写时补的消息还没落库、这一批有消息：返回它们，按历史里的顺序排在这一批最前面（工作线 F77、F81，
    /// session-lifecycle.md §6.3、§6.3.1）。已不在活动历史里（被回退掉）的丢掉。
    fn pending_resume(&self, facts: &[Fact]) -> Vec<Fact> {
        if !facts
            .iter()
            .any(|fact| matches!(fact, Fact::MessageAppended { .. }))
        {
            return Vec::new();
        }
        let mut pending = self
            .resume_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let found: Vec<Fact> = pending
            .iter()
            .filter_map(|id| self.messages.iter().find(|message| message.id() == id))
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect();
        if found.is_empty() {
            pending.clear();
        }
        found
    }

    /// 会话里切到这个会话（`/resume`、`/fork`）之后：续写时补在内存里、还没落库的消息（末尾未结调用的中断结果与
    /// `No response requested.`）马上单独提交一批（工作线 F79、F81，session-lifecycle.md §6.3、§6.3.1）。没有要写的时候
    /// 什么都不做；提交失败时放回去，随下一批带消息的提交写。
    async fn store_resume_sentinel(&self, id: CommandId) -> Ack {
        // 先取下再提交：`commit` 看到这一批有消息时不会再把它们排进来一次。
        let pending = std::mem::take(
            &mut *self
                .resume_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        // 已不在活动历史里（被回退掉）的丢掉，同 `pending_resume`。
        let facts: Vec<Fact> = pending
            .iter()
            .filter_map(|wanted| self.messages.iter().find(|message| message.id() == wanted))
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect();
        if facts.is_empty() {
            return accepted();
        }
        match self.commit(Some(id), facts).await {
            Ok(()) => accepted(),
            Err(reason) => {
                *self
                    .resume_pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = pending;
                rejected(reason)
            }
        }
    }

    /// 提交一批锚在给定回合的事实，不改当前轮：起任务时记下回合、结果晚到的旁路请求用（会话标题的用量行，
    /// usage.md §2.1.1，工作线 F73）。
    pub(crate) async fn commit_in_turn(
        &self,
        turn: Option<TurnAnchor>,
        facts: Vec<Fact>,
    ) -> Result<(), String> {
        let batch = FactBatch {
            owner_epoch: OwnerEpoch::LOCAL,
            command: None,
            facts,
            turn,
        };
        self.deps
            .store
            .commit(&self.meta.id, batch)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn turn_anchor(&self) -> Option<TurnAnchor> {
        self.turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn on_job(&mut self, done: Result<JobDone, rust_cli_base::TaskError>) {
        self.job = None;
        match done {
            Ok(JobDone::Startup(outcome)) => self.finish_startup(*outcome).await,
            Ok(JobDone::Rewind(finished)) => self.finish_rewind(*finished).await,
            Err(error) => {
                tracing::warn!(%error, "a session job ended without a result");
                self.start_next().await;
            }
        }
    }

    pub(crate) fn emit_queue(&self) {
        self.log.append(KernelEvent::QueueChanged {
            items: self.queue.iter().cloned().collect(),
        });
    }

    async fn view(&self) -> SessionView {
        SessionView {
            meta: self.meta.clone(),
            messages: [self.earlier.as_slice(), self.messages.as_slice()].concat(),
            selection: self.selection.clone(),
            mode: self.execution.mode,
            plan: self.execution.plan,
            pending: self
                .pending
                .iter()
                .map(|pending| (pending.id.clone(), pending.request.clone()))
                .collect(),
            queue: self.queue.iter().cloned().collect(),
            last_seq: self.log.last_seq(),
            revision: self.log.revision(),
            running: self.running.is_some(),
            background: [self.background().await, self.background_agents().await].concat(),
            peer_held: self.peers.views(),
            // 冷加载后不用等下一条回复（F27）：与 `context_changed` 同样两个函数。
            context_usage: self.context_occupancy(),
            context_notice: self.context_notice().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
#[path = "turn_anchor_tests.rs"]
mod turn_anchor_tests;
