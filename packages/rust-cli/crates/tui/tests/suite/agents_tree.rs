//! 子代理列表的树（docs/specs/rust-cli/tui-panels.md §1.2–§1.4，修复线 F30）：内核是 LocalKernel，子代理由 agents 的
//! Agent 工具真实地嵌套派生（main → A → A1 → A1a，main 另有 B），父子关系来自快照的 `owner`、停放来自 `parked`。
//! 模型按请求来源与子代理的 prompt 回复，每个子代理挂在自己的闸门上，测试决定谁先结束。
//! 修复线 F34：子代理的行与在跑的命令出自同一份快照（tui-panels.md §0），同一帧画出。
//! 列表是任务卡的 Subagents（tui-tasks.md §3）：^T 打开（↓ 是兼容的旧键）；输入框上方是任务行（§2）。
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossterm::event::KeyCode;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, OperationContext, StartCommand, WorkspaceRef,
};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStatus, BackgroundTask, ShellState,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId, ShellTaskId};
use rust_cli_kernel_api::machine::{Background, Commands, Shell};
use rust_cli_kernel_api::model::{ModelPort, ModelRequest, QuerySource};
use rust_cli_kernel_api::testing::{
    MemoryStore, UnusedEnvironment, registry, text_response, tool_use_response, unsupported_machine,
};
use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::sessions_seed::{Seed, seed};
use super::support::{Item, Setup, with_ui};

/// 按来源回复的模型：主线程按序取剧本；子代理按第一条消息里的标记与已有的工具结果数回复；旁路请求回固定文本。
struct Routed {
    main: Mutex<VecDeque<Vec<Item>>>,
    gates: HashMap<&'static str, (watch::Sender<bool>, watch::Receiver<bool>)>,
}

/// 派生一个后台 general-purpose 子代理。
fn spawn(id: &str, description: &str, key: &str) -> Vec<Item> {
    tool_use_response(
        id,
        "Agent",
        &json!({"description": description, "prompt": format!("{key} do the work."),
                "subagent_type": "general-purpose", "run_in_background": true}),
    )
}

impl Routed {
    fn new() -> Arc<Self> {
        let gates = ["A", "A1", "A1a", "B"]
            .into_iter()
            .map(|name| (name, watch::channel(false)))
            .collect();
        Arc::new(Self {
            main: Mutex::new(VecDeque::from([
                spawn("toolu_main_a", "Alpha task", "KEY-AAA"),
                spawn("toolu_main_b", "Beta task", "KEY-BBB"),
                text_response(&["Launched."]),
            ])),
            gates,
        })
    }

    fn release(&self, name: &str) {
        self.gates[name].0.send_replace(true);
    }

    /// 子代理 `key` 的这一步：先等闸门 `gate`（有的话）再回复。
    fn reply(&self, gate: Option<&str>, events: Vec<Item>) -> BoxStream<'static, Item> {
        let Some(gate) = gate else {
            return stream::iter(events).boxed();
        };
        let mut open = self.gates[gate].1.clone();
        stream::once(async move {
            let _ = open.wait_for(|open| *open).await;
        })
        .flat_map(move |()| stream::iter(events.clone()))
        .boxed()
    }
}

impl ModelPort for Routed {
    fn stream(&self, request: ModelRequest, cancel: CancellationToken) -> BoxStream<'static, Item> {
        let first = serde_json::to_string(&request.messages.first()).unwrap();
        let results = serde_json::to_string(&request.messages)
            .unwrap()
            .matches("\"tool_use_id\"")
            .count();
        let reply = match request.source {
            QuerySource::MainThread => {
                let next = self.main.lock().unwrap().pop_front();
                self.reply(None, next.unwrap_or_else(|| text_response(&["Noted."])))
            }
            QuerySource::Subagent(_) if first.contains("KEY-AAA") => {
                if results == 0 {
                    self.reply(None, spawn("toolu_a_1", "Alpha one", "KEY-CCC"))
                } else {
                    self.reply(Some("A"), text_response(&["A done."]))
                }
            }
            QuerySource::Subagent(_) if first.contains("KEY-CCC") => {
                if results == 0 {
                    self.reply(None, spawn("toolu_a1_1", "Alpha one deep", "KEY-DDD"))
                } else {
                    self.reply(Some("A1"), text_response(&["A1 done."]))
                }
            }
            QuerySource::Subagent(_) if first.contains("KEY-DDD") => {
                self.reply(Some("A1a"), text_response(&["A1a done."]))
            }
            QuerySource::Subagent(_) if first.contains("KEY-BBB") => {
                self.reply(Some("B"), text_response(&["B done."]))
            }
            _ => self.reply(None, text_response(&["Fixture side reply"])),
        };
        reply.take_until(cancel.cancelled_owned()).boxed()
    }
}

fn wide() -> Setup {
    Setup {
        width: 120,
        height: 36,
        ..Setup::default()
    }
}

/// 列表里以 `prefix` 开头的最后一行被选中（名字是 brand；选中只改颜色，没有指针，tui-kit.md §4.3）。等待有上限，只为让
/// 失败的测试结束。
async fn until_selected(ui: &super::support::Ui, prefix: &str) {
    let brand = wide().colors.ink().theme.brand;
    let wait = async {
        loop {
            let shot = ui.frame();
            let hit = (0..shot.area.height).rev().find_map(|y| {
                let row = shot.row(y);
                row.starts_with(prefix)
                    .then(|| (row.find("general-purpose").unwrap(), y))
            });
            // 行首是 ASCII 与单宽的记号：字节位置减去多字节记号的多余字节是列。
            if let Some((at, y)) = hit {
                let column = row_column(&shot.row(y), at);
                if shot[(column, y)].fg == brand {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!(
            "timed out waiting for {prefix} to be selected; screen:\n{}",
            ui.screen()
        );
    }
}

/// 一行里字节位置 `at` 所在的列（行里的字都是单宽的）。
fn row_column(row: &str, at: usize) -> u16 {
    row[..at].chars().count() as u16
}

/// 子代理的层的行（主会话、`↑ / ↓ N more` 与第 3 列是记号的行；右侧的用时去掉）。transcript 的工具行的记号在第 1 列，
/// 不算。
pub(super) fn list(screen: &str) -> Vec<String> {
    screen
        .lines()
        .filter(|line| {
            ["     main", "     主会话", "     ↑ ", "     ↓ "]
                .iter()
                .any(|head| line.starts_with(head))
                || ["   ○ ", "   ● ", "   ✓ ", "   ✗ ", "   – "]
                    .iter()
                    .any(|head| line.starts_with(head))
        })
        .map(|line| match line.rfind("   ") {
            Some(cut) if line[cut..].trim().ends_with('s') => line[..cut].trim_end().to_owned(),
            _ => line.trim_end().to_owned(),
        })
        .collect()
}

/// 任务卡开着：有种类一行（选中哪个种类都算）。
fn card(screen: &str) -> bool {
    screen
        .lines()
        .any(|l| l.starts_with("   Subagents") || l.starts_with("   子代理"))
}

/// 任务卡开在 Subagents：有主会话的那一行或藏起的行的计数。
pub(super) fn open(screen: &str) -> bool {
    list(screen).iter().any(|l| {
        l.starts_with("     main") || l.starts_with("     主会话") || l.starts_with("     ↑ ")
    })
}

/// 打开任务卡的 Subagents：卡关着时按 ↓（没有东西可看时 ↓ 不打开它），卡开在别的种类时按 ←（tui-tasks.md §3.1、§3.3）。
/// 等待有上限，只为让失败的测试结束。
pub(super) async fn open_list(ui: &super::support::Ui) -> String {
    let wait = async {
        loop {
            let screen = ui.screen();
            if open(&screen) {
                return screen;
            }
            ui.key(if card(&screen) {
                KeyCode::Left
            } else {
                KeyCode::Down
            });
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    match tokio::time::timeout(Duration::from_secs(10), wait).await {
        Ok(screen) => screen,
        Err(_) => panic!(
            "timed out opening the subagent list; screen:\n{}",
            ui.screen()
        ),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_subagents_form_a_focused_tree() {
    let model = Routed::new();
    let gates = model.clone();
    with_ui(model, wide(), |mut ui| async move {
        ui.submit("run the agents");
        // 状态栏下面没有常驻的列表：输入框上方是任务行（tui-tasks.md §2），A 派生的 A1、A1a 也算在跑的后台子代理。
        let screen = ui.wait_for("Launched.").await;
        assert!(list(&screen).is_empty(), "{screen}");
        let screen = ui
            .wait("the task line", |s| {
                s.lines().any(|l| l.starts_with(" ○ 4 subagents running "))
            })
            .await;
        let rows: Vec<&str> = screen.lines().collect();
        let line = rows
            .iter()
            .position(|l| l.starts_with(" ○ 4 subagents running "))
            .unwrap();
        assert!(rows[line].contains(" running   ↓/^T all tasks"), "{screen}");
        let board = rows.iter().position(|l| l.starts_with(" ›  ")).unwrap();
        assert_eq!(
            line + 2,
            board,
            "the task line sits right above the board\n{screen}"
        );
        // 看 main：只列顶层；A 的后代 A1、A1a 算进 `(+2)`。↓ 打开任务卡，落在 Subagents（只有它有东西）。
        open_list(&ui).await;
        let screen = ui
            .wait("the top-level rows", |s| {
                list(s)
                    .iter()
                    .any(|l| l == "   ○ general-purpose (+2)  Alpha task")
                    && list(s)
                        .iter()
                        .any(|l| l == "   ○ general-purpose       Beta task")
            })
            .await;
        assert!(
            screen.contains("   Subagents 4  ·  Commands  ·  Todos"),
            "{screen}"
        );
        assert_eq!(list(&screen).len(), 3, "{screen}");
        // 打开时选中第一个子代理 A（tui-tasks.md §3.2）；到 A1 也得先看 A。
        until_selected(&ui, "   ○ general-purpose (+2)").await;
        ui.key(KeyCode::Enter);
        // 看 A（顶层）：全部顶层 + A 的直接子代理 A1（它的孩子算进 `(+1)`）。enter 之后卡留着，选中的仍是 A（§3.3）。
        ui.wait("the view of A", |s| {
            s.lines()
                .any(|l| l.trim_end().ends_with("↳ general-purpose · Alpha task"))
                && list(s)
                    .iter()
                    .any(|l| l == "   ○ └ general-purpose (+1)  Alpha one")
        })
        .await;
        // 选中的仍是 A：往下一行就是 A1（选中的若回到了 main，这里会落在 A 上）。
        ui.key(KeyCode::Down);
        until_selected(&ui, "   ○ └ general-purpose (+1)").await;
        ui.key(KeyCode::Enter);
        // 看 A1（嵌套）：祖先链 A + A1 + A1 的孩子 A1a；别的顶层分支 B 隐藏，链上的行不写 `(+N)`。正在看的写 `current`。
        let screen = ui
            .wait("the rows of A1", |s| {
                s.lines()
                    .any(|l| l.trim_end().ends_with("↳ general-purpose · Alpha one"))
                    && list(s)
                        .iter()
                        .any(|l| l == "   ○   └ general-purpose  Alpha one deep")
            })
            .await;
        let rows = list(&screen);
        assert_eq!(rows[1], "   ○ general-purpose      Alpha task", "{screen}");
        assert_eq!(
            rows[2], "   ○ └ general-purpose    Alpha one  current",
            "{screen}"
        );
        assert!(!rows.iter().any(|l| l.contains("Beta task")), "{screen}");
        // esc 先关上卡，再回到主会话。
        ui.key(KeyCode::Esc);
        ui.wait("the card closes", |s| !open(s)).await;
        ui.key(KeyCode::Esc);
        ui.wait("back to main", |s| !s.contains("Viewing @general-purpose"))
            .await;
        // A1 跑完这一轮而 A1a 还在跑：A1 停放，照常当 A1a 的父代理、算在跑。`/tasks` 也打开任务卡（tui-tasks.md §3.1）。
        gates.release("A1");
        ui.submit("/tasks");
        ui.wait("A still counts A1", |s| {
            list(s)
                .iter()
                .any(|l| l == "   ○ general-purpose (+2)  Alpha task")
        })
        .await;
        // A1a 完成（后台），A1 收到通知续跑后也完成：完成的后台代理不再藏起（留到驱逐，tui-panels.md §1.2），A 仍写
        // `(+2)`；种类一行只数在跑的。开着的卡跟着快照变。
        gates.release("A1a");
        let screen = ui
            .wait("only A and B running", |s| {
                s.contains("   Subagents 2  ·  ")
                    && list(s)
                        .iter()
                        .any(|l| l == "   ○ general-purpose (+2)  Alpha task")
            })
            .await;
        assert!(!screen.contains("Alpha one"), "{screen}");
        gates.release("A");
        gates.release("B");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tree_speaks_chinese() {
    let model = Routed::new();
    let gates = model.clone();
    let setup = Setup {
        lang: "zh_CN.UTF-8",
        ..wide()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("派生子代理");
        ui.wait_for("Launched.").await;
        open_list(&ui).await;
        ui.wait("the top-level rows", |s| {
            list(s)
                .iter()
                .any(|l| l.starts_with("     主会话") && l.ends_with("  当前"))
                && list(s)
                    .iter()
                    .any(|l| l == "   ○ general-purpose (+2)  Alpha task")
        })
        .await;
        gates.release("A1");
        gates.release("A1a");
        ui.wait("only A and B running", |s| {
            s.contains("   子代理 2  ·  ")
                && list(s)
                    .iter()
                    .any(|l| l == "   ○ general-purpose (+2)  Alpha task")
        })
        .await;
        gates.release("A");
        gates.release("B");
    })
    .await;
}

/// 推迟一部分操作的执行环境（修复线 F34 的确定性复现），其余操作同 [`UnusedEnvironment`]：
/// - `shell_state` 先等 `shells`：后台任务快照里的命令来自它，推迟它就推迟了整份快照里命令的那一半。它第 k 次被问
///   时报 k 个在跑的后台命令（`bash_dev_1`…），所以任务卡写 `Commands k`、任务行写 `k shells running`——这个数
///   就是“第几次问命令”，一份快照用掉一个；
/// - `close_session` 等 `close` 打开：只有关闭顶层会话时调用（切换会话时旧会话的收尾），停在这里时界面已经离开旧会话、
///   新会话还没打开。
struct SlowEnv {
    shells: Duration,
    close: watch::Receiver<bool>,
    /// `shell_state` 被问过几次。
    asked: AtomicUsize,
}

impl SlowEnv {
    fn shells(shells: Duration) -> Arc<Self> {
        Arc::new(Self {
            shells,
            close: watch::channel(true).1,
            asked: AtomicUsize::new(0),
        })
    }

    /// 只有主 env 的注册表：机器只换上命令、shell 与后台命令三个端口（其余端口同 [`UnusedEnvironment`]）。
    fn registry(self: Arc<Self>) -> Arc<dyn EnvironmentRegistry> {
        registry(
            unsupported_machine()
                .with_commands(self.clone())
                .with_shell(self.clone())
                .with_background(self),
        )
    }
}

#[async_trait]
impl Commands for SlowEnv {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        UnusedEnvironment
            .start_command(workspace, context, request)
            .await
    }

    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        UnusedEnvironment.watch_command(operation, cursor)
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        UnusedEnvironment.cancel(operation).await
    }

    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        let _ = self.close.clone().wait_for(|open| *open).await;
        UnusedEnvironment.close_session(session, forget).await
    }
}

#[async_trait]
impl Shell for SlowEnv {
    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        _session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        let asked = self.asked.fetch_add(1, Ordering::Relaxed) + 1;
        tokio::time::sleep(self.shells).await;
        Ok(ShellState {
            cwd: workspace.root.clone(),
            outputs: None,
            background: (1..=asked)
                .map(|i| ShellTaskId::new(format!("bash_dev_{i}")))
                .collect(),
        })
    }
}

#[async_trait]
impl Background for SlowEnv {
    async fn background_command(
        &self,
        _request: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        Ok(Some(BackgroundInfo {
            command: "npm run dev".into(),
            status: BackgroundStatus::Running,
            ..BackgroundInfo::default()
        }))
    }
}

/// 列表里有子代理的行。
fn rows(screen: &str) -> bool {
    list(screen).iter().any(|l| l.contains(" general-purpose"))
}

/// 列表里子代理的构形：顶层行数与各行 `(+N)` 的合计（= 这份快照里可见的子代理总数）。只随快照变——用时、活动的
/// 文字与截断都不算进来，所以同一份快照画出的每一帧构形相同。
fn shape(screen: &str) -> (usize, usize) {
    let rows: Vec<String> = list(screen)
        .into_iter()
        .filter(|l| l.contains(" general-purpose"))
        .collect();
    let nested: usize = rows
        .iter()
        .filter_map(|line| {
            let at = line.find("(+")?;
            line[at + 2..].split(')').next()?.parse::<usize>().ok()
        })
        .sum();
    (rows.len(), nested)
}

/// 在跑的命令数：任务卡开着时种类一行的 `Commands k`，关着时任务行的 `k shell(s) running`（tui-tasks.md §2、§3.2）；
/// 都没有时为 None。以前是状态栏的 pill（`k shells`）。
fn shells(screen: &str) -> Option<usize> {
    if let Some(at) = screen.find("Commands ") {
        let digits: String = screen[at + 9..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        return digits.parse().ok();
    }
    let at = screen.find(" shell")?;
    let digits: String = screen[..at]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// 修复线 F34（tui-panels.md §0）：子代理的行与在跑的命令出自同一份快照，同一帧画出。替身环境让问命令这一步等 500 ms，
/// 并且第 k 次报 k 个在跑的命令，所以每份快照都有命令，种类一行的 `Commands k` 就是“这是第几份快照”。每一帧查两条
/// 不变量：
/// - 有子代理的行就一定有命令的数：修复后行只跟着一份快照到达，而那份快照里命令那一半先等了 500 ms；修复前行另走一次
///   快查询（不问命令），在第一份命令到达前 500 ms 就画出来了；
/// - 一个命令数只配一种构形：一份快照的两半不会一新一旧。
#[tokio::test(flavor = "multi_thread")]
async fn the_rows_and_the_commands_arrive_in_one_frame() {
    let model = Routed::new();
    let gates = model.clone();
    let setup = Setup {
        environments: SlowEnv::shells(Duration::from_millis(500)).registry(),
        ..wide()
    };
    with_ui(model, setup, |ui| async move {
        ui.submit("run the agents");
        // 任务卡开在 Subagents 才画行（卡关着时按 ↓，开在别的种类时按 ←）；开着的每一帧查两条不变量，直到 A、B 与命令都在。
        let mut split: Vec<String> = Vec::new();
        let mut paired: HashMap<usize, (usize, usize)> = HashMap::new();
        let watch = async {
            loop {
                let s = ui.screen();
                let shape = shape(&s);
                // 不画行的帧不算一种构形。
                if !open(&s) {
                    ui.key(if card(&s) {
                        KeyCode::Left
                    } else {
                        KeyCode::Down
                    });
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                if let Some(n) = shells(&s) {
                    if let Some(before) = paired.insert(n, shape)
                        && before != shape
                    {
                        split.push(format!("`Commands {n}` went with {before:?} and {shape:?}"));
                    }
                } else if shape != (0, 0) {
                    split.push(format!("the rows {shape:?} arrived without the commands"));
                }
                if shape == (2, 2) && shells(&s).is_some() {
                    return s;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let screen = tokio::time::timeout(Duration::from_secs(10), watch)
            .await
            .unwrap_or_else(|_| panic!("the rows of A and B with the commands; screen:\n{}", ui.screen()));
        assert!(
            split.is_empty(),
            "the subagent rows and the running commands came from different snapshots: {split:?}\nlast screen:\n{screen}"
        );
        for gate in ["A1a", "A1", "A", "B"] {
            gates.release(gate);
        }
    })
    .await;
}

/// 修复线 F34：切换会话时子代理与命令一起清掉（列表是 ↓ 打开的任务卡，打字时关上；这里查的是任务行的命令数跟着旧会话一起
/// 走）。`/resume` 先离开旧会话（不再订阅、清掉列表），新会话打开之后才整体重置；修复前命令（当时的 pill）留到新会话
/// 打开，中间是“列表没了、命令还在”的帧。旧会话的收尾（执行环境的 `close_session`）停在闸门上，让这段时间看得见：
/// 这时没有会话，也就没有快照，画面上不该有命令数（替身环境总报在跑的命令，所以旧会话有，新会话打开后又有，中间这段
/// 必须没有）。
#[tokio::test(flavor = "multi_thread")]
async fn switching_sessions_clears_the_rows_and_the_commands_together() {
    let model = Routed::new();
    let gates = model.clone();
    let store = Arc::new(MemoryStore::default());
    seed(
        &store,
        &[Seed {
            id: "sess_other",
            root: "/Users/dev/project",
            title: "Other work",
            prompt: "earlier question",
            reply: "earlier answer",
            created: 1,
            updated: 2,
        }],
    )
    .await;
    let (close, closing) = watch::channel(false);
    let setup = Setup {
        environments: Arc::new(SlowEnv {
            shells: Duration::ZERO,
            close: closing,
            asked: AtomicUsize::new(0),
        })
        .registry(),
        store,
        ..wide()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("run the agents");
        ui.wait("the commands after the turn", |s| {
            shells(s).is_some() && s.contains("Launched.") && !s.contains("esc stop")
        })
        .await;
        open_list(&ui).await;
        ui.wait("the rows", rows).await;
        // 打字关上卡；离开旧会话时命令跟着快照一起走（旧会话的收尾停在闸门上，这段时间里不该有命令，也不该有行）。
        ui.submit("/resume sess_other");
        let screen = ui
            .wait("the commands to leave", |s| shells(s).is_none())
            .await;
        assert!(
            !rows(&screen),
            "the commands left but the rows of the old session stayed:\n{screen}"
        );
        close.send_replace(true);
        let screen = ui.wait_for("earlier answer").await;
        // 新会话有自己的命令（替身环境照旧报在跑的命令），但旧会话的行没有跟过来。
        assert!(!rows(&screen), "{screen}");
        for gate in ["A1a", "A1", "A", "B"] {
            gates.release(gate);
        }
    })
    .await;
}
