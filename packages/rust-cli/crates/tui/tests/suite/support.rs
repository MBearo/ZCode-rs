// 改写自 apps/zcode-host/crates/tui/tests/suite/support.rs（feat/rs 79b6691c55）：真实 Host 与本地假模型换成
// LocalKernel 与 kernel-api `testing` 的替身；终端是记录每帧的 [`Screen`]，帧按 kit 的规则铺成格子网读回（[`Shot`]，
// docs/specs/rust-cli/tui.md §8、tui-kit.md §3.7）。
//! 集成测试共用：内核替身、可控节奏的模型、虚拟终端与按键。
//!
//! 界面的时钟缺省是真实时间（`WallClock`）：与 F7 之前 TUI 自己读系统时间时相同，相对时间的显示不变；内核用固定时钟。
//! 断言依赖界面时刻之差的测试（思考的用时）经 `Setup.ui_clock` 换成固定时钟。
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use futures_util::stream::{self, BoxStream, StreamExt};
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
use rust_cli_tui::{ColorLevel, Colors, Frame, Options, Screen, drive};
use rust_cli_tui_kit::render::{StylePool, Surface};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

pub type Item = Result<ModelEvent, ModelError>;

/// 按响应给出事件的模型：每次请求取下一个通道，测试决定什么时候发出事件（用来测运行中的输入与中断）。
/// 通道用完后回一个协议错误。
pub struct GatedModel {
    responses: Mutex<VecDeque<mpsc::UnboundedReceiver<Item>>>,
    requests: Mutex<Vec<ModelRequest>>,
    /// 每个响应（按请求的次序）被取了几次下一个事件：取第 n + 1 次时前 n 个已经处理完。
    polls: Arc<Mutex<Vec<usize>>>,
}

impl GatedModel {
    /// `count` 个响应的模型与它们的发送端。
    pub fn new(count: usize) -> (Arc<Self>, Vec<mpsc::UnboundedSender<Item>>) {
        let (senders, receivers): (Vec<_>, VecDeque<_>) =
            (0..count).map(|_| mpsc::unbounded_channel()).unzip();
        let model = Arc::new(Self {
            responses: Mutex::new(receivers),
            requests: Mutex::default(),
            polls: Arc::default(),
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

    /// 等第 `response` 个响应（从 0 数）的前 `count` 个事件都已处理完（内核来取下一个）。画面上看不出事件到了时用它对齐。
    pub async fn consumed(&self, response: usize, count: usize) {
        let wait = async {
            while self
                .polls
                .lock()
                .unwrap()
                .get(response)
                .is_none_or(|n| *n <= count)
            {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), wait)
            .await
            .unwrap_or_else(|_| panic!("response {response} never consumed {count} events"));
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
        let polls = self.polls.clone();
        let at = {
            let mut polls = polls.lock().unwrap();
            polls.push(0);
            polls.len() - 1
        };
        stream::unfold(receiver, move |mut receiver| {
            polls.lock().unwrap()[at] += 1;
            async move { receiver.recv().await.map(|item| (item, receiver)) }
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

/// 启动前选择屏的按键事件（信任对话框与自动更新的选择屏直接收终端事件）。
pub fn key_event(code: KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// 同上，ctrl 加一个字符。
pub fn ctrl_event(c: char) -> crossterm::event::Event {
    crossterm::event::Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

/// 同上，松开按键的事件（两个选择屏都不处理）。
pub fn release_event(code: KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    })
}

/// 测试拨动的内核时钟：请求的开始、首个内容与结束各读一次（usage.md §1；架构整理 U15：以前两个测试文件各写一份；F126：
/// 用 kernel-api testing 的共用时钟）。
pub use rust_cli_kernel_api::testing::ManualClock;

/// 测试的终端设置。
pub struct Setup {
    pub width: u16,
    pub height: u16,
    pub lang: &'static str,
    pub colors: Colors,
    /// 入口给的配置值（界面语言、主题等，tui.md §7.1）。
    pub ui: rust_cli_tui::UiConfig,
    /// 入口注入的服务；缺省全是替身（`stub_services.rs`）。
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
    pub session_store: Option<Arc<dyn rust_cli_kernel_api::store::Store>>,
    /// 打开后画帧失败（终端关掉了，dead_terminal.rs）。
    pub hang: Arc<std::sync::atomic::AtomicBool>,
    /// 跨会话的端口（peers.rs）；缺省关闭。
    pub peers: rust_cli_runtime::PeerDeps,
    /// 内核的时钟（请求的计时，footer_stats.rs 拨动它）；缺省固定在 0。
    pub clock: Option<Arc<dyn rust_cli_kernel_api::clock::Clock>>,
    /// 界面的时钟；缺省真实时间（`WallClock`）。思考的用时是界面时钟上思考行建立到消息收拢之差，断言收起的思考没有
    /// 用时（一行只有 `Thought`）的测试给固定时钟（flow.rs、find.rs）。
    pub ui_clock: Option<Arc<dyn rust_cli_kernel_api::clock::Clock>>,
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
    /// 终端分辨得出增强按键（修复线 F104，tui-input.md §11.1.1）：真实终端取键盘增强的查询结论；缺省否。
    pub keyboard_enhanced: bool,
    /// 终端支持程序状态（OSC 7501，tui-manage.md §5.4）：真实终端取进界面时的探测；缺省否（program_status.rs 设为真）。
    pub program_status: bool,
    /// 新会话的缺省模型；缺省 `selection()`（fixture 的 scripted）。没有模型的门道（login.rs）给空的模型；还没定下的
    /// 缺省模型（startup.rs，performance.md §12.3）给 `DefaultModel::pending`。
    pub selection: Option<rust_cli_kernel_api::default_model::DefaultModel>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            width: 80,
            height: 24,
            lang: "en_US.UTF-8",
            colors: Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
            ui: rust_cli_tui::UiConfig::default(),
            services: super::stub_services::services(),
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
            ui_clock: None,
            hooks: None,
            startup: rust_cli_tui::Startup::New,
            os: rust_cli_tui::Os::Unix,
            mode: PermissionMode::Default,
            fork_subagent: None,
            keyboard_enhanced: false,
            program_status: false,
            selection: None,
        }
    }
}

pub use super::shot::{Shot, ShotCell, shot_of, text_of};

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
    /// 终端分辨得出增强按键（修复线 F104）。
    enhanced: bool,
    /// 终端支持程序状态（OSC 7501）。
    program_status: bool,
    /// 终端的（列，行）。
    size: (u16, u16),
    /// 打开后写帧失败（终端关掉了，dead_terminal.rs）。
    dead: Arc<std::sync::atomic::AtomicBool>,
    /// 把帧的行铺成格子网用的样式池（真彩，与渲染器的规则相同）。
    pool: StylePool,
    shot: Arc<Mutex<Option<Shot>>>,
    copies: Arc<Mutex<Vec<String>>>,
    notifications: Arc<Mutex<Vec<String>>>,
    handovers: Arc<Mutex<Vec<&'static str>>>,
}

impl Screen for Recorder {
    fn keyboard_enhanced(&self) -> bool {
        self.enhanced
    }

    fn program_status(&self) -> bool {
        self.program_status
    }

    fn copy(&mut self, text: &str) {
        self.copies.lock().unwrap().push(text.to_owned());
    }

    fn notify(&mut self, sequence: &str) {
        self.notifications.lock().unwrap().push(sequence.to_owned());
    }

    fn size(&self) -> std::io::Result<(u16, u16)> {
        Ok(self.size)
    }

    fn write(&mut self, _bytes: &str) -> std::io::Result<()> {
        if self.dead.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::from_raw_os_error(5));
        }
        Ok(())
    }

    fn drawn(&mut self, frame: &Frame, width: u16) {
        let surface = Surface::build(usize::from(width), &frame.lines, &mut self.pool);
        // 先存格子，再发文字：等文字的测试醒来时格子已经是这一帧的。
        *self.shot.lock().unwrap() = Some(Shot::of(&surface, &self.pool));
        self.screen.send_replace(text_of(&surface));
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
    /// 最近一帧（tui-kit.md §3.7）。
    pub shot: Arc<Mutex<Option<Shot>>>,
    pub copies: Arc<Mutex<Vec<String>>>,
    /// 写到终端的通知序列（tui-manage.md §5）。
    pub notifications: Arc<Mutex<Vec<String>>>,
    /// 画帧（`draw`）与终端交出 / 收回（`suspend` / `resume`）的顺序（tui-input.md §4.1）。
    pub handovers: Arc<Mutex<Vec<&'static str>>>,
}

impl Ui {
    /// 光标格的（列，行）：最近一帧里带 `attr::CARET` 的格子，终端光标停在那里；没有时 None（tui.md §7.6、tui-kit.md §3.2）。
    pub fn cursor(&self) -> Option<(u16, u16)> {
        self.shot
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|shot| shot.caret)
    }

    /// 最近一帧（还没画过时 panic）。
    pub fn frame(&self) -> Shot {
        self.shot.lock().unwrap().clone().unwrap()
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

    /// 终端报告焦点（`FocusGained` / `FocusLost`，终端通知看它，ui/notify.rs）。
    pub fn focus(&self, focused: bool) {
        let event = if focused {
            Event::FocusGained
        } else {
            Event::FocusLost
        };
        self.keys.send(event).unwrap();
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
    /// 清空草稿（ctrl+u 删到行首）：跟着草稿的一层关上之后草稿还留着它的命令（tui-kit.md §4.0「板」）。
    pub fn clear(&self) {
        self.press(KeyCode::Char('u'), KeyModifiers::CONTROL);
    }

    pub fn submit(&self, text: &str) {
        self.type_text(text);
        self.key(KeyCode::Enter);
    }

    /// 在模型选择器里选中过滤出的第一个模型：打出 `/model `，等选择器画出来（槽里是 `←→`），打过滤词，⏎。要先等：目录到之前
    /// 的 ⏎ 只是发出草稿（tui-kit.md §4.0「板」）。
    pub async fn pick_model(&mut self, word: &str) {
        self.type_text("/model ");
        self.wait_for("←→").await;
        self.type_text(word);
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

    /// 请求打开后 12 个 tick（396 ms）内回答没有点亮，⏎ 与数字不起作用（tui.md §5.2，Puffin `ARM_TICKS`）：等过这段时间。
    pub async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(450)).await;
    }
}

/// 测试用的 LocalKernel（伪终端里的端到端也用，panic_terminal.rs）。
pub fn kernel(
    model: Arc<dyn ModelPort>,
    environments: Arc<dyn EnvironmentRegistry>,
    store: Arc<dyn rust_cli_kernel_api::store::Store>,
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
            selection: selection().into(),
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
    let shot = Arc::new(Mutex::new(None));
    let copies = Arc::new(Mutex::new(Vec::new()));
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let handovers = Arc::new(Mutex::new(Vec::new()));
    let mut recorder = Recorder {
        screen,
        enhanced: setup.keyboard_enhanced,
        program_status: setup.program_status,
        size: (setup.width, setup.height),
        dead: setup.hang.clone(),
        pool: StylePool::new(),
        shot: shot.clone(),
        copies: copies.clone(),
        notifications: notifications.clone(),
        handovers: handovers.clone(),
    };
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
        selection: setup
            .selection
            .clone()
            .unwrap_or_else(|| selection().into()),
        mode: setup.mode,
        started: std::time::Instant::now(),
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
        ui: setup.ui,
        services: setup.services,
        stop: setup.stop.unwrap_or_default(),
        clock: setup.ui_clock.unwrap_or_else(|| Arc::new(WallClock)),
        ids: Arc::new(SequentialIds::default()),
        os: setup.os,
    };
    let ui = Ui {
        keys,
        screen: watcher,
        shot,
        copies,
        notifications,
        handovers,
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
    let run = drive(kernel, options, setup.colors, &mut recorder, input);
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
