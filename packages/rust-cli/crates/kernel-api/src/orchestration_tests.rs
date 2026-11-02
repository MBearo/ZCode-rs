//! 编排 trait 的缺省实现（修复线 F16 新增的方法不改变已有实现的行为）与测试代理的中性缺省。

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
            attachments: Vec::new(),
        })
    }
}

/// 只写两个必需方法的注册表（测试代理 `RegistryOverride` 的中性缺省，工作线 F134：`ToolRegistry` 本身没有缺省体）。
struct Empty;

impl crate::testing::RegistryOverride for Empty {
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
    ContextRequest::new(
        crate::ids::SessionId::new("s1"),
        crate::env::WorkspaceRef {
            environment: crate::ids::EnvironmentId::new("local"),
            root: crate::env::EnvPath::new("/Users/dev/w"),
        },
        QuerySource::MainThread,
        crate::permission::PermissionMode::Default,
        crate::model::ModelSelection::new("p".to_owned(), "m".to_owned()),
    )
}

/// F98（remote-exec.md §25.2.1、§25.7）：这次运行的缺省机器只在不是本机时写出；旧形状照常能读。
#[test]
fn the_default_machine_is_written_only_when_remote() {
    let plain = serde_json::to_value(request()).unwrap();
    assert!(plain.get("default_machine").is_none());
    let back: ContextRequest = serde_json::from_value(plain).unwrap();
    assert_eq!(back.default_machine, Default::default());
    let remote = ContextRequest {
        default_machine: crate::ids::MachineName::new("dev-a"),
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

/// 只覆盖 `ask` 的测试代理（工作线 F137：`Interactions` 本身没有缺省体）：撤回与会话级的入口委托给 `NoOneToAsk`。
struct AskOnly;

#[async_trait]
impl crate::testing::AskOverride for AskOnly {
    async fn ask(
        &self,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Err(OrchestrationError::Failed {
            message: "unused".to_owned(),
        })
    }
}

#[test]
fn the_ask_proxy_has_no_lasting_entry_unless_overridden() {
    assert!(Interactions::lasting(&AskOnly).is_none());
}

/// 没有人能回答：权限询问是 Unavailable，提问不回答（按询问的种类）。
#[tokio::test]
async fn no_one_to_ask_answers_by_the_kind_of_question() {
    let permission = InteractionRequest::Permission {
        tool_use: ToolUseId::new("toolu_1"),
        tool: "Bash".to_owned(),
        input: serde_json::json!({"command": "ls"}),
        message: "ask".to_owned(),
        suggestions: Vec::new(),
        rule_seed: None,
        asked_by: None,
        machine: crate::ids::MachineName::primary(),
    };
    let question = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_2"),
        questions: serde_json::Value::Null,
    };
    let nobody = crate::testing::NoOneToAsk;
    assert_eq!(
        nobody.ask(permission).await.unwrap().answer,
        Answer::from(PermissionAnswer::Unavailable)
    );
    assert_eq!(
        nobody.ask(question).await.unwrap().answer,
        Answer::from(QuestionAnswer::Declined)
    );
    assert!(nobody.lasting().is_none());
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

/// 会话关闭是自己的变体（工作线 F130）：文字不变，序列化成 `{"kind": "closed"}`。
#[test]
fn a_closed_session_has_its_own_error() {
    let closed = OrchestrationError::Closed;
    assert_eq!(closed.to_string(), "the session is closed");
    let value = serde_json::to_value(&closed).unwrap();
    assert_eq!(value, serde_json::json!({"kind": "closed"}));
    assert_eq!(
        serde_json::from_value::<OrchestrationError>(value).unwrap(),
        closed
    );
}
