//! 子代理列表的树（docs/specs/rust-cli/tui-panels.md §1.2–§1.4，修复线 F30）：内核是 LocalKernel，子代理由 agents 的
//! Agent 工具真实地嵌套派生（main → A → A1 → A1a，main 另有 B），父子关系来自快照的 `owner`、停放来自 `parked`。
//! 模型按请求来源与子代理的 prompt 回复，每个子代理挂在自己的闸门上，测试决定谁先结束。
//! 修复线 F34：列表的行与 footer 的 pill 出自同一份快照（tui-panels.md §0、§3.1），同一帧画出。
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::KeyCode;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, Environment, FileContent, OperationContext, Outcome,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_shell::ShellState;
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::model::{ModelPort, ModelRequest, QuerySource};
use rust_cli_kernel_api::testing::{
    MemoryStore, UnusedEnvironment, text_response, tool_use_response,
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
        // 看 main：只列顶层；A 的后代 A1、A1a 算进 `(+2)`。下面的 ↓ 先选中后台任务的 pill：只等行就够，画出行的那一帧
        // 一定也有 pill。修复线 F34：行与 pill 出自同一份快照（一次查询、一次重画）。F32 时两者是两次查询、先后不定，
        // 测试只能两样都等（只等行时负载下 400 次里 11 次 pill 晚到，↓ 直接进了列表）；现在改回只等行，并断言同一画面上
        // 已有 pill。
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
        assert!(screen.contains("· 4 local agents"), "{screen}");
        assert_eq!(list(&screen).len(), 3, "{screen}");
        // ↓ 先选中后台任务的 pill，再进入列表，往下到 A 再到 A1 也得先看 A。
        ui.key(KeyCode::Down);
        ui.wait_for("Enter to view tasks").await;
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
        ui.key(KeyCode::Down);
        ui.wait_for("Enter to view tasks").await;
        ui.key(KeyCode::Enter);
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
/// - `shell_state` 先等 `shells`：后台任务快照里的命令来自它；修复前 pill 是单独的一次查询，推迟它就推迟了 pill，
///   子代理的行照常先到；
/// - `close_session` 等 `close` 打开：只有关闭顶层会话时调用（切换会话时旧会话的收尾），停在这里时界面已经离开旧会话、
///   新会话还没打开。
struct SlowEnv {
    shells: Duration,
    close: watch::Receiver<bool>,
}

impl SlowEnv {
    fn shells(shells: Duration) -> Arc<Self> {
        Arc::new(Self {
            shells,
            close: watch::channel(true).1,
        })
    }
}

#[async_trait::async_trait]
impl Environment for SlowEnv {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        UnusedEnvironment.facts(workspace).await
    }

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

    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        tokio::time::sleep(self.shells).await;
        UnusedEnvironment.shell_state(workspace, session).await
    }
}

/// 列表里有子代理的行。
fn rows(screen: &str) -> bool {
    list(screen).iter().any(|l| l.contains(" general-purpose"))
}

/// 修复线 F34（tui-panels.md §0、§3.1）：子代理的行与 footer 的 pill 同一帧出现。shell 查询推迟 500 ms 时，修复前的
/// 实现先画出子代理的行（agents 的变化通知之后的列表查询），500 ms 后 pill 才到（主会话事件之后的后台任务查询）；修复后
/// 两者是同一次查询，一起等这 500 ms。画面上不许出现“有子代理的行、没有 pill”或反过来的帧。
#[tokio::test(flavor = "multi_thread")]
async fn the_rows_and_the_pill_arrive_in_one_frame() {
    let model = Routed::new();
    let gates = model.clone();
    let setup = Setup {
        env: SlowEnv::shells(Duration::from_millis(500)),
        ..wide()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("run the agents");
        let split = AtomicBool::new(false);
        let screen = ui
            .wait("the rows of A and B with the pill", |s| {
                let pill = s.contains(" local agent");
                if rows(s) != pill {
                    split.store(true, Ordering::Relaxed);
                }
                rows(s) && pill && list(s).iter().any(|l| l.contains("Beta task"))
            })
            .await;
        assert!(
            !split.load(Ordering::Relaxed),
            "a frame showed the subagent rows and the pill apart; last screen:\n{screen}"
        );
        for gate in ["A1a", "A1", "A", "B"] {
            gates.release(gate);
        }
    })
    .await;
}

/// 修复线 F34：切换会话时列表与 pill 一起清掉。`/resume` 先离开旧会话（不再订阅、清掉列表），新会话打开之后才整体
/// 重置；修复前 pill 留到新会话打开，中间是“列表没了、pill 还在”的帧。旧会话的收尾（执行环境的 `close_session`）停在
/// 闸门上，让这段时间看得见。
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
        env: Arc::new(SlowEnv {
            shells: Duration::ZERO,
            close: closing,
        }),
        store,
        ..wide()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("run the agents");
        ui.wait("the rows and the pill after the turn", |s| {
            rows(s)
                && s.contains(" local agent")
                && s.contains("Launched.")
                && !s.contains("esc to interrupt")
        })
        .await;
        ui.submit("/resume sess_other");
        let screen = ui.wait("the rows to leave", |s| !rows(s)).await;
        assert!(
            !screen.contains(" local agent"),
            "the rows left but the pill of the old session stayed:\n{screen}"
        );
        close.send_replace(true);
        let screen = ui.wait_for("earlier answer").await;
        assert!(!screen.contains(" local agent"), "{screen}");
        for gate in ["A1a", "A1", "A", "B"] {
            gates.release(gate);
        }
    })
    .await;
}
