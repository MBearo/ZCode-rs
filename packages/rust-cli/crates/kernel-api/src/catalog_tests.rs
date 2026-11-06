use super::*;
use crate::env::EnvPath;
use crate::ids::EnvironmentId;

/// 跨前端的类型：序列化往返不变，缺省的清单为空。
#[test]
fn catalog_types_round_trip() {
    let query = CatalogQuery {
        session: SessionId::new("sess_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        selection: ModelSelection::new("p".to_owned(), "m".to_owned()),
    };
    let back: CatalogQuery = serde_json::from_value(serde_json::to_value(&query).unwrap()).unwrap();
    assert_eq!(back, query);
    let catalog = SessionCatalog {
        tools: vec!["Task".to_owned(), "Bash".to_owned()],
        agents: vec!["general-purpose".to_owned()],
        skills: vec!["greet".to_owned()],
    };
    let back: SessionCatalog =
        serde_json::from_value(serde_json::to_value(&catalog).unwrap()).unwrap();
    assert_eq!(back, catalog);
    assert_eq!(SessionCatalog::default().tools.len(), 0);
}

/// agent 类型：往返不变；没有工具与模型时不写这两项，来源是 snake_case。
#[test]
fn agent_info_round_trips_and_leaves_out_what_it_lacks() {
    let info = AgentInfo {
        name: "tools:review".to_owned(),
        description: "Reviews a change.".to_owned(),
        origin: AgentOrigin::BuiltIn,
        tools: None,
        model: None,
    };
    let value = serde_json::to_value(&info).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"name": "tools:review", "description": "Reviews a change.", "origin": "built_in"})
    );
    let full = AgentInfo {
        tools: Some(vec!["Read".to_owned()]),
        model: Some("m".to_owned()),
        origin: AgentOrigin::Plugin,
        ..info
    };
    let back: AgentInfo = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
    assert_eq!(back, full);
}
