// `/model` 平铺所有可用的套餐（docs/specs/rust-cli/tui.md `/model`，工作线 F49）：每项一个模型，说明是供应商名；
// 团队套餐每个团队项目一项（`<供应商> Team Plan · <团队>`，中文 `<供应商> 团队套餐 · <团队>`）；选中哪一项，会话的
// 选择就带上那个团队项目。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::model::{ModelConnection, ModelSelection};
use rust_cli_kernel_api::provider::{
    CatalogConnection, CatalogModel, ModelCatalog, ModelDirectory, WireProtocol,
};
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, with_ui};

const PERSONAL: &str = "account:bigmodel-individual-coding-plan";
const TEAM: &str = "account:bigmodel-team-coding-plan";

fn connection(project: &str) -> ModelConnection {
    ModelConnection {
        product_id: "prod-1".into(),
        organization_id: "org-t".into(),
        project_id: project.into(),
    }
}

/// 个人、两个团队项目与另一个 provider。
struct Plans;

#[async_trait::async_trait]
impl ModelDirectory for Plans {
    async fn catalog(&self) -> ModelCatalog {
        let model = |provider: &str, name: &str, team: Option<(&str, &str)>| CatalogModel {
            capabilities: Default::default(),
            provider: provider.into(),
            provider_name: match provider {
                PERSONAL => "BigModel Individual Coding Plan".into(),
                TEAM => "BigModel Team Coding Plan".into(),
                _ => "Fixture".into(),
            },
            model: name.into(),
            protocol: WireProtocol::AnthropicMessages,
            reasoning_levels: vec!["low".into(), "high".into()],
            context_window: 200_000,
            max_output_tokens: 32_000,
            connection: team.map(|(project, team)| CatalogConnection {
                id: connection(project),
                vendor: "BigModel".into(),
                name: team.into(),
            }),
        };
        ModelCatalog {
            revision: 1,
            models: vec![
                model(PERSONAL, "GLM-5.3", None),
                model(TEAM, "GLM-5.3", Some(("p-a", "团队 A"))),
                model(TEAM, "GLM-5.3", Some(("p-b", "团队 B"))),
                model("fixture", "scripted", None),
            ],
            default: Some(ModelSelection {
                provider: PERSONAL.into(),
                model: "GLM-5.3".into(),
                reasoning: Some("high".into()),
                connection: None,
            }),
        }
    }
}

/// 列表里的行（去掉右边的空白）：从标题行到最后一个模型行。
fn list_lines(screen: &str, title: &str, last: &str) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let start = lines.iter().position(|line| line.contains(title)).unwrap();
    let end = start
        + lines[start..]
            .iter()
            .rposition(|line| line.contains(last))
            .unwrap();
    lines[start..=end]
        .iter()
        .map(|line| line.trim_end().to_owned())
        .collect()
}

/// 打开 `/model`，选团队 B（当前是 fixture 的 scripted，在最后一项；往上一项），再打开一次看 ✔ 在哪一项。返回两次
/// 打开时的列表与模型端口。
async fn pick_second_team(
    lang: &'static str,
    title: &'static str,
) -> (Vec<String>, Vec<String>, Arc<GatedModel>) {
    let model = GatedModel::scripted(vec![text_response(&["hi"]), text_response(&["again"])]);
    let seen = Arc::new(std::sync::Mutex::new((Vec::new(), Vec::new())));
    let setup = Setup {
        lang,
        width: 100,
        models: Some(Arc::new(Plans)),
        ..Setup::default()
    };
    let record = seen.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("hello");
        ui.wait("the first reply", |s| {
            s.contains("⏺ hi") && !s.contains("esc to")
        })
        .await;
        ui.submit("/model");
        let screen = ui.wait_for(title).await;
        record.lock().unwrap().0 = list_lines(&screen, title, "Fixture");
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        // 等 footer 换成新模型（会话的 `ModelChanged` 到了）再重开：列表的 ✔ 取打开时视图里的选择，模型以内核为准
        // （tui.md §4.1）。缺陷修复（工作线 F87）：以前列表一关就重开，事件还没到时 ✔ 仍在 scripted 上，偶发失败。
        ui.wait("the session to switch the model", |s| {
            !s.contains(title)
                && s.lines()
                    .last()
                    .is_some_and(|footer| footer.contains("GLM-5.3"))
        })
        .await;
        // 再打开：✔ 只在团队 B（同一 provider 与模型的团队 A 不算当前）。
        ui.submit("/model");
        let screen = ui.wait_for(title).await;
        record.lock().unwrap().1 = list_lines(&screen, title, "Fixture");
        ui.key(KeyCode::Esc);
        ui.wait("the list to close", |s| !s.contains(title)).await;
        ui.submit("again please");
        ui.wait("the second reply", |s| {
            s.contains("⏺ again") && !s.contains("esc to")
        })
        .await;
    })
    .await;
    let (first, second) = seen.lock().unwrap().clone();
    (first, second, model)
}

#[tokio::test(flavor = "multi_thread")]
async fn plans_are_listed_flat_with_their_vendor() {
    let (first, second, model) = pick_second_team("en_US.UTF-8", "Select model").await;
    snapshot(&first, EN_FIRST);
    snapshot(&second, EN_CHOSEN);
    // 选中的是团队 B：下一次请求的选择带上它的团队项目。
    let requests = model.requests();
    assert_eq!(requests[1].selection.provider, TEAM);
    assert_eq!(
        requests[1].selection.connection.as_deref(),
        Some(&connection("p-b"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn team_plans_follow_the_interface_language() {
    let (first, second, model) = pick_second_team("zh_CN.UTF-8", "选择模型").await;
    snapshot(&first, ZH_FIRST);
    snapshot(&second, ZH_CHOSEN);
    assert_eq!(
        model.requests()[1].selection.connection.as_deref(),
        Some(&connection("p-b"))
    );
}

/// 渲染快照：列表区域逐行比较。
fn snapshot(lines: &[String], expected: &str) {
    let got = lines.join("\n");
    assert_eq!(got, expected.trim_matches('\n'), "\n{got}");
}

const EN_FIRST: &str = "
  Select model
  Applies to this session and new sessions started here.
  ⌕ Search…

    GLM-5.3      BigModel Individual Coding Plan
    GLM-5.3      BigModel Team Plan · 团队 A
    GLM-5.3      BigModel Team Plan · 团队 B
  › scripted ✔   Fixture
";

const EN_CHOSEN: &str = "
  Select model
  Applies to this session and new sessions started here.
  ⌕ Search…

    GLM-5.3     BigModel Individual Coding Plan
    GLM-5.3     BigModel Team Plan · 团队 A
  › GLM-5.3 ✔   BigModel Team Plan · 团队 B
    scripted    Fixture
";

const ZH_FIRST: &str = "
  选择模型
  用于本会话与之后在这里开始的新会话。
  ⌕ 搜索…

    GLM-5.3      BigModel Individual Coding Plan
    GLM-5.3      BigModel 团队套餐 · 团队 A
    GLM-5.3      BigModel 团队套餐 · 团队 B
  › scripted ✔   Fixture
";

const ZH_CHOSEN: &str = "
  选择模型
  用于本会话与之后在这里开始的新会话。
  ⌕ 搜索…

    GLM-5.3     BigModel Individual Coding Plan
    GLM-5.3     BigModel 团队套餐 · 团队 A
  › GLM-5.3 ✔   BigModel 团队套餐 · 团队 B
    scripted    Fixture
";
