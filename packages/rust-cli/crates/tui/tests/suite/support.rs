// 改写自 apps/zcode-host/crates/tui/tests/suite/support.rs（feat/rs 79b6691c55）：真实 Host 与本地假模型换成
// LocalKernel 与 kernel-api `testing` 的替身；终端换成 ratatui 的 `TestBackend`（docs/specs/rust-cli/tui.md §8）。
//! 集成测试共用：内核替身、可控节奏的模型、虚拟终端与按键。
//!
//! 界面的时钟是真实时间（`WallClock`）：与 F7 之前 TUI 自己读系统时间时相同，相对时间的显示不变；内核用固定时钟。
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures_util::stream::{self, BoxStream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::model::{ModelError, ModelEvent, ModelPort, ModelRequest};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::{CatalogModel, ModelCatalog, ModelDirectory, WireProtocol};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryArtifacts, MemoryStore, SequentialIds, UnusedEnvironment, environments,
    selection,
};
use rust_cli_runtime::{Defaults, LocalKernel, Ports};
use rust_cli_tui::ui::palette::{ColorLevel, Palette};
use rust_cli_tui::{Options, Screen, drive};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

pub type Item = Result<ModelEvent, ModelError>;

/// 按响应给出事件的模型：每次请求取下一个通道，测试决定什么时候发出事件（用来测运行中的输入与中断）。
/// 通道用完后回一个协议错误。
pub struct GatedModel {
    responses: Mutex<VecDeque<mpsc::UnboundedReceiver<Item>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl GatedModel {
    /// `count` 个响应的模型与它们的发送端。
    pub fn new(count: usize) -> (Arc<Self>, Vec<mpsc::UnboundedSender<Item>>) {
        let (senders, receivers): (Vec<_>, VecDeque<_>) =
            (0..count).map(|_| mpsc::unbounded_channel()).unzip();
        let model = Arc::new(Self {
            responses: Mutex::new(receivers),
            requests: Mutex::default(),
        });
        (model, senders)
    }

    /// 每个响应都是写好的事件（不需要控制节奏时）。
    pub fn scripted(responses: Vec<Vec<Item>>) -> Arc<Self> {
        let (model, senders) = Self::new(responses.len());
        for (sender, events) in senders.iter().zip(responses) {
            send(sender, events);
        }
        model
    }

    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

pub fn send(sender: &mpsc::UnboundedSender<Item>, events: Vec<Item>) {
    for event in events {
        sender.send(event).unwrap();
    }
}

/// 发出这次响应剩下的事件并结束它（主循环等模型流结束才继续）。
pub fn finish(sender: mpsc::UnboundedSender<Item>, events: Vec<Item>) {
    send(&sender, events);
}

impl ModelPort for GatedModel {
    fn stream(&self, request: ModelRequest, cancel: CancellationToken) -> BoxStream<'static, Item> {
        self.requests.lock().unwrap().push(request);
        let Some(receiver) = self.responses.lock().unwrap().pop_front() else {
            return stream::iter([Err(ModelError::Protocol {
                message: "no scripted response left".to_owned(),
            })])
            .boxed();
        };
        stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|item| (item, receiver))
        })
        .take_until(cancel.cancelled_owned())
        .boxed()
    }
}

/// 一次请求里所有消息的文字（找 tool_result 与并入的输入）。
pub fn request_text(request: &ModelRequest) -> String {
    serde_json::to_string(&request.messages).unwrap()
}

struct Directory;

#[async_trait::async_trait]
impl ModelDirectory for Directory {
    async fn catalog(&self) -> ModelCatalog {
        let model = |name: &str| CatalogModel {
            capabilities: Default::default(),
            provider: "fixture".into(),
            provider_name: "Fixture".into(),
            model: name.into(),
            protocol: WireProtocol::AnthropicMessages,
            reasoning_levels: Vec::new(),
            context_window: 200_000,
            max_output_tokens: 32_000,
            connection: None,
        };
        ModelCatalog {
            revision: 1,
            models: vec![model("scripted"), model("other-model")],
            default: Some(selection()),
        }
    }
}

/// 测试的终端设置。
pub struct Setup {
    pub width: u16,
    pub height: u16,
    pub lang: &'static str,
    pub palette: Palette,
    /// 入口注入的服务（界面语言的配置值与写回，tui.md §7.1）。
    pub services: rust_cli_tui::Services,
    /// 执行环境的注册表；缺省本机是不能用的替身（没有 skill，skills.rs 换成内存文件系统）。
    pub environments: Arc<dyn EnvironmentRegistry>,
    /// Artifact 存储；缺省是不支持 artifact 的会话存储（`/fork` 的测试换成内存 artifact，记 checkpoint）。
    pub artifacts: Option<Arc<dyn rust_cli_kernel_api::store::ArtifactStore>>,
    /// 会话存储；测试要查提交的事实时自己传入（manage.rs）。
    pub store: Arc<MemoryStore>,
    /// `LANG` 之外的终端变量（例如 `TERM_PROGRAM`，notify.rs）。
    pub vars: Vec<(&'static str, &'static str)>,
    /// 模型目录；缺省是两个没有档位的模型，None 是显式覆盖（没有目录，model_default.rs）。
    pub models: Option<Arc<dyn ModelDirectory>>,
    /// 入口的退出信号（R5-2）。给了时脚本结束后输入也不结束：事件循环只能经这个信号退出（exit_signal.rs）。
    pub stop: Option<CancellationToken>,
    /// 换掉内核用的会话存储（queue.rs 用它让建会话停住）；缺省直接用 `store`。
    pub session_store: Option<Arc<dyn rust_cli_kernel_api::store::SessionStore>>,
    /// 打开后画帧失败（终端关掉了，dead_terminal.rs）。
    pub hang: Arc<std::sync::atomic::AtomicBool>,
    /// 跨会话的端口（peers.rs）；缺省关闭。
    pub peers: rust_cli_runtime::PeerDeps,
    /// 内核的时钟（请求的计时，footer_stats.rs 拨动它）；缺省固定在 0。
    pub clock: Option<Arc<dyn rust_cli_kernel_api::clock::Clock>>,
    /// hooks 快照（early_interrupt.rs 的 UserPromptSubmit）；缺省没有 hook。
    pub hooks: Option<rust_cli_runtime::HooksConfig>,
    /// 启动时打开的会话（`--resume`，update.rs）；缺省新会话。
    pub startup: rust_cli_tui::Startup,
    /// 运行的平台（修复线 F65）：注入 Windows 时按 Windows 的按键规则（tui-input.md §9）；缺省 Unix。
    pub os: rust_cli_tui::Os,
    /// 启动时的权限模式（`--permission-mode`、项目持久模式）；缺省 default（mode_picker.rs 用 acceptEdits）。
    pub mode: PermissionMode,
    /// `ZCODE_FORK_SUBAGENT`（修复线 L-A）：缺省按交互会话打开 fork（Agent 全部在后台）；前台子代理的测试设 Some(false)。
    pub fork_subagent: Option<bool>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            width: 80,
            height: 24,
            lang: "en_US.UTF-8",
            palette: Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
            services: rust_cli_tui::Services::default(),
            environments: environments(Arc::new(UnusedEnvironment)),
            artifacts: None,
            store: Arc::new(MemoryStore::default()),
            vars: Vec::new(),
            models: Some(Arc::new(Directory)),
            stop: None,
            session_store: None,
            hang: Arc::default(),
            peers: rust_cli_runtime::PeerDeps::off(),
            clock: None,
            hooks: None,
            startup: rust_cli_tui::Startup::New,
            os: rust_cli_tui::Os::Unix,
            mode: PermissionMode::Default,
            fork_subagent: None,
        }
    }
}

/// 画面的文字：宽字符占两格，第二格跳过。
pub fn text_of(buffer: &Buffer) -> String {
    let area = buffer.area;
    (area.top()..area.bottom())
        .map(|y| {
            let mut row = String::new();
            let mut x = area.left();
            while x < area.right() {
                let symbol = buffer[(x, y)].symbol();
                row.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            row.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 画面里第一行含 `needle` 的行号（从 0 起）。
pub fn row_of(screen: &str, needle: &str) -> Option<u16> {
    screen
        .lines()
        .position(|line| line.contains(needle))
        .and_then(|row| u16::try_from(row).ok())
}

/// 把空白压成一个空格（比较折过行的文字）。
pub fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 记录每一帧、复制的文字与终端的交出 / 收回。
struct Recorder {
    screen: watch::Sender<String>,
    buffer: Arc<Mutex<Option<Buffer>>>,
    copies: Arc<Mutex<Vec<String>>>,
    notifications: Arc<Mutex<Vec<String>>>,
    handovers: Arc<Mutex<Vec<&'static str>>>,
}

impl Screen for Recorder {
    fn copy(&mut self, text: &str) {
        self.copies.lock().unwrap().push(text.to_owned());
    }

    fn notify(&mut self, sequence: &str) {
        self.notifications.lock().unwrap().push(sequence.to_owned());
    }

    fn drawn(&mut self, buffer: &Buffer) {
        *self.buffer.lock().unwrap() = Some(buffer.clone());
        self.screen.send_replace(text_of(buffer));
        self.handovers.lock().unwrap().push("draw");
    }

    fn suspend(&mut self) {
        self.handovers.lock().unwrap().push("suspend");
    }

    fn resume(&mut self) {
        self.handovers.lock().unwrap().push("resume");
    }
}

/// 测试脚本操作的界面：按键进去，画面出来。
pub struct Ui {
    keys: mpsc::UnboundedSender<Event>,
    screen: watch::Receiver<String>,
    pub buffer: Arc<Mutex<Option<Buffer>>>,
    pub copies: Arc<Mutex<Vec<String>>>,
    /// 写到终端的通知序列（tui-manage.md §5）。
    pub notifications: Arc<Mutex<Vec<String>>>,
    /// 画帧（`draw`）与终端交出 / 收回（`suspend` / `resume`）的顺序（tui-input.md §4.1）。
    pub handovers: Arc<Mutex<Vec<&'static str>>>,
    cursor: Arc<Mutex<super::dead_terminal::Cursor>>,
}

impl Ui {
    /// 终端光标的（列，行）；隐藏时 None（tui.md §7.6）。
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let cursor = self.cursor.lock().unwrap();
        cursor.visible.then_some(cursor.at)
    }

    pub fn press(&self, code: KeyCode, modifiers: KeyModifiers) {
        self.keys
            .send(Event::Key(KeyEvent::new(code, modifiers)))
            .unwrap();
    }

    /// 一个原样的按键事件（带修饰键与种类，例如 Windows 的松开事件）。
    pub fn send(&self, key: KeyEvent) {
        self.keys.send(Event::Key(key)).unwrap();
    }

    /// 括号粘贴（终端把粘贴的文字整段送来）。
    pub fn paste(&self, text: &str) {
        self.keys.send(Event::Paste(text.to_owned())).unwrap();
    }

    pub fn key(&self, code: KeyCode) {
        self.press(code, KeyModifiers::NONE);
    }

    /// 一个鼠标事件（列、行从 0 起，F25）。
    pub fn mouse(&self, kind: MouseEventKind, column: u16, row: u16) {
        self.keys
            .send(Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
    }

    /// 左键从 `from` 拖到 `to` 再松开（F25 的拖选）。
    pub fn drag(&self, from: (u16, u16), to: (u16, u16)) {
        self.mouse(MouseEventKind::Down(MouseButton::Left), from.0, from.1);
        self.mouse(MouseEventKind::Drag(MouseButton::Left), to.0, to.1);
        self.mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1);
    }

    /// 在（列，行）处滚 `times` 格滚轮（`down` 为真时向下）。
    pub fn wheel(&self, down: bool, times: usize, column: u16, row: u16) {
        let kind = if down {
            MouseEventKind::ScrollDown
        } else {
            MouseEventKind::ScrollUp
        };
        for _ in 0..times {
            self.mouse(kind, column, row);
        }
    }

    /// 复制到终端（OSC 52）的文字，按顺序。
    pub fn copied(&self) -> Vec<String> {
        self.copies.lock().unwrap().clone()
    }

    pub fn type_text(&self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    /// 输入一行并按 enter。
    pub fn submit(&self, text: &str) {
        self.type_text(text);
        self.key(KeyCode::Enter);
    }

    pub fn screen(&self) -> String {
        self.screen.borrow().clone()
    }

    /// 等到画面满足条件；等待有上限，只为让失败的测试结束而不挂住。
    pub async fn wait(&mut self, what: &str, done: impl Fn(&str) -> bool) -> String {
        let wait = async {
            loop {
                let screen = self.screen.borrow_and_update().clone();
                if done(&screen) {
                    return screen;
                }
                self.screen.changed().await.unwrap();
            }
        };
        match tokio::time::timeout(Duration::from_secs(10), wait).await {
            Ok(screen) => screen,
            Err(_) => panic!("timed out waiting for {what}; screen:\n{}", self.screen()),
        }
    }

    pub async fn wait_for(&mut self, needle: &str) -> String {
        let needle = needle.to_owned();
        self.wait(&needle.clone(), move |screen| screen.contains(&needle))
            .await
    }

    /// 切到完全访问的确认框（tui.md §7.8，工作线 F87）：等框出现，勾上“我已了解风险”再确认，等框关掉。
    pub async fn confirm_full_access(&mut self) {
        const TITLES: [&str; 2] = ["⚠ Enable full access?", "⚠ 要启用完全访问吗？"];
        self.wait("the full access confirmation", |s| {
            TITLES.iter().any(|title| s.contains(title))
        })
        .await;
        for code in [
            KeyCode::Up,
            KeyCode::Char(' '),
            KeyCode::Down,
            KeyCode::Down,
            KeyCode::Enter,
        ] {
            self.key(code);
        }
        self.wait("the confirmation to close", |s| {
            !TITLES.iter().any(|title| s.contains(title))
        })
        .await;
    }

    /// 审批与提问弹出后 150 ms 内的按键不生效：等过这段时间。
    pub async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn kernel(
    model: Arc<dyn ModelPort>,
    environments: Arc<dyn EnvironmentRegistry>,
    store: Arc<dyn rust_cli_kernel_api::store::SessionStore>,
    artifacts: Option<Arc<dyn rust_cli_kernel_api::store::ArtifactStore>>,
    peers: rust_cli_runtime::PeerDeps,
    clock: Option<Arc<dyn rust_cli_kernel_api::clock::Clock>>,
    fork_subagent: Option<bool>,
) -> Arc<LocalKernel> {
    Arc::new(LocalKernel::with_context_and_extensions(
        Ports {
            model,
            environments,
            store,
            // 文件工具的 checkpoint 要能落 artifact（`FileChanges`，tui-manage.md §4 的回合改动）。
            artifacts: artifacts.unwrap_or_else(|| Arc::new(MemoryArtifacts::default())),
            clock: clock.unwrap_or_else(|| Arc::new(FixedClock(0))),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: rust_cli_runtime::Limits {
                fork_subagent,
                ..Default::default()
            },
        },
        rust_cli_runtime::ContextConfig::new(rust_cli_runtime::Surface::Interactive),
        rust_cli_runtime::Extensions {
            peers,
            ..rust_cli_runtime::Extensions::none()
        },
    ))
}

/// 跑一次界面：脚本结束（按键的发送端释放）时事件循环退出，之后像入口一样跑 SessionEnd。返回退出时的会话 ID。
pub async fn with_ui<F, Fut>(model: Arc<dyn ModelPort>, setup: Setup, script: F) -> Option<String>
where
    F: FnOnce(Ui) -> Fut,
    Fut: Future<Output = ()>,
{
    let exit = with_ui_exit(model, setup, script).await;
    exit.ending.run().await;
    exit.session
}

/// 同 [`with_ui`]，但返回事件循环的结果本身：SessionEnd 还没跑（入口在恢复终端、打印恢复提示之后才跑）。
pub async fn with_ui_exit<F, Fut>(
    model: Arc<dyn ModelPort>,
    setup: Setup,
    script: F,
) -> rust_cli_tui::Exit
where
    F: FnOnce(Ui) -> Fut,
    Fut: Future<Output = ()>,
{
    let (keys, receiver) = mpsc::unbounded_channel();
    let (screen, watcher) = watch::channel(String::new());
    let buffer = Arc::new(Mutex::new(None));
    let copies = Arc::new(Mutex::new(Vec::new()));
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let handovers = Arc::new(Mutex::new(Vec::new()));
    let mut recorder = Recorder {
        screen,
        buffer: buffer.clone(),
        copies: copies.clone(),
        notifications: notifications.clone(),
        handovers: handovers.clone(),
    };
    let cursor = Arc::new(Mutex::new(super::dead_terminal::Cursor::default()));
    let mut terminal = Terminal::new(super::dead_terminal::Hangable {
        inner: TestBackend::new(setup.width, setup.height),
        dead: setup.hang.clone(),
        cursor: cursor.clone(),
    })
    .unwrap();
    let keys_only = stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|event| (event, receiver))
    });
    let input = match &setup.stop {
        Some(_) => keys_only.chain(stream::pending()).boxed(),
        None => keys_only.boxed(),
    };
    let options = Options {
        history: Vec::new(),
        recorder: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/project"),
        },
        home: Some("/Users/dev".into()),
        selection: selection(),
        mode: setup.mode,
        version: "0.1.0".into(),
        vars: std::iter::once(("LANG".to_owned(), setup.lang.to_owned()))
            .chain(
                setup
                    .vars
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
            )
            .collect::<BTreeMap<_, _>>(),
        models: setup.models,
        startup: setup.startup,
        fork: false,
        services: setup.services,
        stop: setup.stop.unwrap_or_default(),
        clock: Arc::new(WallClock),
        ids: Arc::new(SequentialIds::default()),
        os: setup.os,
    };
    let ui = Ui {
        keys,
        screen: watcher,
        buffer,
        copies,
        notifications,
        handovers,
        cursor,
    };
    let kernel = kernel(
        model,
        setup.environments,
        setup.session_store.unwrap_or(setup.store),
        setup.artifacts,
        setup.peers,
        setup.clock,
        setup.fork_subagent,
    );
    if let Some(hooks) = setup.hooks {
        kernel.replace_hooks(hooks);
    }
    let run = drive(
        kernel,
        options,
        setup.palette,
        &mut terminal,
        &mut recorder,
        input,
    );
    let (result, ()) = tokio::join!(run, script(ui));
    result.unwrap()
}

/// 真实时间的时钟（界面的相对时间与输入历史；只在测试里直接读系统时间）。
pub struct WallClock;

impl rust_cli_kernel_api::clock::Clock for WallClock {
    #[expect(clippy::disallowed_methods, reason = "测试的界面时钟用真实时间")]
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}
