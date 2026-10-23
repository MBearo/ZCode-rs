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
        selection: ModelSelection {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning: None,
            connection: None,
        },
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
