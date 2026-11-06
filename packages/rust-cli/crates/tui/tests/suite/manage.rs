//! 管理命令与小项（docs/specs/rust-cli/tui-display.md §3、§4）：都在本地或经内核会话 API 完成，不发给模型
//! （`/plan 描述` 除外）。
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, request_text, with_ui};

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc stop")
}

/// `/agents` 已按用户决定删除（tui-display.md §3.2，L33）：按未知命令提示，输入保留，不发给模型。
#[tokio::test(flavor = "multi_thread")]
async fn agents_is_an_unknown_command() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/agents");
        let screen = ui.wait_for("Unrecognized command '/agents'").await;
        assert!(screen.contains("› /agents"), "{screen}");
        assert!(!screen.contains("wizard has been removed"), "{screen}");
    })
    .await;
    assert!(model.requests().is_empty(), "no command reached the model");
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_mcp_and_hooks_open_their_settings_pages() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // `/skills`、`/mcp`、`/hooks` 打开设置里自己的那一页（Puffin）；以前各是一页说明（`No skills found` 等）。
        ui.submit("/skills");
        let page = ui.wait_for("Settings  ›  Skills").await;
        assert!(page.contains("+ New skill"), "{page}");
        // `+ New skill` 是最后一行：↑ 绕到它，⏎ 关上设置、草稿是 `$skill-creator `（同 Puffin，`$` 面板落地之后）。
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        ui.wait("the page to close", |s| !s.contains("Settings  ›"))
            .await;
        ui.wait_for("› $skill-creator").await;
        ui.clear();
        ui.submit("/mcp");
        ui.wait_for("Settings  ›  MCP").await;
        ui.key(KeyCode::Esc);
        ui.wait("the page to close", |s| !s.contains("Settings  ›"))
            .await;
        ui.submit("/hooks");
        ui.wait_for("Settings  ›  Hooks").await;
        ui.key(KeyCode::Esc);
        ui.wait("the page to close", |s| !s.contains("Settings  ›"))
            .await;
        // `/tasks` 打开任务卡（tui-tasks.md §3.1）；会话里没有任务时卡的位置说一句。
        ui.submit("/tasks");
        ui.wait_for("No tasks in this session.").await;
    })
    .await;
    assert!(model.requests().is_empty(), "no command reached the model");
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_and_effort_follow_node() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/mode nope");
        ui.wait_for("Unsupported mode: nope.").await;
        // 工作线 F76：照 App 只留三个模式，edit 不再收。
        ui.submit("/mode edit");
        ui.wait_for("Unsupported mode: edit. Available modes: build, guarded, yolo.")
            .await;
        // 修复线 F20：guarded 是真正的 guarded（App 的名称 Autonomous），以前当 Full access。
        ui.submit("/mode guarded");
        ui.wait_for("Autonomous ⇧⇥").await;
        // F26 起不带参数是选择器：当前模式后面 `current`（Puffin 的 `/mode` 层没有头一行，tui-kit.md §4.0「板」）。
        ui.submit("/mode");
        let list = ui
            .wait_for("Asks only before commands that can destroy work")
            .await;
        // 80 列放不下整个 `current`，照 Puffin 截成 `cur…`（同一个 kit 的行）。
        assert!(theme_row(&list, "Autonomous").contains("cur"), "{list}");
        // esc 关上、草稿留着 `/mode `：清掉再打下一条。
        ui.key(KeyCode::Esc);
        ui.clear();
        // 工作线 F87：切到完全访问先确认（yolo_confirm.rs）。
        ui.submit("/mode yolo");
        ui.confirm_full_access().await;
        ui.wait_for("Full access ⇧⇥").await;
        // `/effort` 是 `/model` 的别名（Puffin）：打开模型选择器，档位在那里用 ←→ 改。
        ui.submit("/effort");
        ui.wait_for("other-model").await;
        ui.key(KeyCode::Esc);
        ui.clear();
        // 带词也只打开选择器（用户决定 2026-10-31，tui-display.md §4.4），草稿是 `/model `。
        ui.submit("/variant high");
        let list = ui.wait_for("other-model").await;
        assert!(list.contains("› /model"), "{list}");
        assert!(!list.contains("/variant"), "{list}");
        ui.key(KeyCode::Esc);
        ui.clear();
    })
    .await;
}

/// 修复线 F26（用户决定）：`/mode` 不带参数时弹出与 `/model` 同一种列表（上下键选、enter 确认、esc 取消、当前值
/// 后面 `current`，tui-kit.md §4.2），选中后同 `/mode <名>`；带参数照旧直接生效（上一个测试）。
#[tokio::test(flavor = "multi_thread")]
async fn mode_without_arguments_opens_a_picker() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/mode");
        // 没有头一行（Puffin 的 `/mode` 层）；开着时草稿是 `/mode `（tui-kit.md §4.0「板」）。
        let list = ui
            .wait_for("Asks only before commands that can destroy work")
            .await;
        assert!(list.contains("› /mode"), "{list}");
        // 工作线 F76：App 的三个模式（名称与说明）。
        for name in ["Ask before changes", "Autonomous", "Full access"] {
            assert!(list.contains(name), "{list}");
        }
        assert!(
            theme_row(&list, "Ask before changes").contains("current"),
            "the current mode is marked: {list}"
        );
        // esc 取消：列表关闭，模式不变，草稿留着 `/mode `；⏎ 再打开（Puffin）。
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("the picker to close", |s| {
                !s.contains("Asks only before commands that can destroy work")
            })
            .await;
        assert!(!screen.contains("Autonomous:"), "{screen}");
        assert!(screen.contains("› /mode"), "{screen}");
        // ↓ 两次（Ask before changes → Autonomous → Full access），enter 确认。
        ui.key(KeyCode::Enter);
        ui.wait_for("Asks only before commands that can destroy work")
            .await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.confirm_full_access().await;
        // 确认之后不另说一句（Puffin）：footer 的模式（最后一行）跟着变。
        let screen = ui.wait_for("Full access ⇧⇥").await;
        let footer = screen
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        assert!(footer.contains("Full access"), "{screen}");
        // 再打开时当前值跟着变。
        ui.submit("/mode");
        let list = ui
            .wait("the picker with full access current", |s| {
                s.contains("Asks only before commands that can destroy work")
                    && theme_row(s, "Full access").contains("current")
            })
            .await;
        assert!(list.contains("› /mode"), "{list}");
        ui.key(KeyCode::Esc);
    })
    .await;
    assert!(model.requests().is_empty(), "no command reached the model");
}

/// 有档位的模型目录。
struct LeveledDirectory;

#[async_trait::async_trait]
impl rust_cli_kernel_api::provider::ModelDirectory for LeveledDirectory {
    async fn catalog(&self) -> rust_cli_kernel_api::provider::ModelCatalog {
        rust_cli_kernel_api::provider::ModelCatalog {
            revision: 1,
            models: vec![rust_cli_kernel_api::provider::CatalogModel {
                capabilities: Default::default(),
                provider: "fixture".into(),
                provider_name: "Fixture".into(),
                model: "scripted".into(),
                protocol: rust_cli_kernel_api::provider::WireProtocol::AnthropicMessages,
                reasoning_levels: vec!["low".into(), "high".into()],
                context_window: 200_000,
                max_output_tokens: 32_000,
                connection: None,
            }],
            default: None,
        }
    }
}

/// `/effort` 是 `/model` 的别名（Puffin）：打开模型选择器，草稿是 `/model `，档位在选中模型的那一行用 ←→ 改。以前不带词时弹出
/// 自己的档位列表（修复线 F26），带词时直接设档位（用户决定 2026-10-31 删掉，tui-display.md §4.4）。
#[tokio::test(flavor = "multi_thread")]
async fn effort_only_opens_the_model_picker() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let setup = Setup {
        width: 120,
        models: Some(Arc::new(LeveledDirectory)),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/effort");
        let list = ui.wait_for("› /model").await;
        assert!(list.contains("scripted"), "{list}");
        assert!(!list.contains("Comprehensive implementation"), "{list}");
        ui.key(KeyCode::Esc);
        ui.clear();
        ui.wait("the draft cleared", |s| !s.contains("/model"))
            .await;
        // 带词也一样：词不生效；在用的模型没有档位，开在 `auto` 上，→ 到最低一档。
        ui.submit("/effort low");
        let list = ui.wait_for("auto").await;
        assert!(list.contains("› /model"), "{list}");
        ui.key(KeyCode::Right);
        ui.wait_for("low").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Set model to scripted").await;
        ui.submit("say ok");
        ui.wait("the reply", |s| idle(s, "   ok")).await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests[0].selection.reasoning.as_deref(), Some("low"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pickers_speak_chinese() {
    let model = GatedModel::scripted(Vec::new());
    let setup = Setup {
        width: 120,
        lang: "zh_CN.UTF-8",
        models: Some(Arc::new(LeveledDirectory)),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("变更前确认").await;
        ui.submit("/mode");
        let list = ui.wait_for("只在可能毁掉工作的命令前问").await;
        assert!(theme_row(&list, "变更前确认").contains("当前"), "{list}");
        ui.key(KeyCode::Esc);
        ui.wait("the picker to close", |s| {
            !s.contains("只在可能毁掉工作的命令前问")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_n_usage_and_exit_words() {
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["second"])]);
    let session = with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("one");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
        ui.submit("two");
        ui.wait("the second reply", |s| idle(s, "   second")).await;
        ui.submit("/copy 2");
        ui.wait_for("Copied").await;
        assert_eq!(
            ui.copies.lock().unwrap().last().map(String::as_str),
            Some("first")
        );
        ui.submit("/copy 3");
        ui.wait_for("Only 2 responses available").await;
        ui.submit("/copy x");
        ui.wait_for("Usage: /copy [N]").await;
        // `/cost` 是 `/usage` 的别名：用量的卡在板上（Puffin）；测试没有用量来源，卡说读不到。以前是一页时长与改动的统计。
        ui.submit("/cost");
        ui.wait_for("Could not read your usage").await;
        ui.key(KeyCode::Esc);
        ui.wait("the card to close", |s| {
            !s.contains("Could not read your usage")
        })
        .await;
        // 整行 `:q` 退出，不发给模型。
        ui.submit(":q");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    })
    .await;
    assert!(session.is_some());
    assert_eq!(model.requests().len(), 2, ":q was not sent to the model");
}

/// 存储里唯一的会话（内存存储不看筛选条件）。
async fn only_session(
    store: &rust_cli_kernel_api::testing::MemoryStore,
) -> Option<rust_cli_kernel_api::ids::SessionId> {
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::EnvironmentId;
    use rust_cli_kernel_api::store::{SessionFilter, SessionStore};
    let filter = SessionFilter {
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/project"),
        },
        limit: 10,
    };
    let mut ids = store.list(filter).await.unwrap().into_iter().map(|s| s.id);
    let id = ids.next();
    assert!(ids.next().is_none(), "one session");
    id
}

#[tokio::test(flavor = "multi_thread")]
async fn clear_with_a_name_renames_the_old_session() {
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    let setup = Setup::default();
    let store = setup.store.clone();
    let old = Arc::new(Mutex::new(None));
    let seen = old.clone();
    let facts_of = store.clone();
    let ids_of = store.clone();
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hi there");
        ui.wait("the reply", |s| idle(s, "   hello")).await;
        // 会话的 id 从存储取（以前读 `/status` 页的 `Session:` 一行；`/status` 现在是 `/stats` 的卡，Puffin 没有 id）。
        *seen.lock().unwrap() = only_session(&ids_of).await;
        ui.submit("/clear first try");
        ui.wait("the cleared screen", |s| !s.contains("hi there"))
            .await;
        ui.wait_for("Started a new session.").await;
        // 改名的回执不提示：等它落库后再结束（脚本结束时事件循环退出，在途的回执会被丢掉）。
        let id = seen.lock().unwrap().clone().expect("the session id");
        for _ in 0..200 {
            let renamed = facts_of
                .facts(&id)
                .iter()
                .any(|fact| matches!(fact, rust_cli_kernel_api::store::Fact::TitleChanged { .. }));
            if renamed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    let old = old.lock().unwrap().clone().expect("the session id");
    // 被离开的会话改名（tui-display.md §4.6）。
    let facts = store.facts(&old);
    assert!(
        facts.iter().any(|fact| matches!(
            fact,
            rust_cli_kernel_api::store::Fact::TitleChanged { title } if title == "first try"
        )),
        "{facts:#?}"
    );
}

/// 还没有会话时的 `/rename 名字`（titles.md §4）：改名是新会话草稿里的命令，马上提示，名字也是 `/rename ` 的 ghost；
/// `/clear` 丢掉它；连续两次是两条命令、两条记录。会话建好后先改名、再发第一条输入，首条输入不当标题。修复：以前提示
/// `/rename is not implemented yet.`。
#[tokio::test(flavor = "multi_thread")]
async fn rename_before_the_first_message_names_the_new_session() {
    use rust_cli_kernel_api::store::Fact;
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    let setup = Setup::default();
    let store = setup.store.clone();
    let session = with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/rename gone");
        ui.wait_for("Session renamed to: gone").await;
        ui.submit("/clear");
        ui.wait_for("Started a new session.").await;
        ui.submit("/rename first-name");
        ui.wait_for("Session renamed to: first-name").await;
        ui.submit("/rename my-draft");
        ui.wait_for("Session renamed to: my-draft").await;
        ui.type_text("/rename ");
        ui.wait("the ghost", |s| s.contains("› /rename my-draft"))
            .await;
        for _ in 0.."/rename ".len() {
            ui.key(KeyCode::Backspace);
        }
        ui.submit("hi there, first message");
        ui.wait("the reply", |s| idle(s, "   hello")).await;
    })
    .await;
    let id = rust_cli_kernel_api::ids::SessionId::new(session.expect("the session id"));
    let facts = store.facts(&id);
    let titles: Vec<&str> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::TitleChanged { title } => Some(title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(titles, ["first-name", "my-draft"], "{facts:#?}");
    // 改名排在第一条输入之前（actor 先记下 custom，首条输入不改标题）。
    // 记录与改名同一批，比的是改名与人类的第一条输入。
    let renamed = facts
        .iter()
        .rposition(|fact| matches!(fact, Fact::TitleChanged { .. }));
    let first = facts.iter().position(|fact| {
        matches!(
            fact,
            Fact::MessageAppended {
                message: rust_cli_kernel_api::message::Message::User(user),
            } if user.origin == rust_cli_kernel_api::message::UserOrigin::Human
        )
    });
    assert!(renamed.is_some() && renamed < first, "{facts:#?}");
    // 草稿里的记录照样随第一条输入发给模型。
    let request = request_text(&model.requests()[0]);
    assert!(
        request.contains("<command-args>first-name</command-args>")
            && request.contains("<command-args>my-draft</command-args>")
            && request.contains("The user named this session"),
        "{request}"
    );
    // `/clear` 换掉了草稿：之前的名字与它的记录都不在。
    assert!(!request.contains("Session renamed to: gone"), "{request}");
}

/// 草稿里的改名被拒绝（存储提交失败）：显示原因，记录随改名不在，第一条输入照常发出、照常起首条输入的标题（MR !3465
/// 评审 CR-01：以前“改了名”的记录预先放在草稿里，改名失败时照样发给模型）。
#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_draft_rename_sends_no_record_and_the_message_goes_on() {
    use rust_cli_kernel_api::store::Fact;
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    let setup = Setup::default();
    let store = setup.store.clone();
    store.fail_next_title_change();
    let session = with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/rename my-draft");
        ui.wait_for("Session renamed to: my-draft").await;
        ui.submit("hi there, first message");
        ui.wait_for("injected title failure").await;
        ui.wait("the reply", |s| idle(s, "   hello")).await;
    })
    .await;
    let id = rust_cli_kernel_api::ids::SessionId::new(session.expect("the session id"));
    let facts = store.facts(&id);
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::TitleChanged { title } if title == "my-draft")),
        "{facts:#?}"
    );
    let request = request_text(&model.requests()[0]);
    assert!(!request.contains("Session renamed to"), "{request}");
    assert!(
        !request.contains("The user named this session"),
        "{request}"
    );
    assert!(request.contains("hi there, first message"), "{request}");
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_with_a_description_enters_plan_mode_and_sends_it() {
    let model = GatedModel::scripted(vec![
        text_response(&["hello"]),
        text_response(&["planning"]),
    ]);
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        // 有会话时：先切模式，回执到了再发输入。
        ui.submit("hi");
        ui.wait("the first reply", |s| idle(s, "   hello")).await;
        ui.submit("/plan write a parser");
        let screen = ui.wait("the reply", |s| idle(s, "   planning")).await;
        assert!(screen.contains("Plan mode"), "{screen}");
        // `/plan open` 见 plan_open.rs（要有能给出路径事实的执行环境）。
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(request_text(&requests[1]).contains("write a parser"));
}

/// 记下写进 `theme.json` 的文字；`fail` 为真时写入失败（tui-kit.md §3.4）。读到的总是没有文件。
#[derive(Default)]
struct ThemeWrites {
    written: Mutex<Vec<String>>,
    fail: bool,
}

impl rust_cli_tui::services::Stores for ThemeWrites {
    fn read(
        &self,
        _file: rust_cli_tui::services::StoreFile,
    ) -> futures_util::future::BoxFuture<'static, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
    }

    fn write(
        &self,
        file: rust_cli_tui::services::StoreFile,
        text: String,
    ) -> futures_util::future::BoxFuture<'static, Result<(), String>> {
        assert_eq!(file, rust_cli_tui::services::StoreFile::Theme);
        self.written.lock().unwrap().push(text);
        let result = if self.fail {
            Err("disk full".to_owned())
        } else {
            Ok(())
        };
        Box::pin(async move { result })
    }
}

fn with_stores(writes: &Arc<ThemeWrites>) -> Setup {
    let mut setup = Setup::default();
    setup.services.stores = Some(writes.clone());
    setup
}

/// 列表里写着 `name` 的那一行（名字在第 3 列起）。
fn theme_row<'a>(screen: &'a str, name: &str) -> &'a str {
    // 列表的行是名字后面至少两格（名字补齐）；输入框上方的一句（`Autonomous: …`）不算。
    let row = format!("{name}  ");
    screen
        .lines()
        .find(|line| line.trim_start().starts_with(&row))
        .unwrap_or_else(|| panic!("no {name} row in {screen}"))
}

/// `/theme`（tui-kit.md §3.4、§4.2）：Puffin 的主题列表（Terminal 与具名主题，正在用的后面 `current`）；⏎ 选定写
/// `theme.json`（与 Puffin 同一格式），输入框上方 `Theme: 名字.`；再打开时它是 `current`。
#[tokio::test(flavor = "multi_thread")]
async fn the_theme_list_saves_the_choice_to_the_puffin_theme_file() {
    let model = GatedModel::scripted(Vec::new());
    let writes = Arc::new(ThemeWrites::default());
    with_ui(model, with_stores(&writes), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/theme");
        let list = ui.wait_for("follows your terminal").await;
        assert!(theme_row(&list, "Terminal").contains("current"), "{list}");
        assert!(theme_row(&list, "Puffin").contains("Day · Night"), "{list}");
        assert!(!theme_row(&list, "Puffin").contains("current"), "{list}");
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("Theme: Puffin.").await;
        ui.submit("/theme");
        let list = ui.wait_for("follows your terminal").await;
        assert!(theme_row(&list, "Puffin").contains("current"), "{list}");
        assert!(!theme_row(&list, "Terminal").contains("current"), "{list}");
    })
    .await;
    let expected = rust_cli_tui_kit::theme::scheme::ThemeChoice {
        theme: "puffin",
        ..Default::default()
    }
    .encode();
    assert_eq!(*writes.written.lock().unwrap(), [expected]);
}

/// esc 关上列表、换回打开时的主题，不写文件；打字过滤主题（名字或 id）。
#[tokio::test(flavor = "multi_thread")]
async fn esc_puts_the_theme_back_and_typing_filters_the_list() {
    let model = GatedModel::scripted(Vec::new());
    let writes = Arc::new(ThemeWrites::default());
    with_ui(model, with_stores(&writes), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/theme");
        ui.wait_for("follows your terminal").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Esc);
        ui.wait("the list closes", |s| !s.contains("follows your terminal"))
            .await;
        // esc 留着草稿 `/theme `：⏎ 再打开；打的字进草稿，按命令后的词过滤（tui-kit.md §4.0「板」）。
        ui.key(KeyCode::Enter);
        ui.wait_for("follows your terminal").await;
        for c in "gruv".chars() {
            ui.key(KeyCode::Char(c));
        }
        let list = ui.wait("the filter", |s| !s.contains("Solarized")).await;
        assert!(
            theme_row(&list, "Gruvbox").contains("light · dark"),
            "{list}"
        );
        assert!(list.contains("› /theme gruv"), "{list}");
        ui.key(KeyCode::Esc);
    })
    .await;
    assert!(writes.written.lock().unwrap().is_empty());
}

/// 写不成时说出原因，主题仍然换了（同 Puffin）：再打开时它是 `current`。
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_theme_write_still_changes_the_theme() {
    let model = GatedModel::scripted(Vec::new());
    let writes = Arc::new(ThemeWrites {
        fail: true,
        ..ThemeWrites::default()
    });
    with_ui(model, with_stores(&writes), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/theme");
        ui.wait_for("follows your terminal").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("Unable to save the theme: disk full").await;
        ui.submit("/theme");
        let list = ui.wait_for("follows your terminal").await;
        assert!(theme_row(&list, "Puffin").contains("current"), "{list}");
    })
    .await;
}
