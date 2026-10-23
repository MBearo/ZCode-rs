//! `--model` 的取值（cli-args.md §1.3，工作线 F49）。

use rust_cli_kernel_api::model::ModelConnection;
use rust_cli_kernel_api::provider::{CatalogConnection, CatalogModel, WireProtocol};

use super::{LookupError, is_entry, lookup, selection, written};

const TEAM: &str = "account:bigmodel-team-coding-plan";
const PERSONAL: &str = "account:bigmodel-individual-coding-plan";

fn entry(provider: &str, model: &str, team: Option<(&str, &str)>) -> CatalogModel {
    CatalogModel {
        provider: provider.to_owned(),
        provider_name: provider.to_owned(),
        model: model.to_owned(),
        protocol: WireProtocol::AnthropicMessages,
        reasoning_levels: vec!["low".to_owned(), "high".to_owned()],
        context_window: 1,
        max_output_tokens: 1,
        capabilities: Default::default(),
        connection: team.map(|(project, name)| CatalogConnection {
            id: ModelConnection {
                product_id: "prod-1".to_owned(),
                organization_id: format!("org-{project}"),
                project_id: project.to_owned(),
            },
            vendor: "BigModel".to_owned(),
            name: name.to_owned(),
        }),
    }
}

fn project(found: Result<&CatalogModel, LookupError>) -> Option<String> {
    found
        .unwrap()
        .connection
        .as_ref()
        .map(|c| c.id.project_id.clone())
}

#[test]
fn one_team_needs_no_extra_input() {
    let models = [
        entry(PERSONAL, "GLM-5.3", None),
        entry(TEAM, "GLM-5.3", Some(("p-a", "团队 A"))),
        entry(TEAM, "GLM-5.3-Flash", Some(("p-a", "团队 A"))),
    ];
    // 模型 ID 取第一个匹配：个人在前面。
    assert_eq!(project(lookup(&models, "GLM-5.3")), None);
    assert_eq!(
        project(lookup(&models, &format!("{TEAM}/GLM-5.3"))).as_deref(),
        Some("p-a")
    );
    assert_eq!(
        project(lookup(&models, "GLM-5.3-Flash")).as_deref(),
        Some("p-a")
    );
    let chosen = selection(lookup(&models, &format!("{TEAM}/GLM-5.3")).unwrap());
    assert_eq!(chosen.reasoning.as_deref(), Some("high"));
    assert_eq!(
        chosen.connection.as_ref().unwrap().organization_id,
        "org-p-a"
    );
    assert!(is_entry(&chosen, &models[1]) && !is_entry(&chosen, &models[0]));
}

#[test]
fn several_teams_need_the_team_suffix() {
    let models = [
        entry(TEAM, "GLM-5.3", Some(("p-a", "团队 A"))),
        entry(TEAM, "GLM-5.3", Some(("p-b", "团队 B"))),
    ];
    let name = format!("{TEAM}/GLM-5.3");
    let error = lookup(&models, &name).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "model {name} matches several team plans; pass one of: {TEAM}/GLM-5.3[团队 A], {TEAM}/GLM-5.3[团队 B]"
        )
    );
    // 团队显示名或项目 ID；模型 ID 加后缀也行；方括号里的空白去掉。
    assert_eq!(
        project(lookup(&models, &format!("{TEAM}/GLM-5.3[团队 B]"))).as_deref(),
        Some("p-b")
    );
    assert_eq!(
        project(lookup(&models, "GLM-5.3[ p-a ]")).as_deref(),
        Some("p-a")
    );
    assert_eq!(
        lookup(&models, "GLM-5.3[团队 C]").unwrap_err(),
        LookupError::Missing
    );
    assert_eq!(
        lookup(&models, "GLM-5.3[1m]").unwrap_err(),
        LookupError::Missing
    );
}

#[test]
fn a_shared_team_name_asks_for_the_project() {
    // App 的显示名只用组织名：同一组织的两个项目显示名相同。
    let models = [
        entry(TEAM, "GLM-5.3", Some(("p-a1", "团队 A"))),
        entry(TEAM, "GLM-5.3", Some(("p-a2", "团队 A"))),
    ];
    let error = lookup(&models, "GLM-5.3[团队 A]").unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "team 团队 A matches several team projects of {TEAM}/GLM-5.3; pass one of: {TEAM}/GLM-5.3[p-a1], {TEAM}/GLM-5.3[p-a2]"
        )
    );
    // 列出写法时显示名重复的用项目 ID。
    match lookup(&models, "GLM-5.3").unwrap_err() {
        LookupError::SeveralTeams { choices, .. } => assert_eq!(
            choices,
            [
                format!("{TEAM}/GLM-5.3[p-a1]"),
                format!("{TEAM}/GLM-5.3[p-a2]")
            ]
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        project(lookup(&models, "GLM-5.3[p-a2]")).as_deref(),
        Some("p-a2")
    );
}

#[test]
fn model_ids_with_brackets_are_found_whole() {
    let models = [entry("custom", "weird[1]", None)];
    assert!(lookup(&models, "weird[1]").is_ok());
    assert!(lookup(&models, "custom/weird[1]").is_ok());
    assert_eq!(written(&models[0], false), "custom/weird[1]");
    assert_eq!(lookup(&models, "other").unwrap_err(), LookupError::Missing);
}
