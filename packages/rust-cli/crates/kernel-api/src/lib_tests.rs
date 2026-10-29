//! 跨端口的类型必须能完整地序列化往返（docs/specs/rust-cli/README.md §6 规则 8）。

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::clock::civil_date;
use crate::env::{EnvError, EnvFacts, EnvPath, OperationContext, Origin, WorkspaceRef};
use crate::ids::{
    EnvironmentId, Generation, InteractionId, MessageId, OperationId, OwnerEpoch, RunId, SessionId,
    ToolUseId, TraceId,
};
use crate::listing::{ListingEntry, Listings};
use crate::message::{
    AssistantMessage, AttachmentMessage, AttachmentRole, ContentBlock, Message, StopReason, Usage,
};
use crate::model::{
    ApiContent, ApiMessage, ApiRole, BlockDelta, CONTENT_BLOCKED, ModelError, ModelEvent,
    ModelNotice, ModelSelection, ModelStreamItem, RequestOptions, ToolFlags, TurnOutputConfig,
};
use crate::orchestration::{InteractionRequest, QueryError};
use crate::provider::{CatalogModel, ModelCatalog, WireProtocol};
use crate::session_api::{KernelEvent, SequencedEvent};
use crate::store::{
    Fact, FactBatch, LoadedSession, OpenOperation, PendingInteraction, SessionKind, SessionMeta,
};
use crate::tool::ToolError;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value, "{text}");
}

fn assistant() -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new("msg_1"),
        content: vec![
            ContentBlock::Thinking {
                thinking: "plan".to_owned(),
                signature: Some("sig".to_owned()),
            },
            ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Read".to_owned(),
                input: json!({"file_path": "a.rs"}),
            },
        ],
        model: "m".to_owned(),
        stop_reason: Some(StopReason::ToolUse),
        usage: Some(Usage::default()),
    })
}

#[test]
fn nested_errors_keep_both_tags() {
    round_trip(&ToolError::Environment(EnvError::NotFound {
        path: "a".to_owned(),
    }));
    round_trip(&QueryError::Model(ModelError::RateLimited {
        message: "slow down".to_owned(),
        retry_after_ms: Some(1000),
    }));
}

#[test]
fn facts_and_events_round_trip() {
    round_trip(&FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None,
        facts: vec![
            Fact::MessageAppended {
                message: assistant(),
            },
            Fact::OperationIntent {
                operation: OperationId::new("op_1"),
                fingerprint: "f".to_owned(),
            },
        ],
        turn: Some(crate::store::TurnAnchor {
            prompt: crate::ids::MessageId::new("msg_u1"),
            turn: Some(crate::ids::TurnId::new("turn_1")),
        }),
    });
    round_trip(&SequencedEvent {
        seq: 7,
        event: KernelEvent::MessageDone {
            message: assistant(),
        },
        revision: 3,
    });
    round_trip(&ModelEvent::BlockDelta {
        index: 0,
        delta: BlockDelta::InputJson("{\"a\"".to_owned()),
    });
}

#[test]
fn loaded_sessions_round_trip_with_open_work() {
    let meta = SessionMeta {
        id: SessionId::new("sess_1"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        title: String::new(),
        trace: TraceId::new("sess_1"),
        created_at_ms: 1,
    };
    let loaded = LoadedSession {
        meta,
        messages: vec![assistant()],
        model: None,
        mode: None,
        plan: false,
        open_operations: vec![OpenOperation {
            operation: OperationId::new("op_1"),
            fingerprint: "f".to_owned(),
        }],
        pending_interactions: vec![PendingInteraction {
            interaction: InteractionId::new("ia_1"),
            request: InteractionRequest::PlanApproval {
                tool_use: ToolUseId::new("toolu_1"),
                plan: "p".to_owned(),
            },
        }],
        checkpoints: vec![checkpoint()],
        turn: None,
        last_turn_failed: true,
        earlier: vec![assistant()],
    };
    round_trip(&loaded);
    // 没有这些字段的旧结果也能读，读出来是空清单。
    let mut old = serde_json::to_value(&loaded).unwrap();
    for key in [
        "open_operations",
        "pending_interactions",
        "checkpoints",
        "last_turn_failed",
        "earlier",
    ] {
        old.as_object_mut().unwrap().remove(key);
    }
    let old: LoadedSession = serde_json::from_value(old).unwrap();
    assert!(old.open_operations.is_empty() && old.pending_interactions.is_empty());
    assert!(old.checkpoints.is_empty());
    assert!(!old.last_turn_failed);
    assert!(old.earlier.is_empty());
}

#[test]
fn operation_context_round_trips() {
    round_trip(&OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g1"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new("op_1"),
        origin: Origin::Agent {
            session: SessionId::new("sess_1"),
            run: RunId::new("run_1"),
        },
    });
}

#[test]
fn unknown_stop_reasons_do_not_fail() {
    let reason: StopReason = serde_json::from_str("\"compaction\"").unwrap();
    assert_eq!(reason, StopReason::Unknown);
    // 窗口用满有自己的变体，serde 名就是线上的值（provider-errors.md D7）；以前落到 Unknown。
    let reason: StopReason = serde_json::from_str("\"model_context_window_exceeded\"").unwrap();
    assert_eq!(reason, StopReason::ModelContextWindowExceeded);
    round_trip(&StopReason::ModelContextWindowExceeded);
}

#[test]
fn context_inputs_round_trip() {
    round_trip(&EnvFacts {
        cwd: EnvPath::new("/work"),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24.6.0".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: true,
        additional_directories: vec![EnvPath::new("/extra")],
        home: Some(EnvPath::new("/home/dev")),
    });
    round_trip(&Listings {
        deferred_tools: vec![ListingEntry {
            name: "WebFetch".to_owned(),
            line: "WebFetch".to_owned(),
        }],
        agents: Vec::new(),
        skills: Vec::new(),
    });
    // 旧的事实快照没有新增的字段，也能读。
    let old: EnvFacts = serde_json::from_value(json!({
        "cwd": "/w", "platform": "linux", "os_version": "Linux 6", "shell": "bash", "is_git_repo": false
    }))
    .unwrap();
    assert!(old.additional_directories.is_empty() && old.home.is_none());
}

#[test]
fn system_turns_carry_their_output_config() {
    let message = ApiMessage {
        role: ApiRole::System,
        content: vec![ApiContent {
            block: ContentBlock::Text {
                text: "# Environment".to_owned(),
            },
            cache_control: None,
        }],
        output_config: Some(TurnOutputConfig {
            effort: Some("medium".to_owned()),
        }),
    };
    round_trip(&message);
    let value = serde_json::to_value(&message).unwrap();
    assert_eq!(value["output_config"], json!({"effort": "medium"}));
}

#[test]
fn attachment_roles_follow_the_source() {
    let attachment = |source: &str| AttachmentMessage {
        id: MessageId::new("m"),
        source: source.to_owned(),
        text: String::new(),
        data: serde_json::Value::Null,
    };
    assert_eq!(attachment("instructions").role(), AttachmentRole::User);
    assert_eq!(
        attachment("remote_session_change").role(),
        AttachmentRole::User
    );
    assert_eq!(attachment("environment").role(), AttachmentRole::System);
    assert_eq!(attachment("something_new").role(), AttachmentRole::System);
}

#[test]
fn attachment_blocks_live_in_data() {
    let image = ContentBlock::Image {
        source: crate::message::ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: "iVBORw0KGgo=".to_owned(),
        },
    };
    let attachment = |data: serde_json::Value| AttachmentMessage {
        id: MessageId::new("m"),
        source: "file".to_owned(),
        text: "<system-reminder>\ncall\n</system-reminder>".to_owned(),
        data,
    };
    let with = attachment(json!({
        "filename": "/w/a.png",
        crate::message::ATTACHMENT_BLOCKS: [serde_json::to_value(&image).unwrap()],
    }));
    assert_eq!(with.blocks(), vec![image]);
    assert!(attachment(serde_json::Value::Null).blocks().is_empty());
    assert!(
        attachment(json!({"blocks": "not blocks"}))
            .blocks()
            .is_empty()
    );
}

#[test]
fn civil_dates_match_the_calendar() {
    assert_eq!(civil_date(0), "1970-01-01");
    // 2026-10-17T00:00:00Z 与当天最后一毫秒。
    assert_eq!(civil_date(1_792_195_200_000), "2026-10-17");
    assert_eq!(civil_date(1_792_281_599_999), "2026-10-17");
    assert_eq!(civil_date(951_782_400_000), "2000-02-29");
}

#[test]
fn provider_and_failover_types_round_trip() {
    round_trip(&ModelError::FallbackTriggered {
        original_model: "a".to_owned(),
        fallback_model: "b".to_owned(),
    });
    round_trip(&ModelStreamItem::Notice(ModelNotice::Retrying {
        attempt: 1,
        max_retries: 10,
        delay_ms: 500,
        error: ModelError::Overloaded {
            message: "busy".to_owned(),
        },
    }));
    round_trip(&ModelStreamItem::Event(ModelEvent::MessageStop));
    // L35：停顿与恢复的通知、流空闲的错误（stall-visibility.md §5）。
    round_trip(&ModelStreamItem::Notice(ModelNotice::Stalled {
        retry_in_ms: 280_000,
    }));
    round_trip(&ModelStreamItem::Notice(ModelNotice::Resumed));
    round_trip(&ModelError::StreamIdle { idle_ms: 300_000 });
    assert_eq!(
        ModelError::StreamIdle { idle_ms: 300_000 }.to_string(),
        "Stream idle timeout - no chunks received"
    );
    // 三个新错误的文字就是给用户的说明（provider-errors.md §6）；内容拦截与结束原因共用一句。
    for error in [
        ModelError::QuotaExhausted {
            message: "余额不足".to_owned(),
        },
        ModelError::ContentBlocked {
            message: "blocked".to_owned(),
        },
        ModelError::RequestTooLarge {
            message: "too big".to_owned(),
        },
    ] {
        round_trip(&error);
    }
    assert_eq!(
        ModelError::QuotaExhausted {
            message: "余额不足".to_owned()
        }
        .to_string(),
        "API Error: Credit balance or quota exhausted · 余额不足"
    );
    assert_eq!(
        ModelError::ContentBlocked {
            message: "blocked".to_owned()
        }
        .to_string(),
        format!("{CONTENT_BLOCKED} · blocked")
    );
    assert!(
        ModelError::RequestTooLarge {
            message: "too big".to_owned()
        }
        .to_string()
        .ends_with("/rewind to go back to before they were added. · too big")
    );
    let mut options = RequestOptions {
        fallback_model: Some("b".to_owned()),
        ..RequestOptions::default()
    };
    options.tool_flags.insert(
        "Search".to_owned(),
        ToolFlags {
            defer_loading: true,
            ..ToolFlags::default()
        },
    );
    round_trip(&options);
    round_trip(&ModelCatalog {
        revision: 3,
        models: vec![CatalogModel {
            capabilities: Default::default(),
            provider: "p".to_owned(),
            provider_name: "P".to_owned(),
            model: "m".to_owned(),
            protocol: WireProtocol::OpenAiResponses,
            reasoning_levels: vec!["low".to_owned(), "high".to_owned()],
            context_window: 200_000,
            max_output_tokens: 32_000,
            connection: None,
        }],
        default: Some(ModelSelection {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning: Some("high".to_owned()),
            connection: None,
        }),
    });
}

/// 修复线 F61：压缩用的模型上限。没有输出上限时不写出，旧数据照常解析。
#[test]
fn token_limits_round_trip() {
    use crate::provider::TokenLimits;
    let full = TokenLimits {
        context_window: 1_000_000,
        max_output_tokens: Some(128_000),
    };
    round_trip(&full);
    let open = TokenLimits {
        context_window: 200_000,
        max_output_tokens: None,
    };
    round_trip(&open);
    assert_eq!(
        serde_json::to_value(open).unwrap(),
        serde_json::json!({"context_window": 200_000})
    );
}

/// 工作线 F49：模型选择与目录项的账号连接（Team Plan 的团队项目）。没有时不写出，旧数据照常解析；写出的键名是
/// App 选择的写法（camelCase）。
#[test]
fn model_connections_round_trip_and_default_to_absent() {
    use crate::model::ModelConnection;
    use crate::provider::CatalogConnection;
    let id = ModelConnection {
        product_id: "prod-1".to_owned(),
        organization_id: "org-t".to_owned(),
        project_id: "p-t".to_owned(),
    };
    let selection = ModelSelection {
        provider: "account:bigmodel-team-coding-plan".to_owned(),
        model: "GLM-5.3".to_owned(),
        reasoning: None,
        connection: Some(Box::new(id.clone())),
    };
    round_trip(&selection);
    assert_eq!(
        serde_json::to_value(&selection).unwrap()["connection"],
        json!({"productId": "prod-1", "organizationId": "org-t", "projectId": "p-t"})
    );
    let old: ModelSelection =
        serde_json::from_value(json!({"provider": "p", "model": "m"})).unwrap();
    assert_eq!(old.connection, None);
    assert!(
        serde_json::to_value(&old)
            .unwrap()
            .get("connection")
            .is_none()
    );
    round_trip(&CatalogConnection {
        id,
        vendor: "BigModel".to_owned(),
        name: "团队".to_owned(),
    });
}

#[test]
fn new_request_options_default_to_absent() {
    // 新字段为空时不出现在序列化结果里，旧数据照常解析。
    assert_eq!(
        serde_json::to_value(RequestOptions::default()).unwrap(),
        json!({})
    );
    let parsed: RequestOptions = serde_json::from_value(json!({"effort": "low"})).unwrap();
    assert!(parsed.tool_flags.is_empty() && parsed.fallback_model.is_none());
    assert_eq!(
        serde_json::to_value(WireProtocol::OpenAiChatCompletions).unwrap(),
        json!("openai-chat-completions")
    );
    assert_eq!(
        WireProtocol::parse("anthropic-messages"),
        Some(WireProtocol::AnthropicMessages)
    );
    assert_eq!(WireProtocol::parse("gemini"), None);
}

#[test]
fn question_and_plan_answers_have_a_stable_shape() {
    use crate::orchestration::{PlanApprovalAnswer, QuestionAnswer};
    use crate::permission::PermissionMode;

    let answered = QuestionAnswer::Answered {
        answers: [("Which database?".to_owned(), "SQLite".to_owned())].into(),
        annotations: Default::default(),
        response: None,
        follow_up: false,
        afk_timeout_ms: None,
    };
    round_trip(&answered);
    assert_eq!(
        serde_json::to_value(&answered).unwrap(),
        json!({"decision": "answered", "answers": {"Which database?": "SQLite"}})
    );
    // 只有 `answers` 的旧回答照读（新字段缺省）。
    assert_eq!(
        serde_json::from_value::<QuestionAnswer>(
            json!({"decision": "answered", "answers": {"Which database?": "SQLite"}})
        )
        .unwrap(),
        answered
    );
    let annotated = QuestionAnswer::Answered {
        answers: [("Layout?".to_owned(), "Top".to_owned())].into(),
        annotations: [(
            "Layout?".to_owned(),
            crate::orchestration::QuestionAnnotation {
                preview: Some("## Top".to_owned()),
                notes: Some("wider".to_owned()),
            },
        )]
        .into(),
        response: Some("later".to_owned()),
        follow_up: true,
        afk_timeout_ms: Some(60_000),
    };
    round_trip(&annotated);
    assert_eq!(
        serde_json::to_value(&annotated).unwrap(),
        json!({"decision": "answered", "answers": {"Layout?": "Top"},
               "annotations": {"Layout?": {"preview": "## Top", "notes": "wider"}},
               "response": "later", "follow_up": true, "afk_timeout_ms": 60000})
    );
    round_trip(&QuestionAnswer::Declined);
    // 旧会话的询问（工作线 F72，agents.md §4.1.2）：标题为空时不写出。
    let asked = crate::orchestration::DetachedEvent::Asked {
        ask: Box::new(crate::orchestration::DetachedAsk {
            interaction: InteractionId::new("i1"),
            session: SessionId::new("s1"),
            title: String::new(),
            request: InteractionRequest::Question {
                tool_use: ToolUseId::new("tu1"),
                questions: json!([]),
            },
        }),
    };
    round_trip(&asked);
    assert_eq!(
        serde_json::to_value(&asked).unwrap(),
        json!({"type": "asked", "ask": {"interaction": "i1", "session": "s1",
               "request": {"kind": "question", "tool_use": "tu1", "questions": []}}})
    );
    round_trip(&crate::orchestration::DetachedEvent::Resolved {
        interaction: InteractionId::new("i1"),
    });
    let approve = PlanApprovalAnswer::Approve {
        mode: PermissionMode::AcceptEdits,
    };
    round_trip(&approve);
    assert_eq!(
        serde_json::to_value(&approve).unwrap(),
        json!({"decision": "approve", "mode": "acceptEdits"})
    );
    assert_eq!(
        serde_json::to_value(PlanApprovalAnswer::KeepPlanning { feedback: None }).unwrap(),
        json!({"decision": "keep_planning"})
    );
}

#[test]
fn compaction_types_round_trip() {
    use crate::orchestration::{AutoCompactOutcome, CompactTracking};
    round_trip(&CompactTracking {
        compacted: true,
        turn_counter: 2,
        consecutive_failures: 1,
        consecutive_rapid_refills: 0,
    });
    round_trip(&AutoCompactOutcome::Thrashing {
        message: "Autocompact is thrashing".to_owned(),
    });
    round_trip(&AutoCompactOutcome::NotNeeded);
}

fn checkpoint() -> crate::store::WorkspaceCheckpoint {
    crate::store::WorkspaceCheckpoint {
        machine: None,
        id: "checkpoint_1".to_owned(),
        prompt: crate::ids::MessageId::new("msg_u"),
        assistant: crate::ids::MessageId::new("msg_a"),
        tool_use: ToolUseId::new("toolu_1"),
        snapshot: crate::store::ArtifactRef {
            session: crate::ids::SessionId::new("sess_1"),
            id: crate::ids::ArtifactId::new("tool-result-1"),
            purpose: crate::store::ArtifactPurpose::Checkpoint,
            media_type: "application/json".to_owned(),
            size: 10,
        },
    }
}

#[path = "lib_tests_more.rs"]
mod more;
