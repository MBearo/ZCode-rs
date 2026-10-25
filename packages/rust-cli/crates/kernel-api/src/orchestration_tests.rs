//! 编排 trait 的缺省实现（修复线 F16 新增的方法不改变已有实现的行为）。

use std::sync::Arc;

use super::*;
use crate::model::QuerySource;
use crate::tool::Tool;

struct Turns;

/// 只实现 `build` 的上下文（没有拆开当步 attachment 的实现）。
struct BuildOnly;

#[async_trait]
impl ContextBuilder for BuildOnly {
    async fn build(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        Ok(BuiltContext {
            system: Vec::new(),
            user_context: Vec::new(),
            attachments: Vec::new(),
        })
    }
}

/// 只实现两个必需方法的注册表。
struct Empty;

impl ToolRegistry for Empty {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn find(&self, _name: &str) -> Option<Arc<dyn Tool>> {
        None
    }
}

#[async_trait]
impl ContextBuilder for Turns {
    async fn build(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        Ok(BuiltContext {
            system: Vec::new(),
            user_context: Vec::new(),
            attachments: Vec::new(),
        })
    }

    async fn turn_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        Ok(vec![AttachmentMessage {
            id: crate::ids::MessageId::new("m1"),
            source: "environment".to_owned(),
            text: String::new(),
            data: serde_json::Value::Null,
        }])
    }
}

fn request() -> ContextRequest {
    ContextRequest {
        capabilities: None,
        session: crate::ids::SessionId::new("s1"),
        workspace: crate::env::WorkspaceRef {
            environment: crate::ids::EnvironmentId::new("local"),
            root: crate::env::EnvPath::new("/Users/dev/w"),
        },
        agent: None,
        source: QuerySource::MainThread,
        mode: crate::permission::PermissionMode::Default,
        tool_names: Vec::new(),
        selection: crate::model::ModelSelection {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning: None,
            connection: None,
        },
        plan_flags: Default::default(),
        mcp: None,
        default_machine: None,
    }
}

/// F98（remote-exec.md §25.2.1、§25.7）：这次运行的缺省机器只在不是本机时写出；旧形状照常能读。
#[test]
fn the_default_machine_is_written_only_when_remote() {
    let plain = serde_json::to_value(request()).unwrap();
    assert!(plain.get("default_machine").is_none());
    let back: ContextRequest = serde_json::from_value(plain).unwrap();
    assert_eq!(back.default_machine, None);
    let remote = ContextRequest {
        default_machine: Some("dev-a".to_owned()),
        ..request()
    };
    let value = serde_json::to_value(&remote).unwrap();
    assert_eq!(value["default_machine"], "dev-a");
    assert_eq!(
        serde_json::from_value::<ContextRequest>(value).unwrap(),
        remote
    );
}

#[tokio::test]
async fn start_attachments_default_to_the_turn_attachments() {
    let turns = Turns.start_attachments(request(), &[]).await.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].source, "environment");
    let only = BuildOnly.start_attachments(request(), &[]).await.unwrap();
    assert!(only.is_empty());
}

#[test]
fn registries_withhold_nothing_and_are_sorted_by_default() {
    let tools: Arc<dyn ToolRegistry> = Arc::new(Empty);
    assert!(tools.withheld("Grep").is_none());
    assert!(!tools.keeps_order());
    // 缺省不藏任何工具（工作线 F69 的 `hides`）。
    assert!(!tools.hides("Agent"));
}

/// 只实现 `ask` 的交互：不给会话级的询问入口。
struct AskOnly;

#[async_trait]
impl Interactions for AskOnly {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Err(OrchestrationError::Failed {
            message: "unused".to_owned(),
        })
    }
}

#[test]
fn interactions_have_no_lasting_entry_by_default() {
    assert!(AskOnly.lasting(&SessionId::new("sess_1")).is_none());
}

#[test]
fn the_asking_agent_is_optional_on_the_wire() {
    // 没有 `asked_by` 的旧记录照读；None 不写出，写出的 JSON 与以前相同。
    let old = serde_json::json!({
        "kind": "permission", "tool_use": "toolu_1", "tool": "Bash",
        "input": {"command": "ls"}, "message": "ask"
    });
    let request: InteractionRequest = serde_json::from_value(old.clone()).unwrap();
    let InteractionRequest::Permission { asked_by, .. } = &request else {
        panic!("{request:?}");
    };
    assert_eq!(*asked_by, None);
    assert_eq!(serde_json::to_value(&request).unwrap(), old);
    let mut tagged = old;
    tagged["asked_by"] = serde_json::json!("Explore");
    let request: InteractionRequest = serde_json::from_value(tagged.clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), tagged);
}
