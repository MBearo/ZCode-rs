//! 会话 crate 的测试组装：可替换 hooks、执行环境、artifact 与初始状态（L10 的测试共用）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::hooks::{HookEvent, HookEventName, HookOutcome, HookRunner};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, Generation, SessionId, TraceId};
use rust_cli_kernel_api::local_services::LocalServices;
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{ModelPort, SystemBlock};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, Delivery, KernelEvent, SequencedEvent, StartSource,
};
use rust_cli_kernel_api::store::{SessionKind, SessionMeta, SessionStore, WorkspaceCheckpoint};
use rust_cli_kernel_api::testing::{
    AllPorts, DirectExecutor, FixedClock, FixedContext, ListTools, MemoryArtifacts,
    MemoryEnvironment, MemoryStore, NeverCompact, SequentialIds, local_services, machine,
    selection,
};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_query::{QueryLoop, QueryParts};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{Opened, Session, SessionDeps, SessionState};

pub(crate) fn meta() -> SessionMeta {
    SessionMeta {
        id: SessionId::new("sess_1"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        title: String::new(),
        trace: TraceId::new("trace_1"),
        created_at_ms: 0,
    }
}

pub(crate) fn env() -> Arc<MemoryEnvironment> {
    Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/work"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: None,
    }))
}

/// 按事件名给出预设结果的 hooks；`gate` 有值时 UserPromptSubmit 等它放行（或被取消），开始等时通知 `entered`
/// （修复线 F37：测试确定地在 hook 正在跑时中断）。
#[derive(Default)]
pub(crate) struct ScriptedHooks {
    pub outcomes: Mutex<HashMap<HookEventName, HookOutcome>>,
    pub seen: Mutex<Vec<HookEvent>>,
    pub gate: Option<Arc<Notify>>,
    pub entered: Option<Arc<Notify>>,
}

impl ScriptedHooks {
    pub(crate) fn with(event: HookEventName, outcome: HookOutcome) -> Self {
        let hooks = Self::default();
        hooks.set(event, outcome);
        hooks
    }

    pub(crate) fn set(&self, event: HookEventName, outcome: HookOutcome) {
        self.outcomes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(event, outcome);
    }

    pub(crate) fn seen(&self) -> Vec<HookEvent> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl HookRunner for ScriptedHooks {
    async fn run(&self, event: HookEvent, cancel: CancellationToken) -> HookOutcome {
        let name = event.name;
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
        if let (Some(gate), HookEventName::UserPromptSubmit) = (&self.gate, name) {
            if let Some(entered) = &self.entered {
                entered.notify_one();
            }
            tokio::select! {
                () = gate.notified() => {}
                () = cancel.cancelled() => return HookOutcome::default(),
            }
        }
        self.outcomes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&name)
            .cloned()
            .unwrap_or_default()
    }

    fn has_hooks(&self, event: HookEventName) -> bool {
        self.outcomes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&event)
    }
}

pub(crate) struct Built {
    pub _scope: TaskScope,
    pub store: Arc<MemoryStore>,
    pub session: Session,
}

pub(crate) struct Setup {
    pub model: Arc<dyn ModelPort>,
    pub hooks: Arc<dyn HookRunner>,
    pub machine: MachineEnv,
    pub local: LocalServices,
    pub artifacts: Arc<MemoryArtifacts>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub messages: Vec<Message>,
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    pub opened: Opened,
    pub start: StartSource,
    /// 为 false 时存储里没有这个会话，所有提交都失败。
    pub stored: bool,
    pub cron: Arc<dyn rust_cli_kernel_api::cron::CronSignals>,
    /// 会话标题的生成（title_tests.rs）。
    pub titles: Option<crate::TitleModel>,
    /// actor 自己的旁路请求（rename_tests.rs）；缺省与主循环同一个模型。
    pub side_model: Option<Arc<dyn ModelPort>>,
    /// 缺省是只有一段 system 的 `FixedContext`。
    pub context: Option<Arc<dyn rust_cli_kernel_api::orchestration::ContextBuilder>>,
    /// 斜杠命令的展开（slash_tests.rs）；缺省没有命令。
    pub commands: Arc<dyn rust_cli_kernel_api::user_command::UserCommands>,
    /// `@` 提及与 bash 模式（tui-input.md §1.1、§2.2）；缺省没有。
    pub inputs: Arc<dyn rust_cli_kernel_api::user_input::UserInputs>,
    /// 子代理的信号（tasks_tests.rs）；缺省没有子代理。
    pub agents: Arc<dyn rust_cli_kernel_api::agents::AgentSignals>,
    /// `/cd` 说明里新目录的规则文件（workspace_tests.rs）；缺省没有。
    pub directory_rules: Arc<dyn rust_cli_kernel_api::orchestration::DirectoryRules>,
    /// 冷加载时的当前轮（turn_anchor_tests.rs）；缺省没有。
    pub turn: Option<rust_cli_kernel_api::store::TurnAnchor>,
    /// 冷加载时当前轮以错误结束（resume_sentinel_tests.rs）；缺省否。
    pub last_turn_failed: bool,
    /// 跨会话（peers_tests.rs）；缺省关闭。
    pub peers: crate::PeerDeps,
    /// 父会话关闭之后的询问入口（detached_tests.rs）；缺省没有。
    pub detached: Option<Arc<crate::DetachedAsks>>,
    /// 共用的存储（重开会话的测试）；缺省每次新建。
    pub store: Option<Arc<MemoryStore>>,
    /// 执行环境的注册表（R3）；缺省只有本机（[`Setup::machine`] 与 [`Setup::local`]）。
    pub machines: Option<Arc<dyn rust_cli_kernel_api::env_registry::EnvironmentRegistry>>,
    /// 会话的工作目录；缺省是 [`meta`] 的 `/work`（workspace_tests.rs 的 Windows 用例改它，F57）。
    pub root: &'static str,
}

impl Setup {
    pub(crate) fn new(model: Arc<dyn ModelPort>) -> Self {
        let env = env();
        Self {
            model,
            hooks: Arc::new(rust_cli_kernel_api::testing::NoHooks),
            machine: machine(env.clone()),
            local: local_services(env),
            artifacts: Arc::new(MemoryArtifacts::default()),
            tools: Vec::new(),
            messages: Vec::new(),
            checkpoints: Vec::new(),
            opened: Opened::Created,
            start: StartSource::Startup,
            stored: true,
            cron: Arc::new(rust_cli_kernel_api::cron::NoCron),
            titles: None,
            side_model: None,
            context: None,
            commands: Arc::new(rust_cli_kernel_api::user_command::NoUserCommands),
            inputs: Arc::new(rust_cli_kernel_api::user_input::NoUserInputs),
            agents: Arc::new(rust_cli_kernel_api::agents::NoAgentSignals),
            directory_rules: Arc::new(rust_cli_kernel_api::orchestration::NoDirectoryRules),
            turn: None,
            last_turn_failed: false,
            peers: crate::PeerDeps::off(),
            detached: None,
            store: None,
            machines: None,
            root: "/work",
        }
    }

    /// 会话的机器与本机服务都换成 `env`（一个替身实现全部端口）。
    pub(crate) fn set_env<E: AllPorts + 'static>(&mut self, env: Arc<E>) {
        self.machine = machine(env.clone());
        self.local = local_services(env);
    }

    pub(crate) async fn build(self) -> Built {
        let scope = TaskScope::root("test", |_| {});
        let reused = self.store.is_some();
        let store = self.store.unwrap_or_default();
        let mut meta = meta();
        meta.workspace.root = EnvPath::new(self.root);
        if self.stored && !reused {
            store.create(meta.clone()).await.unwrap();
        }
        let ids = Arc::new(SequentialIds::default());
        let side_model = self.side_model.unwrap_or_else(|| self.model.clone());
        let machines = self.machines.unwrap_or_else(|| {
            Arc::new(rust_cli_kernel_api::env_registry::LocalRegistry::new(
                self.machine.clone(),
                self.local.clone(),
            ))
        });
        let runner = Arc::new(QueryLoop::new(QueryParts {
            model: self.model,
            context: self.context.unwrap_or_else(|| {
                Arc::new(FixedContext(vec![SystemBlock {
                    text: "system".to_owned(),
                    cache_control: None,
                }]))
            }),
            tools: Arc::new(ListTools(self.tools)),
            executor: Arc::new(DirectExecutor),
            compactor: Arc::new(NeverCompact),
            hooks: self.hooks.clone(),
            ids: ids.clone(),
            clock: Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
            machine: self.machine.clone(),
            machines: machines.clone(),
            local: self.local.clone(),
            artifacts: self.artifacts.clone(),
            generation: Generation::new("g1"),
            non_interactive: false,
            limits: rust_cli_query::RequestLimits::default(),
        }));
        let session = Session::spawn(
            SessionDeps {
                store: store.clone(),
                ids,
                runner,
                compactor: Arc::new(NeverCompact),
                machine: self.machine,
                machines,
                local: self.local,
                max_turns: None,
                agents: self.agents,
                cron: self.cron,
                hooks: self.hooks,
                commands: self.commands,
                inputs: self.inputs,
                artifacts: self.artifacts.clone(),
                clock: Arc::new(FixedClock(1_000)),
                generation: Generation::new("g1"),
                spawner: scope.spawner(),
                closing: scope.spawner(),
                titles: self.titles,
                side_model,
                released: Arc::new(crate::NothingToRelease),
                directory_rules: self.directory_rules,
                peers: self.peers,
                detached: self.detached,
            },
            SessionState {
                meta,
                messages: self.messages,
                selection: selection(),
                mode: PermissionMode::Default.into(),
                checkpoints: self.checkpoints,
                start: self.start,
                turn: self.turn,
                last_turn_failed: self.last_turn_failed,
            },
            self.opened,
        );
        Built {
            _scope: scope,
            store,
            session,
        }
    }
}

pub(crate) fn envelope(id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command,
    }
}

pub(crate) fn submit(id: &str, text: &str) -> CommandEnvelope {
    envelope(
        id,
        Command::Submit {
            text: text.to_owned(),
            attachments: Vec::new(),
            delivery: Delivery::Queue,
        },
    )
}

/// 读事件直到满足 `done`（5 s 内）。
pub(crate) async fn until(
    events: &mut BoxStream<'static, SequencedEvent>,
    done: impl Fn(&KernelEvent) -> bool,
) -> Vec<KernelEvent> {
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out; saw {seen:?}"))
            .unwrap();
        let stop = done(&event.event);
        seen.push(event.event);
        if stop {
            return seen;
        }
    }
}

pub(crate) fn finished(event: &KernelEvent) -> bool {
    matches!(event, KernelEvent::TurnFinished { .. })
}
