//! `/context`（docs/specs/rust-cli/context-usage.md §5.2、tui-panels.md §4）：内核算出用量后打开独立页面：标题、
//! 方格图、类别说明、折叠的明细；`/context all` 展开；esc 关闭。替身模型没有 count_tokens，各部分按本地估算。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, text_response};

use super::support::{GatedModel, Setup, with_ui};

/// 有两个 skill 的工作区（明细里有 Skills 一段）。
fn with_skills() -> Setup {
    let skill = |what: &str| format!("---\ndescription: {what}\n---\nDo {what}.\n");
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    })
    .with_file(
        "/Users/dev/project/.zcode/skills/review/SKILL.md",
        &skill("review"),
    )
    .with_file(
        "/Users/dev/project/.zcode/skills/deploy/SKILL.md",
        &skill("deploy"),
    );
    Setup {
        width: 100,
        height: 50,
        env: Arc::new(env),
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn context_opens_a_page_with_the_grid_and_the_categories() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    with_ui(model, with_skills(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/context");
        let screen = ui.wait_for("Estimated usage by category").await;
        // 独立页面：第一行是标题，最后一行是页面的提示；不进 transcript。
        assert_eq!(screen.lines().next(), Some("  Context Usage"), "{screen}");
        assert!(screen.contains("↑/↓ scroll · esc close"), "{screen}");
        assert!(screen.contains("\u{26C1} System prompt: "), "{screen}");
        assert!(screen.contains("\u{26F6} Free space: "), "{screen}");
        // 目录给 200k 的窗口：10 × 10 的方格，空闲格是 ⛶。
        assert!(screen.contains("/200k tokens ("), "{screen}");
        assert!(screen.contains("\u{26F6} \u{26F6} \u{26F6}"), "{screen}");
        // 明细默认折叠。
        assert!(screen.contains("Skills · /skills"), "{screen}");
        // 两个工作区 skill 加内置的 `loop`。
        assert!(screen.contains("└ 3 skills · "), "{screen}");
        assert!(screen.contains("/context all to expand"), "{screen}");
        ui.key(KeyCode::Esc);
        let closed = ui
            .wait("the page to close", |s| !s.contains("Estimated usage"))
            .await;
        assert!(!closed.contains("Context Usage"), "{closed}");
        // `/context all`：每个 skill 一行。
        ui.submit("/context all");
        let screen = ui.wait_for("Estimated usage by category").await;
        assert!(screen.contains("review (Project): "), "{screen}");
        assert!(screen.contains("deploy (Project): "), "{screen}");
        assert!(!screen.contains("to expand"), "{screen}");
        ui.key(KeyCode::Char('q'));
        ui.wait("the page to close", |s| !s.contains("Estimated usage"))
            .await;
        // 不建会话：`/status` 仍是“还没有开始”。
        ui.submit("/status");
        let status = ui.wait_for("Session").await;
        assert!(status.contains("not started"), "{status}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn context_headings_follow_the_language() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let setup = Setup {
        width: 100,
        height: 40,
        lang: "zh_CN.UTF-8",
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("/context");
        let screen = ui.wait_for("上下文用量").await;
        assert!(screen.contains("按类别估算"), "{screen}");
        assert!(screen.contains("↑/↓ 滚动 · esc 关闭"), "{screen}");
    })
    .await;
}
