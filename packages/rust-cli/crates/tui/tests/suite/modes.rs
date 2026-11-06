//! 权限模式与 plan 开关（修复线 F20，docs/specs/rust-cli/shell-permissions.md §4.6、tui.md §5.2 / §7）：
//! shift+tab 循环 build → guarded → yolo → build，不在循环里的模式回到 build；`/plan` 只开关 plan，状态栏同时显示
//! 两者（S2 起照 Puffin：`Plan · <模式> ⇧⇥`，tui-kit.md §4.2「状态栏」）；批准计划后回到进入前的权限模式；guarded 下危险命令询问一次（只有 Yes / No）。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, environments, text_response, tool_use_response,
};
use serde_json::json;

use super::support::{GatedModel, Setup, request_text, with_ui};

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc stop")
}

/// 能给出路径事实的执行环境（ExitPlanMode 要找计划文件）。
fn env() -> Arc<MemoryEnvironment> {
    Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    }))
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_tab_cycles_build_guarded_and_yolo() {
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    with_ui(model, Setup::default(), |mut ui| async move {
        // 没有会话（草稿）时循环。
        ui.wait_for("Ask before changes ⇧⇥").await;
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.wait_for("Autonomous ⇧⇥").await;
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        // 工作线 F87：进完全访问先确认（yolo_confirm.rs）。
        ui.confirm_full_access().await;
        ui.wait_for("Full access ⇧⇥").await;
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        let screen = ui.wait_for("Ask before changes ⇧⇥").await;
        assert!(!screen.contains("Plan mode"), "{screen}");
        // 有会话时同样循环（以 `ModeChanged` 为准）。
        ui.submit("hi");
        ui.wait("the reply", |s| idle(s, "   hello")).await;
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.wait_for("Autonomous ⇧⇥").await;
        // edit 不在循环里、工作线 F76 起 `/mode` 也不再能选（shift+tab 从 edit 回到 build 见 mode_picker.rs）。
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_is_a_switch_next_to_the_mode() {
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    let setup = Setup {
        lang: "zh_CN.UTF-8",
        ..Setup::default()
    };
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes ⇧⇥").await;
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.confirm_full_access().await;
        ui.wait_for("Full access ⇧⇥").await;
        // `/plan` 打开 plan：footer 同时显示 plan 与权限模式。
        ui.submit("/plan");
        let screen = ui.wait_for("Plan · Full access ⇧⇥").await;
        // 用户决定（2026-10-20）：权限模式后面不再有切换提示。
        assert!(!screen.contains("shift+tab"), "{screen}");
        ui.submit("hi");
        ui.wait("the reply", |s| idle(s, "   hello")).await;
        // shift+tab 改的是权限模式，plan 不变。
        // 80 列放不下时 `⇧⇥`（名次最低）先走（Puffin 状态栏的 `fit`，tui-kit.md §4.2）。
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.wait_for("Plan · Ask before changes").await;
        // 再一次 `/plan` 关掉，权限模式留着。
        ui.submit("/plan");
        // 关掉时状态栏不再写 `Plan · `，输入框上方说模式的一句（照 Puffin `mode_line`）。
        let screen = ui
            .wait("plan off", |s| {
                s.contains("Ask before changes ⇧⇥") && !s.contains("Plan ·")
            })
            .await;
        assert!(screen.contains("Plan mode is off."), "{screen}");
    })
    .await;
    // 中文的 footer。
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("变更前确认").await;
        ui.submit("/plan");
        ui.wait_for("计划 · 变更前确认 ⇧⇥").await;
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.wait_for("计划 · 自主模式 ⇧⇥").await;
    })
    .await;
}

/// EnterPlanMode 打开 plan；批准计划只关掉 plan，回到进入前的 guarded（两种批准各换一个模式的做法 F20 不提供）。
#[tokio::test(flavor = "multi_thread")]
async fn approving_the_plan_returns_to_the_mode_from_before() {
    let model = GatedModel::scripted(vec![
        tool_use_response("toolu_1", "EnterPlanMode", &json!({})),
        tool_use_response("toolu_2", "ExitPlanMode", &json!({})),
        text_response(&["implementing"]),
    ]);
    // 对话框连同计划与选项要放得下。
    let setup = Setup {
        environments: environments(env()),
        height: 48,
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes ⇧⇥").await;
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.wait_for("Autonomous ⇧⇥").await;
        ui.submit("plan first");
        let ask = ui.wait_for("Start on this plan?").await;
        // 回答照 Puffin 的计划请求（tui-kit.md §4.2「计划」）：批准写它在哪个模式里跑。
        assert!(
            ask.contains("1  Approve         runs in Autonomous  ⇧⇥ changes it"),
            "{ask}"
        );
        assert!(ask.contains("2  Keep planning"), "{ask}");
        ui.settle().await;
        ui.key(KeyCode::Enter);
        let screen = ui.wait("the reply", |s| idle(s, "   implementing")).await;
        assert!(screen.contains("Autonomous ⇧⇥"), "{screen}");
        assert!(!screen.contains("Plan ·"), "{screen}");
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    // EnterPlanMode 之后的请求在 plan 里（完整提醒），批准之后退出（结果文字）。
    assert!(request_text(&requests[1]).contains("Plan mode is active"));
    assert!(request_text(&requests[2]).contains("User has approved exiting plan mode"));
}

/// guarded：危险命令询问一次，没有“不再询问”（照 Puffin 的回答：允许一次、完全访问、拒绝，板头标 risky）；其余命令同
/// yolo 不询问。
#[tokio::test(flavor = "multi_thread")]
async fn guarded_asks_once_for_dangerous_commands() {
    let model = GatedModel::scripted(vec![
        tool_use_response("toolu_1", "Bash", &json!({"command": "touch made.txt"})),
        tool_use_response("toolu_2", "Bash", &json!({"command": "rm -rf build"})),
        text_response(&["cleaned"]),
    ]);
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes ⇧⇥").await;
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.wait_for("Autonomous ⇧⇥").await;
        ui.submit("clean up");
        let ask = ui.wait_for("Run this command?  risky").await;
        assert!(ask.contains("rm -rf build"), "{ask}");
        // 回答行照 Puffin 的 `N  标签`（tui-kit.md §4.2「请求」）。
        assert!(ask.contains("1  Allow once"), "{ask}");
        assert!(ask.contains("3  Deny"), "{ask}");
        assert!(!ask.contains("Always allow"), "{ask}");
        ui.settle().await;
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| idle(s, "   cleaned")).await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    // touch 没有询问（同 yolo，执行环境替身给出执行错误而不是拒绝）；rm 批准后执行。
    for request in &requests[1..] {
        let text = request_text(request);
        assert!(!text.contains("doesn't want to proceed"), "{text}");
    }
}
