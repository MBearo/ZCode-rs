//! 远端执行 R4（docs/specs/rust-cli/remote-exec.md §21）：`/machines` 的列表与子命令、机器状态行。机器服务用替身
//! （[`FakeMachines`]），它照入口的约定改快照、发出变化。工具行、询问框与颜色在 `machines_marks.rs`。
//! 旧代码没有 `/machines`（未知命令）与状态行，这些测试都失败。
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use rust_cli_tui::machines::{
    Changed, ExecutorInfo, MachineFailure, MachineProblem, MachineRow, MachineStatus, Machines,
};
use tokio::sync::watch;

use super::support::{GatedModel, Setup, Ui, row_of, squash, with_ui};

/// 机器服务的替身：内存里的清单；`connect` 按 `failures` 失败，否则就绪（12 ms、Linux aarch64、0.1.58）。
#[derive(Default)]
pub struct FakeMachines {
    rows: Mutex<Vec<MachineRow>>,
    changes: Mutex<Option<watch::Sender<u64>>>,
    pub calls: Mutex<Vec<String>>,
    pub failures: Mutex<BTreeMap<String, MachineFailure>>,
}

pub fn row(name: &str, status: MachineStatus) -> MachineRow {
    let ready = status == MachineStatus::Ready;
    MachineRow {
        name: name.to_owned(),
        ssh: name.to_owned(),
        working_directory: None,
        status,
        latency_ms: ready.then_some(12),
        executor: ready.then(|| ExecutorInfo {
            os: "linux".to_owned(),
            arch: "aarch64".to_owned(),
            version: "0.1.58".to_owned(),
            refreshes_env: true,
            home: Some("/home/dev".to_owned()),
        }),
    }
}

impl FakeMachines {
    pub fn with(rows: Vec<MachineRow>) -> Arc<Self> {
        let fake = Arc::new(Self::default());
        *fake.rows.lock().unwrap() = rows;
        fake
    }

    fn sender(&self) -> watch::Sender<u64> {
        self.changes
            .lock()
            .unwrap()
            .get_or_insert_with(|| watch::channel(0).0)
            .clone()
    }

    /// 改快照并发出变化（入口的状态机变了）。
    pub fn update(&self, change: impl FnOnce(&mut Vec<MachineRow>)) {
        change(&mut self.rows.lock().unwrap());
        self.sender().send_modify(|version| *version += 1);
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn has(&self, name: &str) -> bool {
        self.rows.lock().unwrap().iter().any(|row| row.name == name)
    }

    fn status(&self, name: &str) -> Option<MachineStatus> {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .find(|row| row.name == name)
            .map(|row| row.status.clone())
    }

    pub fn set(&self, name: &str, status: MachineStatus) {
        let fresh = row(name, status);
        self.update(|rows| {
            if let Some(found) = rows.iter_mut().find(|row| row.name == name) {
                found.status = fresh.status;
                found.latency_ms = fresh.latency_ms;
                found.executor = fresh.executor.or(found.executor.take());
            }
        });
    }
}

impl Machines for FakeMachines {
    fn snapshot(&self) -> Vec<MachineRow> {
        self.rows.lock().unwrap().clone()
    }

    fn changes(&self) -> BoxStream<'static, ()> {
        let receiver = self.sender().subscribe();
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            Some(((), receiver))
        })
        .boxed()
    }

    fn add(
        &self,
        name: String,
        ssh: String,
        working_directory: Option<String>,
    ) -> BoxFuture<'static, Result<(), MachineProblem>> {
        self.record(format!(
            "add {name} {ssh} {}",
            working_directory.clone().unwrap_or_default()
        ));
        let result = if name == "local" {
            Err(MachineProblem::InvalidName(name))
        } else if self.has(&name) {
            Err(MachineProblem::Exists(name))
        } else {
            let mut added = row(&name, MachineStatus::Disconnected);
            added.ssh = ssh;
            added.working_directory = working_directory;
            self.update(|rows| rows.push(added));
            Ok(())
        };
        Box::pin(async move { result })
    }

    fn connect(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>> {
        self.record(format!("connect {name}"));
        let failure = self.failures.lock().unwrap().get(&name).cloned();
        let result = match (self.status(&name), failure) {
            (None, _) => Err(MachineProblem::Missing(name)),
            (Some(MachineStatus::Ready), _) => Ok(Changed::Already),
            (Some(_), Some(failure)) => {
                self.set(&name, MachineStatus::Failed(failure.clone()));
                Err(MachineProblem::Connection(failure))
            }
            (Some(_), None) => {
                self.set(&name, MachineStatus::Ready);
                Ok(Changed::Done)
            }
        };
        Box::pin(async move { result })
    }

    fn disconnect(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>> {
        self.record(format!("disconnect {name}"));
        let result = match self.status(&name) {
            None => Err(MachineProblem::Missing(name)),
            Some(status) if !status.is_live() => Ok(Changed::Already),
            Some(_) => {
                self.set(&name, MachineStatus::Disconnected);
                Ok(Changed::Done)
            }
        };
        Box::pin(async move { result })
    }

    /// 立即重试（F98）：在等重连的机器马上连上；别的状态“没在等”。
    fn retry(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>> {
        self.record(format!("retry {name}"));
        let result = match self.status(&name) {
            None => Err(MachineProblem::Missing(name)),
            Some(status) if status.awaits_retry() => {
                self.set(&name, MachineStatus::Ready);
                Ok(Changed::Done)
            }
            Some(_) => Ok(Changed::Already),
        };
        Box::pin(async move { result })
    }

    fn remove(&self, name: String) -> BoxFuture<'static, Result<(), MachineProblem>> {
        self.record(format!("remove {name}"));
        let result = if self.has(&name) {
            self.update(|rows| rows.retain(|row| row.name != name));
            Ok(())
        } else {
            Err(MachineProblem::Missing(name))
        };
        Box::pin(async move { result })
    }
}

/// 四种状态各一台。
pub fn four() -> Vec<MachineRow> {
    vec![
        row("dev-a", MachineStatus::Ready),
        row(
            "gpu-1",
            MachineStatus::Reconnecting {
                attempt: 2,
                retry_at_ms: None,
                reason: "connection refused".to_owned(),
            },
        ),
        row(
            "ci-3",
            MachineStatus::Failed(MachineFailure::Unreachable("connection refused".to_owned())),
        ),
        row("old", MachineStatus::Disconnected),
    ]
}

pub fn setup_with(fake: Option<Arc<FakeMachines>>, lang: &'static str) -> Setup {
    let mut setup = Setup {
        width: 100,
        height: 30,
        lang,
        ..Setup::default()
    };
    setup.services.machines = fake.map(|fake| fake as Arc<dyn Machines>);
    setup
}

pub async fn ready(ui: &mut Ui) -> String {
    ui.wait("the composer", |s| {
        s.contains("? keys") || s.contains("? 按键")
    })
    .await
}

pub fn idle() -> Arc<GatedModel> {
    GatedModel::scripted(Vec::new())
}

/// 没有清单里的机器时状态行不存在：有服务、清单为空的画面与没有服务时逐字相同（§21.5）。
#[tokio::test(flavor = "multi_thread")]
async fn without_machines_the_screen_is_unchanged() {
    let mut screens = Vec::new();
    for fake in [None, Some(FakeMachines::with(Vec::new()))] {
        let seen = Arc::new(Mutex::new(String::new()));
        let keep = seen.clone();
        with_ui(
            idle(),
            setup_with(fake, "en_US.UTF-8"),
            |mut ui| async move {
                *keep.lock().unwrap() = ready(&mut ui).await;
            },
        )
        .await;
        let screen = seen.lock().unwrap().clone();
        assert!(!screen.contains("● local"), "{screen}");
        screens.push(screen);
    }
    assert_eq!(screens[0], screens[1]);
}

/// 坞（tui-kit.md §4.0 第 6 条，Puffin `machines_row`）：板上面一行，只有在变的机器（连接中、重连中、连不上），每台的圆点、
/// 名字与这一步；快照变了马上重画，都不在变时没有这一行。以前是状态栏下面常驻的一行，从 `● local` 起列出每台（§21.5）。
/// 画面最后一行非空的字（状态栏）。
fn status_bar(screen: &str) -> &str {
    screen
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_end()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dock_shows_the_changing_machines_and_follows_changes() {
    let fake = FakeMachines::with(four());
    let control = fake.clone();
    with_ui(
        idle(),
        setup_with(Some(fake), "en_US.UTF-8"),
        |mut ui| async move {
            let line =
                "○ gpu-1  reconnecting · attempt 2  ✗ ci-3  couldn't connect · /machines shows why";
            let screen = ui.wait_for(line).await;
            // 板上面（输入框与状态栏在它下面）；就绪与没连的不写。就绪的写在状态栏的末尾（Puffin `machine_rows::where_`）。
            assert!(
                row_of(&screen, line) < row_of(&screen, "Ask before changes"),
                "{screen}"
            );
            assert!(
                !screen.contains("○ dev-a") && !screen.contains("old"),
                "{screen}"
            );
            assert!(status_bar(&screen).ends_with("@dev-a"), "{screen}");
            control.set("gpu-1", MachineStatus::Ready);
            let screen = ui
                .wait("gpu-1 to leave the dock", |s| {
                    s.contains("✗ ci-3  couldn't connect") && !s.contains("○ gpu-1")
                })
                .await;
            assert!(status_bar(&screen).ends_with("@dev-a +1"), "{screen}");
            control.update(Vec::clear);
            ui.wait("the dock to go", |s| !s.contains("ci-3")).await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dock_in_chinese() {
    with_ui(
        idle(),
        setup_with(Some(FakeMachines::with(four())), "zh_CN.UTF-8"),
        |mut ui| async move {
            ui.wait_for("○ gpu-1  重连中 · 第 2 次  ✗ ci-3  连接失败 · /machines 查看原因")
                .await;
        },
    )
    .await;
}

fn list_rows() -> Vec<MachineRow> {
    let mut dev = row("dev-a", MachineStatus::Ready);
    dev.working_directory = Some("/srv/proj".to_owned());
    vec![
        dev,
        row(
            "gpu-1",
            MachineStatus::Failed(MachineFailure::Unreachable("connection refused".to_owned())),
        ),
    ]
}

/// 屏幕上以 `left` 开头（去掉缩进）的那一行，去掉行尾空白；没有时报出整屏。
fn line_with<'a>(screen: &'a str, left: &str) -> &'a str {
    screen
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(left))
        .unwrap_or_else(|| panic!("{left}\n{screen}"))
}

/// `/machines` 的列表（§21.3；照 Puffin 的 `/machines`，tui-kit.md §4.2）：头一行写 `1 of 2 connected`，每台一行（记号、
/// 名字、系统、工作目录，状态靠右），选中的那台下面是它的细节（失败的说明只在选中时），最后是 `+ Add a machine`；槽随焦点
/// 写 `⏎ details  space <操作>`（明显的 `esc close` 不写，kit `pick`）。空格做随状态的操作、`x` 删除、esc 关闭，结果写进 transcript，列表随快照刷新。
/// 以前头一行是 `2 machines · 1 connected`、enter 随状态连或断、槽是一句长的说明。
#[tokio::test(flavor = "multi_thread")]
async fn the_list_shows_each_machine_and_acts_on_the_focused_one() {
    let fake = FakeMachines::with(list_rows());
    let calls = fake.clone();
    with_ui(
        idle(),
        setup_with(Some(fake), "en_US.UTF-8"),
        |mut ui| async move {
            ready(&mut ui).await;
            ui.submit("/machines");
            let screen = ui.wait_for("1 of 2 connected").await;
            for text in [
                "   Machines   1 of 2 connected",
                "     ssh dev-a  ·  executor 0.1.58",
                "   + Add a machine",
                "⏎ details  space disconnect",
            ] {
                assert!(screen.contains(text), "{text}\n{screen}");
            }
            let dev = line_with(&screen, "● dev-a");
            assert!(
                dev.starts_with("● dev-a       Linux aarch64   /srv/proj")
                    && dev.ends_with(" 12ms"),
                "{screen}"
            );
            let gpu = line_with(&screen, "✗ gpu-1");
            assert!(
                gpu.starts_with(&format!("✗ gpu-1{}remote home", " ".repeat(23)))
                    && gpu.ends_with(" couldn't connect"),
                "{screen}"
            );
            // 失败的说明只在选中那台时出现（Puffin 的细节行）。
            assert!(!screen.contains("connection refused."), "{screen}");
            ui.key(KeyCode::Down);
            let screen = ui.wait_for("space try again").await;
            for text in [
                "     connection refused.",
                "     Is the machine on? ssh gpu-1 in another terminal shows what ssh sees.",
            ] {
                assert!(screen.contains(text), "{text}\n{screen}");
            }
            ui.key(KeyCode::Char(' '));
            ui.wait_for("Connected gpu-1.").await;
            ui.wait_for("2 of 2 connected").await;
            ui.key(KeyCode::Char('x'));
            ui.wait_for("Removed gpu-1.").await;
            // 删掉之后焦点按原下标落到相邻的一项（`+ Add a machine`），同 Puffin `machine_focus`。
            let screen = ui.wait_for("1 of 1 connected").await;
            assert!(screen.contains("⏎ add"), "{screen}");
            assert!(!screen.contains("ssh dev-a"), "{screen}");
            ui.key(KeyCode::Esc);
            ui.wait("the list to close", |s| !s.contains("1 of 1 connected"))
                .await;
        },
    )
    .await;
    assert_eq!(
        *calls.calls.lock().unwrap(),
        ["connect gpu-1", "remove gpu-1"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_in_chinese_and_when_empty() {
    with_ui(
        idle(),
        setup_with(Some(FakeMachines::with(list_rows())), "zh_CN.UTF-8"),
        |mut ui| async move {
            ready(&mut ui).await;
            ui.submit("/machines");
            // 中文换字不换排法（tui-kit.md §4.1 第 4 条）。
            let screen = ui.wait_for("2 台里 1 台已连接").await;
            for text in [
                "   远端机器   2 台里 1 台已连接",
                "     ssh dev-a  ·  执行器 0.1.58",
                "   + 添加机器",
                "⏎ 详情  space 断开",
            ] {
                assert!(screen.contains(text), "{text}\n{screen}");
            }
            assert!(
                line_with(&screen, "✗ gpu-1")
                    .starts_with(&format!("✗ gpu-1{}远端 home", " ".repeat(23)))
                    && line_with(&screen, "✗ gpu-1").ends_with(" 连接失败"),
                "{screen}"
            );
            ui.key(KeyCode::Down);
            ui.wait_for("     connection refused。").await;
            ui.wait_for("     机器开着吗？在另一个终端里运行 ssh gpu-1 看看 ssh 怎么说。")
                .await;
        },
    )
    .await;
    with_ui(
        idle(),
        setup_with(Some(FakeMachines::with(Vec::new())), "en_US.UTF-8"),
        |mut ui| async move {
            ready(&mut ui).await;
            ui.submit("/machines");
            // 没有机器时同 Puffin：一句话与 `+ Add a machine`（替身没有 `~/.ssh/config` 的主机）。
            let screen = ui.wait_for("Machines   none yet").await;
            for text in [
                "   Let the agent run its tools on another computer over SSH.",
                "   + Add a machine",
                "⏎ add",
            ] {
                assert!(screen.contains(text), "{text}\n{screen}");
            }
        },
    )
    .await;
}

/// 子命令（§21.3）：结果写进 transcript；add 之后马上连；连接失败写出原因（§21.4）。
#[tokio::test(flavor = "multi_thread")]
async fn subcommands_change_the_list_and_report_back() {
    let fake = FakeMachines::with(Vec::new());
    fake.failures.lock().unwrap().insert(
        "bad".to_owned(),
        MachineFailure::Unreachable("connection refused".to_owned()),
    );
    let calls = fake.clone();
    with_ui(
        idle(),
        setup_with(Some(fake), "en_US.UTF-8"),
        |mut ui| async move {
            ready(&mut ui).await;
            // `Added dev-b (ssh host). Connecting…` 在输入框上方，替身马上连上时换成 `Connected dev-b.`（一次一句，
            // tui-kit.md §4.0「本地命令的结果」）。
            ui.submit("/machines add dev-b host /srv/my proj");
            let steps: [(&str, &str); 11] = [
                ("", "Connected dev-b."),
                ("/machines add dev-b other", "A machine named \"dev-b\" already exists."),
                ("/machines connect dev-b", "dev-b is already connected."),
                ("/machines disconnect dev-b", "Disconnected dev-b."),
                ("/machines disconnect dev-b", "dev-b isn't connected."),
                ("/machines remove dev-b", "Removed dev-b."),
                (
                    "/machines connect nope",
                    "There's no machine named \"nope\". Run /machines to see the list.",
                ),
                ("/machines add bad host", "Couldn't reach bad over SSH: connection refused"),
                (
                    "/machines add local host",
                    "\"local\" can't be a machine name: use 1-32 letters, digits, '.', '_' or '-', and not \"local\".",
                ),
                ("/machines frob x", "Usage: /machines [add <name> <ssh target>"),
                ("/machines add onlyname", "Usage: /machines [add <name> <ssh target>"),
            ];
            for (input, answer) in steps {
                if !input.is_empty() {
                    ui.submit(input);
                }
                ui.wait(answer, |s| squash(s).contains(&squash(answer))).await;
            }
        },
    )
    .await;
    assert_eq!(
        *calls.calls.lock().unwrap(),
        [
            "add dev-b host /srv/my proj",
            "connect dev-b",
            "add dev-b other ",
            "connect dev-b",
            "disconnect dev-b",
            "disconnect dev-b",
            "remove dev-b",
            "connect nope",
            "add bad host ",
            "connect bad",
            "add local host ",
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn subcommands_in_chinese() {
    with_ui(
        idle(),
        setup_with(Some(FakeMachines::with(Vec::new())), "zh_CN.UTF-8"),
        |mut ui| async move {
            ready(&mut ui).await;
            for (input, answer) in [
                ("/machines add dev-b host", "已连上 dev-b。"),
                ("/machines disconnect dev-b", "已断开 dev-b。"),
                (
                    "/machines connect nope",
                    "没有叫“nope”的机器。用 /machines 查看列表。",
                ),
                ("/machines remove dev-b", "已删除 dev-b。"),
            ] {
                ui.submit(input);
                ui.wait_for(answer).await;
            }
        },
    )
    .await;
}

/// 没有机器服务时（§21.3）：提示不可用，不当未知命令。
#[tokio::test(flavor = "multi_thread")]
async fn without_the_service_machines_is_unavailable() {
    with_ui(
        idle(),
        setup_with(None, "en_US.UTF-8"),
        |mut ui| async move {
            ready(&mut ui).await;
            ui.submit("/machines");
            ui.wait_for("Remote machines aren't available here.").await;
            ui.submit("/machines add dev-a host");
            let screen = ui.wait_for("Remote machines aren't available here.").await;
            assert!(!screen.contains("Unrecognized command"), "{screen}");
        },
    )
    .await;
}

/// 执行器协议不兼容且忙（§21.4、§17.1）：写明版本与协议，以及什么时候能换（照 Puffin，原因一行，放不下时截掉；下面是
/// 建议）。
#[tokio::test(flavor = "multi_thread")]
async fn a_busy_incompatible_executor_says_how_to_upgrade() {
    let rows = vec![row(
        "gpu-1",
        MachineStatus::Failed(MachineFailure::Incompatible {
            version: "0.1.40".to_owned(),
            protocol: 0,
            busy: true,
        }),
    )];
    for (lang, wanted, advice) in [
        (
            "en_US.UTF-8",
            "     The executor on gpu-1 is zcode 0.1.40 (protocol 0) and is busy",
            "     Connect again once its work ends.",
        ),
        (
            "zh_CN.UTF-8",
            "     gpu-1 上的执行器是 zcode 0.1.40（协议 0），正忙",
            "     等它的工作结束后再连。",
        ),
    ] {
        with_ui(
            idle(),
            setup_with(Some(FakeMachines::with(rows.clone())), lang),
            |mut ui| async move {
                ready(&mut ui).await;
                ui.submit("/machines");
                let screen = ui.wait_for(wanted).await;
                assert!(screen.contains(advice), "{advice}\n{screen}");
            },
        )
        .await;
    }
}

/// 真实时间（毫秒）：界面的时钟（`WallClock`）同它。
fn wall_ms() -> u64 {
    use rust_cli_kernel_api::clock::Clock;
    super::support::WallClock.now_ms()
}

/// 等下一次重连的机器（F96，remote-exec.md §23.4）：12 s 之后第 3 次，最近一次的原因。
pub fn waiting(name: &str, lead_ms: u64) -> MachineRow {
    row(
        name,
        MachineStatus::Reconnecting {
            attempt: 3,
            retry_at_ms: Some(wall_ms() + lead_ms),
            reason: "ssh: connect to host gpu-1 port 22: Connection refused".to_owned(),
        },
    )
}

/// F96（remote-exec.md §23.4）：一直重连时状态行写倒计时与第几次，照模型重试的写法；每秒重画，倒计时往下走。
#[tokio::test(flavor = "multi_thread")]
async fn the_dock_counts_down_to_the_next_reconnect() {
    let fake = FakeMachines::with(vec![
        row("dev-a", MachineStatus::Ready),
        waiting("gpu-1", 12_400),
    ]);
    with_ui(
        idle(),
        setup_with(Some(fake), "en_US.UTF-8"),
        |mut ui| async move {
            ui.wait_for("○ gpu-1  reconnecting · next try in 12s · attempt 3")
                .await;
            // 没有别的事件，倒计时也往下走（每秒重画）。
            ui.wait_for("○ gpu-1  reconnecting · next try in 10s · attempt 3")
                .await;
        },
    )
    .await;
    with_ui(
        idle(),
        setup_with(
            Some(FakeMachines::with(vec![waiting("gpu-1", 12_400)])),
            "zh_CN.UTF-8",
        ),
        |mut ui| async move {
            ui.wait_for("○ gpu-1  重连中 · 12s后重试 · 第 3 次").await;
        },
    )
    .await;
}

/// F96：列表里重连中的机器写同样的状态，选中时下面是最近一次的原因；执行器太旧、不能刷新环境时细节行写明（§23.5）。
#[tokio::test(flavor = "multi_thread")]
async fn the_list_shows_the_reconnect_reason_and_old_executors() {
    let mut old = row("dev-a", MachineStatus::Ready);
    if let Some(executor) = &mut old.executor {
        executor.refreshes_env = false;
    }
    let fake = FakeMachines::with(vec![old, waiting("gpu-1", 30_400)]);
    with_ui(
        idle(),
        setup_with(Some(fake), "en_US.UTF-8"),
        |mut ui| async move {
            ready(&mut ui).await;
            ui.submit("/machines");
            // 倒计时每秒往下走：等它走到 30s。
            // 行尾照 Puffin 写 `reconnecting · 30s`（第几次在坞行里）。
            let screen = ui.wait_for("reconnecting · 30s").await;
            for text in [
                "1 of 2 connected",
                "     ssh dev-a  ·  executor 0.1.58  ·  env not refreshed (older executor)",
            ] {
                assert!(screen.contains(text), "{text}\n{screen}");
            }
            assert!(
                line_with(&screen, "○ gpu-1").ends_with("reconnecting · 30s"),
                "{screen}"
            );
            // 最近一次的原因在选中那台的下面（F98，remote-exec.md §25.4.4：ssh 的这一种写成人话）。
            ui.key(KeyCode::Down);
            ui.wait_for("     Last error: ssh can't reach gpu-1:22 (Connection refused)")
                .await;
        },
    )
    .await;
    let mut old = row("dev-a", MachineStatus::Ready);
    if let Some(executor) = &mut old.executor {
        executor.refreshes_env = false;
    }
    with_ui(
        idle(),
        setup_with(
            Some(FakeMachines::with(vec![old, waiting("gpu-1", 30_400)])),
            "zh_CN.UTF-8",
        ),
        |mut ui| async move {
            ready(&mut ui).await;
            ui.submit("/machines");
            let screen = ui.wait_for("重连中 · 30s").await;
            for text in [
                "2 台里 1 台已连接",
                "     ssh dev-a  ·  执行器 0.1.58  ·  不能刷新环境（执行器较旧）",
            ] {
                assert!(screen.contains(text), "{text}\n{screen}");
            }
            ui.key(KeyCode::Down);
            ui.wait_for("     最近一次的错误：ssh 连不上 gpu-1:22（Connection refused）")
                .await;
        },
    )
    .await;
}
