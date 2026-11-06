//! 上下文的页（docs/specs/rust-cli/tui-panels.md §4；设计稿 D）：`/context`（`/stats` 的别名）打开卡，卡的组成来自内核的
//! 计数、槽是 `⏎ context`；⏎ 打开代替正文的页：面包屑、方格图与图例、一句话、大纲（逐行打开），esc 回卡；`/stats all`
//! 直接打开页；打字关上卡与页。各部分按本地估算（工作线 F93 起不发 count_tokens）。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, environments, text_response};

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
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
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
        environments: environments(Arc::new(env)),
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_card_opens_the_context_page_and_its_outline() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    with_ui(model, with_skills(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // `/context` 是 `/stats` 的卡：计数到了之后卡有组成的图例，槽是 `⏎ context`。
        ui.submit("/context");
        let card = ui
            .wait("the card's legend", |s| {
                s.contains("   Stats") && s.contains("■ skills")
            })
            .await;
        assert!(card.contains("⏎ context"), "{card}");
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Stats  ›  Context").await;
        // 页代替正文，输入框与状态栏留着。
        assert!(screen.contains("Ask before changes"), "{screen}");
        assert!(screen.contains(" of 200k   "), "{screen}");
        assert!(screen.contains("comes with every request"), "{screen}");
        assert!(screen.contains("■ Skills"), "{screen}");
        assert!(screen.contains("⏎ expand"), "{screen}");
        // 打开 Skills：工作区的两个 skill 与内置的三个，note 是来源的小写。
        ui.key(KeyCode::End);
        let open = ui.wait("Skills chosen", |s| s.contains("▸ ■ Skills")).await;
        assert!(open.contains("5 skills"), "{open}");
        ui.key(KeyCode::Right);
        let open = ui.wait("the skills", |s| s.contains("▾ ■ Skills")).await;
        let review = open
            .lines()
            .find(|l| l.trim_start().starts_with("review "))
            .unwrap_or_else(|| panic!("{open}"));
        assert!(review.contains("project"), "{review}");
        assert!(open.contains("⏎ collapse"), "{open}");
        // esc 回到卡，再 esc 关上。
        ui.key(KeyCode::Esc);
        ui.wait("back to the card", |s| {
            s.contains("   Stats") && !s.contains("Stats  ›  Context")
        })
        .await;
        ui.key(KeyCode::Esc);
        ui.wait("the card to close", |s| !s.contains("   Stats"))
            .await;
        // `/stats all` 直接打开页；打字关上卡与页，字进草稿。
        ui.submit("/stats all");
        ui.wait_for("Stats  ›  Context").await;
        ui.type_text("h");
        ui.wait("the page to close", |s| {
            !s.contains("Stats  ›  Context") && !s.contains("   Stats")
        })
        .await;
    })
    .await;
}

/// Session 标签（tui-panels.md §4.5）：`/status` 打开卡与 Session，行来自真实的内核与界面（会话 ID、回合、目录、版本、MCP
/// 的事实）；⇥ 换到 Context 再换回来，esc 回到卡。
#[tokio::test(flavor = "multi_thread")]
async fn status_opens_the_session_tab() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    with_ui(model, with_skills(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 还没有会话：Session 说还没开始，槽里没有 ⏎。
        ui.submit("/status");
        let fresh = ui.wait_for("Stats  ›  Session").await;
        assert!(fresh.contains("not started"), "{fresh}");
        assert!(fresh.contains("⇥ context"), "{fresh}");
        assert!(!fresh.contains("copy id"), "{fresh}");
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Esc);
        ui.wait("the card to close", |s| !s.contains("   Stats"))
            .await;
        ui.submit("hello");
        ui.wait_for("ok").await;
        ui.submit("/status");
        let screen = ui
            .wait("the session's facts", |s| {
                s.contains("Stats  ›  Session") && s.contains("No MCP servers")
            })
            .await;
        assert!(screen.contains("1 turn"), "{screen}");
        assert!(screen.contains("Directory  ~/project"), "{screen}");
        assert!(screen.contains("Version    zcode "), "{screen}");
        assert!(screen.contains("Mode       Ask before changes"), "{screen}");
        assert!(screen.contains("⏎ copy id  ⇥ context"), "{screen}");
        ui.key(KeyCode::Tab);
        ui.wait_for("Stats  ›  Context").await;
        ui.key(KeyCode::Tab);
        ui.wait_for("Stats  ›  Session").await;
        ui.key(KeyCode::Esc);
        ui.wait("back to the card", |s| {
            s.contains("   Stats") && !s.contains("Stats  ›  Session")
        })
        .await;
    })
    .await;
}
