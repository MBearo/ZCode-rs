//! Session 标签（tui-panels.md §4.1、§4.5）：`/status` 直接打开它，卡上 ⇥ 打开它，页上 ⇥ 换标签且各自的状态留着；行按
//! 卡的排法，事实到了才有 MCP、账号、分支与 hooks；⏎ 复制会话 ID；查看子代理时写子代理，Context 不能去。
use std::time::Instant;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::manage::{HookEntry, McpServerInfo, McpServerState};
use rust_cli_kernel_api::panels::{SubagentEntry, SubagentStatus};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::StyledLine;

use super::*;
use crate::machines::{ExecutorInfo, MachineRow, MachineStatus};
use crate::services::{Family, FamilyStatus, MemoryFile, PlanConnection};
use crate::ui::agents::{AgentList, Viewing};
use crate::ui::context_tree::tests::sample;
use crate::ui::stats_page::StatsPage;
use crate::ui::test_support::{app, key};
use crate::view::Transcript;

const ID: &str = "7b2c91e4-1f0a-4c55-9a8e-3d1f2a6b8c21";

fn started() -> App {
    let mut app = app();
    app.session = Some(ID.into());
    app
}

/// 会话的标题经内核的事件折进视图（同真实的会话）。
fn titled(app: &mut App) {
    app.view.apply(
        SequencedEvent {
            seq: 1,
            event: KernelEvent::TitleChanged {
                title: "Fix the footer spacing".into(),
            },
            revision: 0,
        },
        0,
    );
}

fn server(name: &str, state: McpServerState, tools: usize, reason: Option<&str>) -> McpServerInfo {
    McpServerInfo {
        name: name.into(),
        state,
        reason: reason.map(str::to_owned),
        tools,
        machine: Default::default(),
    }
}

fn facts() -> SessionFacts {
    SessionFacts {
        mcp: vec![
            server("github", McpServerState::Connected, 24, None),
            server(
                "jira",
                McpServerState::Failed,
                0,
                Some("401 Unauthorized: the token has expired"),
            ),
            server("slack", McpServerState::Disabled, 0, None),
        ],
        hooks: vec![HookEntry {
            event: "PreToolUse".into(),
            matcher: None,
            kind: "command".into(),
            display: "./scripts/check.sh".into(),
            source: "user".into(),
        }],
        providers: Vec::new(),
        families: vec![FamilyStatus {
            family: Family::Zai,
            name: "Z.ai".into(),
            signed_in: true,
            account: "dev@example.com".into(),
            connection: PlanConnection::Individual,
            teams: Vec::new(),
            teams_error: String::new(),
            provider: "fixture".into(),
            models: Vec::new(),
        }],
        branch: Some("main".into()),
    }
}

fn text(app: &App, width: usize) -> Vec<String> {
    app.session_lines(width, 0)
        .iter()
        .map(|l| StyledLine::plain(l).trim_end().to_owned())
        .collect()
}

/// 槽（经 `page_frame::body`：刚复制了时它换上那一句）。
fn slot(app: &App) -> String {
    crate::ui::page_frame::body(app, 100, Instant::now(), 0)
        .and_then(|b| b.slot)
        .unwrap_or_default()
        .iter()
        .map(|s| s.text.as_ref())
        .collect()
}

fn stats_page(app: &App) -> StatsPage {
    match &app.overlay {
        Some(Overlay::Card(Card::StatsPage(page))) => page.clone(),
        other => panic!("not the page: {other:?}"),
    }
}

/// `/status` 打开卡与 Session 标签，`/stats all` 打开 Context，`/stats` 只开卡；esc 都回到卡。
#[test]
fn status_opens_the_session_tab_and_esc_goes_back_to_the_card() {
    let mut app = started();
    assert_eq!(app.slash("/status"), Some(Action::CountContext));
    assert_eq!(app.stats_tab(), Some(Tab::Session));
    assert!(app.session_facts_wanted());
    app.card_key(key(KeyCode::Esc));
    assert!(matches!(app.overlay, Some(Overlay::Card(Card::Stats(_)))));
    assert!(!app.session_facts_wanted());
    let _ = app.slash("/stats all");
    assert_eq!(app.stats_tab(), Some(Tab::Context));
    assert!(!app.session_facts_wanted(), "only the Session tab asks");
    app.overlay = None;
    let _ = app.slash("/stats");
    assert!(matches!(app.overlay, Some(Overlay::Card(Card::Stats(_)))));
}

/// 卡的槽是 `⏎ context  ⇥ session`；卡上 ⇥ 打开 Session 并要问它的事实；页上 ⇥ 换标签，Context 的开合留着。
#[test]
fn tab_moves_between_the_tabs_and_keeps_each_ones_state() {
    let mut app = started();
    let _ = app.open_stats();
    let card_slot: String = app
        .card_layer(100, 0, 0)
        .expect("the card")
        .1
        .iter()
        .map(|s| s.text.as_ref())
        .collect();
    assert!(card_slot.contains("context"), "{card_slot}");
    assert!(card_slot.contains("session"), "{card_slot}");
    assert_eq!(
        app.card_key(key(KeyCode::Tab)),
        Some(Some(Action::SessionFacts))
    );
    assert_eq!(app.stats_tab(), Some(Tab::Session));
    assert!(slot(&app).contains("copy id"), "{}", slot(&app));
    assert!(slot(&app).contains("context"), "{}", slot(&app));
    // 去 Context，打开一行，再来回一次：开着的仍开着。
    app.context_counted(Some(SessionId::new(ID)), Ok(sample()), Instant::now(), 0);
    assert_eq!(app.card_key(key(KeyCode::Tab)), Some(None));
    assert_eq!(app.stats_tab(), Some(Tab::Context));
    assert!(slot(&app).contains("session"), "{}", slot(&app));
    app.card_key(key(KeyCode::Right));
    assert_eq!(stats_page(&app).context.open.len(), 1);
    assert_eq!(
        app.card_key(key(KeyCode::Tab)),
        Some(Some(Action::SessionFacts))
    );
    app.card_key(key(KeyCode::Tab));
    assert_eq!(stats_page(&app).context.open.len(), 1);
}

/// 面包屑的右端是两个标签；行按 §4.5：标题、完整的 ID、模型与窗口、账号、模式、目录与分支、版本与新版本、MCP、规则文件、hooks。
#[test]
fn the_session_tab_lists_the_facts() {
    let mut app = started();
    titled(&mut app);
    app.newer_release = Some("0.1.85".into());
    app.memory_files = vec![MemoryFile {
        label: "User memory".into(),
        display: "~/.zcode/AGENTS.md".into(),
        path: "/Users/dev/.zcode/AGENTS.md".into(),
    }];
    let _ = app.open_stats_page(Tab::Session);
    let before = text(&app, 100);
    assert!(
        before.iter().any(|l| l == "   Checking the servers…"),
        "{before:#?}"
    );
    assert!(!before.iter().any(|l| l.starts_with("   Account")));
    app.context_counted(Some(SessionId::new(ID)), Ok(sample()), Instant::now(), 0);
    app.session_facts_arrived(facts());
    let lines = text(&app, 100);
    let has = |want: &str| {
        assert!(lines.iter().any(|l| l == want), "{want:?} in {lines:#?}");
    };
    assert!(lines[1].starts_with("   Stats  ›  Session"), "{lines:#?}");
    assert!(lines[1].ends_with("Context   Session"), "{lines:#?}");
    has("   Session    Fix the footer spacing");
    has(&format!("              {ID}"));
    has("   Model      scripted   Z.ai");
    has("              200k window, the model's own  ·  compacts at 160k");
    has("   Account    dev@example.com  ·  Individual Plan");
    has("   Mode       Ask before changes");
    has("   Version    zcode 0.1.0  ·  0.1.85 is out, run zcode update");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("   Directory  ") && l.ends_with("  ·  main")),
        "{lines:#?}"
    );
    let mcp = lines
        .iter()
        .find(|l| l.starts_with("   MCP servers"))
        .expect("the MCP heading");
    assert!(mcp.ends_with("1 connected  ·  1 failed"), "{mcp}");
    has("   ✓ github        24 tools");
    assert!(
        lines.iter().any(|l| l
            .starts_with("   ✗ jira          401 Unauthorized: the token has expired")
            && l.ends_with("/mcp")),
        "{lines:#?}"
    );
    has("   – slack         turned off");
    has("     ~/.zcode/AGENTS.md   User memory");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("     PreToolUse  ./scripts/check.sh") && l.ends_with("user")),
        "{lines:#?}"
    );
}

/// 没有会话时说还没开始；窄时 ID 只写头尾、没有新版本的提示与行尾的指向；没有 MCP 服务器时说没有。
#[test]
fn before_the_first_message_and_at_narrow_widths() {
    let mut app = app();
    app.newer_release = Some("0.1.85".into());
    app.session_facts_arrived(SessionFacts::default());
    let lines = text(&app, 100);
    assert!(
        lines
            .iter()
            .any(|l| l == "   Session    not started  ·  your first message starts one"),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|l| l == "   No MCP servers"), "{lines:#?}");
    let mut app = started();
    app.newer_release = Some("0.1.85".into());
    app.session_facts_arrived(facts());
    let lines = text(&app, 64);
    assert!(
        lines.iter().any(|l| l == "              7b2c91e4…6b8c21"),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|l| l == "   Version    zcode 0.1.0"));
    assert!(!lines.iter().any(|l| l.ends_with("/mcp")), "{lines:#?}");
}

/// 机器：就绪的写往返与系统，没连的写 not connected，标题右端是各自的数。
#[test]
fn machines_are_listed_when_there_are_any() {
    let mut app = started();
    app.machines_enabled = true;
    app.machines = vec![
        MachineRow {
            name: "devbox".into(),
            ssh: "dev@devbox".into(),
            working_directory: None,
            status: MachineStatus::Ready,
            latency_ms: Some(42),
            executor: Some(ExecutorInfo {
                os: "linux".into(),
                arch: "arm64".into(),
                version: "0.1.0".into(),
                refreshes_env: false,
                home: None,
            }),
        },
        MachineRow {
            name: "gpu".into(),
            ssh: "gpu".into(),
            working_directory: None,
            status: MachineStatus::Disconnected,
            latency_ms: None,
            executor: None,
        },
    ];
    let lines = text(&app, 100);
    let heading = lines
        .iter()
        .find(|l| l.starts_with("   Machines"))
        .expect("the machines heading");
    assert!(
        heading.ends_with("1 ready  ·  1 not connected"),
        "{heading}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "   ✓ devbox        ready  ·  42 ms  ·  linux arm64"),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "   – gpu           not connected")
    );
}

/// ⏎ 复制完整的会话 ID，槽里说一句到下一个键；没有会话时 ⏎ 什么也不做、槽里没有它。
#[test]
fn enter_copies_the_session_id() {
    let mut main = started();
    let _ = main.open_stats_page(Tab::Session);
    assert_eq!(
        main.card_key(key(KeyCode::Enter)),
        Some(Some(Action::Copy(ID.into())))
    );
    assert_eq!(slot(&main), "Copied the session ID");
    let mut fresh = app();
    let _ = fresh.open_stats_page(Tab::Session);
    assert_eq!(fresh.card_key(key(KeyCode::Enter)), Some(None));
    assert!(!slot(&fresh).contains("copy id"), "{}", slot(&fresh));
}

/// 查看子代理时：写子代理的会话，Context 是 line 色、⇥ 不去它；不问事实（它们是主会话的）。
#[test]
fn while_viewing_a_subagent_the_tab_shows_the_subagent() {
    let mut app = started();
    titled(&mut app);
    app.snapshots.agents = AgentList {
        entries: vec![SubagentEntry {
            agent: AgentId::new("agent_a"),
            child: SessionId::new("sess_child"),
            agent_type: "Explore".into(),
            description: "Look around".into(),
            status: SubagentStatus::Running,
            background: true,
            waiting: false,
            elapsed_ms: 0,
            ended_ago_ms: None,
            version: 0,
            owner: None,
            activity: None,
            parked: false,
            machine: Default::default(),
        }],
        at: Some(0),
        shown: 0,
    };
    app.viewing = Some(Viewing {
        agent: AgentId::new("agent_a"),
        kind: "Explore".into(),
        description: "Look around".into(),
        version: 0,
        status: SubagentStatus::Running,
        messages: Vec::new(),
        owner: None,
        view: Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
    });
    // 卡上 ⏎ 不开 Context；⇥ 开 Session。
    let _ = app.open_stats();
    assert_eq!(app.card_key(key(KeyCode::Enter)), Some(None));
    assert!(matches!(app.overlay, Some(Overlay::Card(Card::Stats(_)))));
    app.card_key(key(KeyCode::Tab));
    assert_eq!(app.stats_tab(), Some(Tab::Session));
    assert!(!app.session_facts_wanted());
    assert_eq!(app.card_key(key(KeyCode::Tab)), Some(None));
    assert_eq!(app.stats_tab(), Some(Tab::Session));
    let lines = text(&app, 100);
    assert!(
        lines
            .iter()
            .any(|l| l == "   Subagent   Explore  ·  running"),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|l| l == "              sess_child"));
    assert!(
        lines
            .iter()
            .any(|l| l == "              started by Fix the footer spacing")
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "   Context is counted for the main session only.")
    );
    assert!(!lines.iter().any(|l| l.starts_with("   Directory")));
    assert_eq!(
        app.card_key(key(KeyCode::Enter)),
        Some(Some(Action::Copy("sess_child".into())))
    );
    assert!(!slot(&app).contains("context"), "{}", slot(&app));
}

/// 自己加的 provider 写目录里的名字，不写它的 UUID；长的服务器名字把第二列推后（至多行宽的一半）；规则文件的种类对齐。
#[test]
fn custom_providers_and_long_names_read_well() {
    let mut app = started();
    app.memory_files = vec![
        MemoryFile {
            label: "User memory".into(),
            display: "~/.zcode/AGENTS.md".into(),
            path: "/Users/dev/.zcode/AGENTS.md".into(),
        },
        MemoryFile {
            label: "Project memory".into(),
            display: "./AGENTS.md".into(),
            path: "/Users/dev/project/AGENTS.md".into(),
        },
    ];
    app.session_facts_arrived(SessionFacts {
        mcp: vec![
            server("plugin:zotero:zotero", McpServerState::Connected, 20, None),
            server("docs", McpServerState::Connected, 3, None),
        ],
        providers: vec![("fixture".into(), "My Gateway".into())],
        ..SessionFacts::default()
    });
    let lines = text(&app, 100);
    let has = |want: &str| {
        assert!(lines.iter().any(|l| l == want), "{want:?} in {lines:#?}");
    };
    has("   Model      scripted   My Gateway");
    has("   ✓ plugin:zotero:zotero  20 tools");
    has("   ✓ docs                   3 tools");
    has("     ~/.zcode/AGENTS.md   User memory");
    has("     ./AGENTS.md          Project memory");
    // 没有名字时不写 provider。
    app.session_facts_arrived(SessionFacts::default());
    assert!(
        text(&app, 100)
            .iter()
            .any(|l| l == "   Model      scripted"),
        "{:#?}",
        text(&app, 100)
    );
}
