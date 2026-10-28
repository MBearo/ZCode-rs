//! 子代理列表的树（docs/specs/rust-cli/tui-panels.md §1.2–§1.4，修复线 F30）：内核是 LocalKernel，子代理由 agents 的
//! Agent 工具真实地嵌套派生（main → A → A1 → A1a，main 另有 B），父子关系来自快照的 `owner`、停放来自 `parked`。
//! 模型按请求来源与子代理的 prompt 回复，每个子代理挂在自己的闸门上，测试决定谁先结束。
//! 修复线 F34：列表的行与 footer 的 pill 出自同一份快照（tui-panels.md §0、§3.1），同一帧画出。
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossterm::event::KeyCode;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, FileContent, OperationContext, Outcome, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStatus, BackgroundTask, ShellState,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId, ShellTaskId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::model::{ModelPort, ModelRequest, QuerySource};
use rust_cli_kernel_api::testing::{
    MemoryStore, UnusedEnvironment, environments, text_response, tool_use_response,
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

/// footer 下面的列表：从 main 那一行到画面末尾（右侧的用时去掉）。
fn list(screen: &str) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(at) = lines
        .iter()
        .rposition(|line| line.contains(" main") || line.contains(" 主会话"))
    else {
        return Vec::new();
    };
    lines[at..]
        .iter()
        .map(|line| match line.rfind("   ") {
            Some(cut) if line[cut..].trim().ends_with('s') => line[..cut].trim_end().to_owned(),
            _ => line.trim_end().to_owned(),
        })
        .filter(|line| !line.is_empty())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_subagents_form_a_focused_tree() {
    let model = Routed::new();
    let gates = model.clone();
    with_ui(model, wide(), |mut ui| async move {
        ui.submit("run the agents");
        // 看 main：只列顶层；A 的后代 A1、A1a 算进 `(+2)`。修复线 F105：在跑的全是子代理，footer 没有 pill（列表已经
        // 列出它们），↓ 直接进入列表。
        let screen = ui
            .wait("the top-level rows", |s| {
                list(s)
                    .iter()
                    .any(|l| l == "  ◯ general-purpose (+2)  Alpha task")
                    && list(s)
                        .iter()
                        .any(|l| l == "  ◯ general-purpose       Beta task")
            })
            .await;
        assert!(!screen.contains("local agent"), "{screen}");
        assert!(!screen.contains("· ↓ to manage"), "{screen}");
        assert_eq!(list(&screen).len(), 3, "{screen}");
        // ↓ 进入列表，往下到 A 再到 A1 也得先看 A。
        ui.key(KeyCode::Down);
        ui.wait_for("↑/↓ to select").await;
        ui.key(KeyCode::Down);
        ui.wait_for("› ◯ general-purpose (+2)").await;
        ui.key(KeyCode::Enter);
        // 看 A（顶层）：全部顶层 + A 的直接子代理 A1（它的孩子算进 `(+1)`）。
        ui.wait("the view of A", |s| {
            list(s)
                .iter()
                .any(|l| l == "  └ ◯ general-purpose (+1)  Alpha one")
        })
        .await;
        ui.key(KeyCode::Down);
        ui.wait_for("↑/↓ to select").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.wait_for("› └ ◯ general-purpose (+1)").await;
        ui.key(KeyCode::Enter);
        // 看 A1（嵌套）：祖先链 A + A1 + A1 的孩子 A1a；别的顶层分支 B 隐藏，链上的行不写 `(+N)`。
        let screen = ui
            .wait("the view of A1", |s| {
                list(s)
                    .iter()
                    .any(|l| l == "    └ ◯ general-purpose  Alpha one deep")
            })
            .await;
        let rows = list(&screen);
        assert_eq!(rows[1], "  ◯ general-purpose      Alpha task", "{screen}");
        assert!(
            rows[2].starts_with("  └ ") && rows[2].ends_with(" general-purpose    Alpha one"),
            "{screen}"
        );
        assert!(!rows.iter().any(|l| l.contains("Beta task")), "{screen}");
        // A1 跑完这一轮而 A1a 还在跑：A1 停放。等 Background 面板（同一份快照）里它是 done，再回到列表：停放的 A1 照常
        // 当 A1a 的父代理，A 仍是 `(+2)`（不认停放时 A1 算隐藏的已完成后台代理，A 只剩 `(+1)`）。
        ui.key(KeyCode::Esc);
        ui.wait("back to main", |s| {
            s.lines().any(|l| l.ends_with(" main") && !l.contains('◯'))
        })
        .await;
        gates.release("A1");
        // 全是子代理时没有 pill（F105），Background 面板从 `/tasks` 打开。
        ui.submit("/tasks");
        ui.wait("A1 is done in the panel", |s| {
            s.lines().any(|l| {
                l.contains("Alpha one ") && !l.contains("deep") && l.trim_end().ends_with("done")
            })
        })
        .await;
        ui.key(KeyCode::Esc);
        ui.wait("A still counts the parked A1", |s| {
            list(s)
                .iter()
                .any(|l| l == "  ◯ general-purpose (+2)  Alpha task")
        })
        .await;
        // A1a 完成（后台）：立即隐藏；A1 收到通知续跑后也完成、隐藏。A 只剩自己，footer 指向 `/tasks`。
        gates.release("A1a");
        let screen = ui
            .wait("the hidden agents", |s| {
                list(s)
                    .iter()
                    .any(|l| l == "  ◯ general-purpose  Alpha task")
                    && s.contains("/tasks to see subagents")
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
        ui.wait("the top-level rows", |s| {
            list(s).iter().any(|l| l.ends_with(" 主会话"))
                && list(s)
                    .iter()
                    .any(|l| l == "  ◯ general-purpose (+2)  Alpha task")
        })
        .await;
        gates.release("A1");
        gates.release("A1a");
        ui.wait_for("/tasks 查看子代理").await;
        gates.release("A");
        gates.release("B");
    })
    .await;
}

/// 推迟一部分操作的执行环境（修复线 F34 的确定性复现），其余操作同 [`UnusedEnvironment`]：
/// - `shell_state` 先等 `shells`：后台任务快照里的命令来自它，推迟它就推迟了整份快照里命令的那一半。它第 k 次被问
///   时报 k 个在跑的后台命令（`bash_dev_1`…），所以 footer 的 pill 写 `k shells`——这个数就是“第几次问命令”，
///   一份快照用掉一个。修复线 F105 起子代理不进 pill，pill 只数这些命令；
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
}

#[async_trait]
impl EnvInfo for SlowEnv {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        UnusedEnvironment.facts(workspace).await
    }
}

#[async_trait]
impl Files for SlowEnv {
    async fn read_file(
        &self,
        workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        UnusedEnvironment.read_file(workspace, request).await
    }

    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        UnusedEnvironment
            .write_file(workspace, context, request)
            .await
    }
}

#[async_trait]
impl Commands for SlowEnv {
    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
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

    async fn operation_status(&self, operation: OperationId) -> Result<Outcome, EnvError> {
        UnusedEnvironment.operation_status(operation).await
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
        }))
    }
}

impl Search for SlowEnv {}
impl Checkpoints for SlowEnv {}
impl Pdf for SlowEnv {}
impl Git for SlowEnv {}
impl Fetch for SlowEnv {}

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

/// footer 的 pill 里在跑的命令数（`N shells`）；没有 pill 时为 None。
fn shells(screen: &str) -> Option<usize> {
    let at = screen.find(" shell")?;
    let digits: String = screen[..at]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// 修复线 F34（tui-panels.md §0、§3.1）：子代理的行与 footer 的 pill 出自同一份快照，同一帧画出。替身环境让问命令
/// 这一步等 500 ms，并且第 k 次报 k 个在跑的命令，所以每份快照都有 pill，pill 的数就是“这是第几份快照”。每一帧查
/// 两条不变量：
/// - 有子代理的行就一定有 pill：修复后行只跟着一份快照到达，而那份快照里命令那一半先等了 500 ms；修复前行另走一次
///   快查询（不问命令），在第一个 pill 到达前 500 ms 就画出来了；
/// - 一个 pill 的数只配一种构形：一份快照的两半不会一新一旧。
#[tokio::test(flavor = "multi_thread")]
async fn the_rows_and_the_pill_arrive_in_one_frame() {
    let model = Routed::new();
    let gates = model.clone();
    let setup = Setup {
        environments: environments(SlowEnv::shells(Duration::from_millis(500))),
        ..wide()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("run the agents");
        let split: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let paired: Mutex<HashMap<usize, (usize, usize)>> = Mutex::new(HashMap::new());
        let screen = ui
            .wait("the rows of A and B with the pill", |s| {
                let shape = shape(s);
                if let Some(n) = shells(s) {
                    let before = paired.lock().unwrap().insert(n, shape);
                    if let Some(before) = before
                        && before != shape
                    {
                        split
                            .lock()
                            .unwrap()
                            .push(format!("`{n} shells` went with {before:?} and {shape:?}"));
                    }
                } else if shape != (0, 0) {
                    split
                        .lock()
                        .unwrap()
                        .push(format!("the rows {shape:?} arrived without a pill"));
                }
                shape == (2, 2) && shells(s).is_some()
            })
            .await;
        let split = split.lock().unwrap();
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

/// 修复线 F34：切换会话时列表与 pill 一起清掉。`/resume` 先离开旧会话（不再订阅、清掉列表），新会话打开之后才整体
/// 重置；修复前 pill 留到新会话打开，中间是“列表没了、pill 还在”的帧。旧会话的收尾（执行环境的 `close_session`）停在
/// 闸门上，让这段时间看得见：这时没有会话，也就没有快照，画面上不该有任何 pill（替身环境总报在跑的命令，所以旧会话
/// 有 pill，新会话打开后又有，中间这段必须没有）。
#[tokio::test(flavor = "multi_thread")]
async fn switching_sessions_clears_the_rows_and_the_pill_together() {
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
        environments: environments(Arc::new(SlowEnv {
            shells: Duration::ZERO,
            close: closing,
            asked: AtomicUsize::new(0),
        })),
        store,
        ..wide()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("run the agents");
        ui.wait("the rows and the pill after the turn", |s| {
            rows(s)
                && shells(s).is_some()
                && s.contains("Launched.")
                && !s.contains("esc to interrupt")
        })
        .await;
        ui.submit("/resume sess_other");
        let screen = ui.wait("the rows to leave", |s| !rows(s)).await;
        assert!(
            shells(&screen).is_none(),
            "the rows left but the pill of the old session stayed:\n{screen}"
        );
        close.send_replace(true);
        let screen = ui.wait_for("earlier answer").await;
        // 新会话有自己的 pill（替身环境照旧报在跑的命令），但旧会话的行没有跟过来。
        assert!(!rows(&screen), "{screen}");
        for gate in ["A1a", "A1", "A", "B"] {
            gates.release(gate);
        }
    })
    .await;
}
