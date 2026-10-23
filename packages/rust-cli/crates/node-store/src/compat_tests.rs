//! 修复线 F2：与 Node 共用数据的记录形状（docs/specs/rust-cli/node-store.md §4.2、§4.5、§4.10）。
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, Message, StopReason, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use serde_json::{Value, json};

use crate::tests::{TestStore, batch, started, text, user};

fn reply(id: &str, model: &str, stop: Option<StopReason>) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: vec![text("partial")],
            model: model.into(),
            stop_reason: stop,
            usage: None,
        }),
    }
}

fn called(id: &str, provider: &str, model: &str) -> Fact {
    Fact::ModelCalled {
        call: ModelCall {
            message: MessageId::new(id),
            source: QuerySource::MainThread,
            selection: ModelSelection {
                provider: provider.into(),
                model: model.into(),
                reasoning: None,
                connection: None,
            },
            started_at_ms: 1000,
            first_token_at_ms: None,
            completed_at_ms: 1000,
            status: CallStatus::Cancelled,
            stop_reason: None,
            tool_calls: 0,
            usage: None,
            retries: 0,
            error: None,
        },
    }
}

fn assistant_rows(t: &TestStore) -> Vec<Value> {
    t.rows("select data from message where json_extract(data, '$.role') = 'assistant' order by time_created, id")
        .into_iter()
        .map(|row| serde_json::from_str(row[0].as_str().unwrap()).unwrap())
        .collect()
}

/// 流式中被中断的 assistant 没有停止原因：Node 写 `unknown`（App 协议的 step-finish `reason` 必填），读回 Unknown。
/// 以前两个键都不写，App 的 strict schema 拒收整个会话。
#[tokio::test]
async fn an_interrupted_step_writes_nodes_unknown_finish() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let facts = vec![user("msg_u1", "Go"), reply("msg_a1", "m", None)];
    t.store
        .commit(&SessionId::new("sess_1"), batch(None, facts))
        .await
        .unwrap();
    assert_eq!(assistant_rows(&t)[0]["finish"], "unknown");
    let reasons = t.rows("select json_extract(data, '$.reason') from part where json_extract(data, '$.type') = 'step-finish'");
    assert_eq!(reasons, vec![vec![json!("unknown")]]);
    let loaded = t.store.load(&SessionId::new("sess_1")).await.unwrap();
    let Message::Assistant(read) = &loaded.messages[1] else {
        panic!("assistant expected");
    };
    assert_eq!(read.stop_reason, Some(StopReason::Unknown));
}

/// Node 的 `modelId` / `providerId` 是请求用的目录模型（同批 `ModelCalled` 的选择）；没有请求时是会话当前的选择，
/// 都不取响应里的模型名（以前取它：带日期或网关别名时 App 的稳定 fork 选中目录外的模型）。
#[tokio::test]
async fn the_assistant_records_the_requested_catalog_model() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let facts = vec![
        user("msg_u1", "Go"),
        reply("msg_a1", "m-20260101", Some(StopReason::EndTurn)),
        called("msg_a1", "p2", "m2"),
    ];
    t.store
        .commit(&SessionId::new("sess_1"), batch(None, facts))
        .await
        .unwrap();
    let facts = vec![
        user("msg_u2", "Again"),
        reply("msg_a2", "m-20260101", Some(StopReason::EndTurn)),
    ];
    t.store
        .commit(&SessionId::new("sess_1"), batch(None, facts))
        .await
        .unwrap();
    let rows = assistant_rows(&t);
    let models: Vec<_> = rows
        .iter()
        .map(|row| (row["providerId"].clone(), row["modelId"].clone()))
        .collect();
    assert_eq!(
        models,
        vec![(json!("p2"), json!("m2")), (json!("p"), json!("m"))]
    );
}

/// 回合结束写会话的 `lastOutcome`（Node `persistDurableSessionEvent` 的三个值）与 `time_updated`。
#[tokio::test]
async fn the_turn_end_records_the_last_outcome() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    for (status, expected, at) in [
        (CallStatus::Completed, "completed", 2000),
        (CallStatus::Cancelled, "interrupted", 3000),
        (CallStatus::Error, "error", 4000),
    ] {
        t.source.at(at);
        let facts = vec![
            user(&format!("msg_u{at}"), "Go"),
            Fact::TurnEnded { status },
        ];
        t.store.commit(&session, batch(None, facts)).await.unwrap();
        assert_eq!(
            t.rows("select last_outcome, time_updated from session"),
            vec![vec![json!(expected), json!(at)]]
        );
    }
}

fn human(id: &str, text: &str) -> Fact {
    user(id, text)
}

fn rows_of(t: &TestStore, sql: &str) -> Vec<Value> {
    t.rows(sql)
        .into_iter()
        .map(|mut row| row.remove(0))
        .collect()
}

/// bash 模式与 skill 命令（Node `persistUserPrompt` 的 displayInput / modelInput）：text 是界面原文，模型文本在
/// `metadata.modelInput`；bash 的输出是合成消息，不开新的轮次；冷读取仍是原来的模型文本与 Human 消息。
#[tokio::test]
async fn bash_and_skill_inputs_store_the_typed_text() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let skill = "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>main</command-args>";
    let output = "<bash-stdout>a.txt</bash-stdout><bash-stderr></bash-stderr>";
    let facts = vec![
        human("msg_u1", "<bash-input>ls</bash-input>"),
        human("msg_u2", output),
        human("msg_u3", skill),
    ];
    t.store
        .commit(&SessionId::new("sess_1"), batch(None, facts))
        .await
        .unwrap();
    assert_eq!(
        rows_of(
            &t,
            "select json_extract(data, '$.text') || '|' || coalesce(json_extract(data, '$.metadata.modelInput'), '') from part order by id"
        ),
        vec![
            json!("! ls|<bash-input>ls</bash-input>"),
            json!(format!("{output}|")),
            json!(format!("/review main|{skill}")),
        ]
    );
    // 输出消息是 Node 运行时的合成消息（来源加前缀），与输入同一轮。
    let messages: Vec<Value> = rows_of(&t, "select data from message order by id")
        .iter()
        .map(|data| serde_json::from_str(data.as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(messages[1]["synthetic"], true);
    assert_eq!(messages[1]["metadata"]["source"], "rust-cli/bash_output");
    assert_eq!(
        messages[1]["anchor"]["turnId"],
        messages[0]["anchor"]["turnId"]
    );
    assert_ne!(
        messages[2]["anchor"]["turnId"],
        messages[0]["anchor"]["turnId"]
    );
    assert_eq!(
        rows_of(&t, "select title from session"),
        vec![json!("! ls")]
    );
    let loaded = t.store.load(&SessionId::new("sess_1")).await.unwrap();
    let texts: Vec<_> = loaded
        .messages
        .iter()
        .map(|message| match message {
            Message::User(user) => (
                user.origin.clone(),
                serde_json::to_value(&user.content).unwrap()[0]["text"].clone(),
            ),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            (UserOrigin::Human, json!("<bash-input>ls</bash-input>")),
            (UserOrigin::Human, json!(output)),
            (UserOrigin::Human, json!(skill)),
        ]
    );
}

/// `plan_mode_exit` 与 Node 提醒表重名：存成 `rust-cli/plan_mode_exit`，冷读取还原原来的来源。
#[tokio::test]
async fn plan_mode_exit_is_stored_under_a_prefixed_source() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let exit = Fact::MessageAppended {
        message: Message::Attachment(AttachmentMessage {
            id: MessageId::new("msg_x1"),
            source: "plan_mode_exit".into(),
            text: "<system-reminder>\nExited.\n</system-reminder>".into(),
            data: json!({"planExists": false}),
        }),
    };
    t.store
        .commit(
            &SessionId::new("sess_1"),
            batch(None, vec![human("msg_u1", "Go"), exit]),
        )
        .await
        .unwrap();
    assert_eq!(
        rows_of(
            &t,
            "select json_extract(data, '$.metadata.source') from message where id = 'msg_x1'"
        ),
        vec![json!("rust-cli/plan_mode_exit")]
    );
    let loaded = t.store.load(&SessionId::new("sess_1")).await.unwrap();
    let Message::Attachment(read) = &loaded.messages[1] else {
        panic!("attachment expected");
    };
    assert_eq!(read.source, "plan_mode_exit");
    assert_eq!(read.data, json!({"planExists": false}));
}

/// Node 群协作会话（`permission.scope = session`）：“总是允许”与改模式只写会话行，保留 `scope`；项目规则集与
/// 项目持久模式不动；`session_rules` 读出会话规则，其他会话为 None。
#[tokio::test]
async fn a_node_group_session_keeps_rules_and_mode_in_the_session_row() {
    use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule, RuleBehavior};
    let t = TestStore::new();
    started(&t, "sess_1").await;
    started(&t, "sess_2").await;
    t.connection()
        .execute(
            "update session set permission = ? where id = 'sess_1'",
            [r#"{"version":1,"scope":"session","mode":"build","allow":[{"toolName":"Read"}]}"#],
        )
        .unwrap();
    let session = SessionId::new("sess_1");
    let remember = Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: vec![PermissionRule {
            tool: "Bash".into(),
            content: Some("npm test:*".into()),
        }],
    };
    t.store
        .commit(&session, batch(None, vec![remember]))
        .await
        .unwrap();
    let facts = vec![
        Fact::ModeChanged {
            mode: PermissionMode::AcceptEdits,
        },
        Fact::ProjectModeSet {
            mode: PermissionMode::AcceptEdits,
        },
    ];
    t.store.commit(&session, batch(None, facts)).await.unwrap();
    assert_eq!(
        rows_of(&t, "select permission from session where id = 'sess_1'"),
        vec![json!(
            r#"{"version":1,"scope":"session","mode":"edit","allow":[{"toolName":"Read"},{"toolName":"Bash","ruleContent":"npm test:*"}]}"#
        )]
    );
    assert_eq!(
        rows_of(&t, "select count(*) from local_setting"),
        vec![json!(0)]
    );
    let rules = t.store.session_rules(&session).await.unwrap().unwrap();
    assert_eq!(
        rules.allow,
        vec![
            PermissionRule {
                tool: "Read".into(),
                content: None
            },
            PermissionRule {
                tool: "Bash".into(),
                content: Some("npm test:*".into())
            }
        ]
    );
    assert_eq!(
        t.store
            .session_rules(&SessionId::new("sess_2"))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        t.store.load(&session).await.unwrap().mode,
        Some(PermissionMode::AcceptEdits)
    );
}

/// 子代理调用的关联（`Fact::SubagentLinked`）写进 tool part 的 `state.metadata`（Node 子代理查询读它）。
#[tokio::test]
async fn a_subagent_link_lands_in_the_tool_part_state() {
    use rust_cli_kernel_api::ids::{AgentId, ToolUseId};
    use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent, UserMessage};
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let call = Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new("msg_a1"),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Agent".into(),
                input: json!({"subagent_type": "Explore", "prompt": "look"}),
            }],
            model: "m".into(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
    };
    let result = Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new("msg_r1"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_1"),
                content: vec![ToolResultContent::Text {
                    text: "report".into(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("msg_a1"),
            },
        }),
    };
    let link = Fact::SubagentLinked {
        tool_use: ToolUseId::new("toolu_1"),
        agent: AgentId::new("agent_1"),
        child: SessionId::new("sess_subagent_agent_1"),
        agent_type: "Explore".into(),
    };
    t.store
        .commit(
            &SessionId::new("sess_1"),
            batch(None, vec![user("msg_u1", "Explore"), call, result, link]),
        )
        .await
        .unwrap();
    assert_eq!(
        rows_of(
            &t,
            "select json_extract(data, '$.state.metadata') from part where json_extract(data, '$.type') = 'tool'"
        ),
        vec![json!(
            r#"{"schemaVersion":1,"childSessionId":"sess_subagent_agent_1","agentId":"agent_1","agentType":"Explore"}"#
        )]
    );
}
