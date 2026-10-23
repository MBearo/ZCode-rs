//! 后来新增的跨端口类型的往返测试（WebFetch、服务端工具、工具调用输出、checkpoint）。

use super::*;

#[test]
fn web_fetch_dtos_round_trip() {
    round_trip(&crate::env_web::FetchUrl {
        url: "https://example.com/a".to_owned(),
        headers: vec![("accept".to_owned(), "text/html".to_owned())],
        timeout_ms: 60_000,
        max_bytes: 10 * 1024 * 1024,
    });
    round_trip(&crate::env_web::FetchedUrl {
        status: 301,
        content_type: String::new(),
        location: Some("/b".to_owned()),
        retry_after: None,
        body: b"moved".to_vec(),
    });
}

#[test]
fn server_tool_blocks_and_options_round_trip() {
    round_trip(&ContentBlock::ServerToolUse {
        id: "srvtoolu_1".to_owned(),
        name: "web_search".to_owned(),
        input: json!({"query": "rust"}),
    });
    round_trip(&ContentBlock::WebSearchToolResult {
        tool_use_id: "srvtoolu_1".to_owned(),
        content: json!({"type": "web_search_tool_result_error", "error_code": "max_uses_exceeded"}),
    });
    round_trip(&crate::model::RequestOptions {
        server_tools: vec![crate::model::ServerTool::WebSearch {
            allowed_domains: Some(vec!["example.com".to_owned()]),
            blocked_domains: None,
            max_uses: 8,
        }],
        ..crate::model::RequestOptions::default()
    });
}

#[test]
fn tool_call_output_round_trips() {
    use crate::message::{ToolResultContent, UserMessage, UserOrigin};
    use crate::tool::{ToolCallOutput, ToolOutput};
    let output = ToolOutput {
        content: vec![ToolResultContent::Text {
            text: "Launching skill: greet".to_owned(),
        }],
        is_error: false,
        display: serde_json::Value::Null,
    };
    round_trip(&ToolCallOutput {
        output: output.clone(),
        new_messages: vec![Message::User(UserMessage {
            id: MessageId::new("msg_skill"),
            content: vec![ContentBlock::Text {
                text: "Base directory for this skill: /w/.zcode/skills/greet".to_owned(),
            }],
            origin: UserOrigin::Meta {
                source: "skill".to_owned(),
            },
        })],
    });
    // 没有新消息时不写这个字段，旧形状照常能读。
    let plain: ToolCallOutput = serde_json::from_value(json!({"output": output})).unwrap();
    assert!(plain.new_messages.is_empty());
}

#[test]
fn checkpoint_facts_round_trip() {
    round_trip(&crate::store::Fact::CheckpointCreated {
        checkpoint: checkpoint(),
    });
    round_trip(&crate::store::Fact::CheckpointsRestored {
        checkpoints: vec!["checkpoint_1".to_owned()],
    });
    round_trip(&crate::session_api::RewindOption {
        message: crate::ids::MessageId::new("msg_u"),
        files: vec![crate::session_api::FileChange {
            path: "/work/a.txt".to_owned(),
            additions: 1,
            deletions: 2,
        }],
        code: true,
    });
}

/// L11b：定时任务与 worktree 的 DTO（docs/specs/rust-cli/tools-more.md §10.5、§11.5）。
#[test]
fn cron_and_worktree_dtos_round_trip() {
    use crate::env::EnvPath;
    use crate::env_worktree::{
        WorktreeChanges, WorktreeCreate, WorktreeCreated, WorktreeEntry, WorktreeInspect,
        WorktreeList, WorktreeRemove,
    };
    round_trip(&crate::cron::CronFire {
        job: "1a2b3c4d".to_owned(),
        prompt: "check the deploy".to_owned(),
        kind: crate::cron::CronFireKind::Loop,
    });
    // 旧形状（没有 kind）按普通任务读（serde 缺省，L22）。
    let old: crate::cron::CronFire =
        serde_json::from_value(serde_json::json!({"job": "1a2b3c4d", "prompt": "p"})).unwrap();
    assert_eq!(old.kind, crate::cron::CronFireKind::Task);
    round_trip(&crate::session_api::KernelEvent::ScheduledFire {
        at: "Oct 18 3:04pm".to_owned(),
        kind: crate::cron::CronFireKind::Loop,
        fold: Some(crate::cron::LoopFold {
            streak: 2,
            since: "Oct 18 2:44pm".to_owned(),
        }),
    });
    let id = crate::ids::MessageId::new("msg_mueycw12_c9269e63-088e-46d9-aa6f-dadc1eebd2e1");
    assert_eq!(
        id.created_ms(),
        Some(u64::from_str_radix("mueycw12", 36).unwrap())
    );
    assert_eq!(crate::ids::MessageId::new("m1").created_ms(), None);
    round_trip(&WorktreeCreate {
        cwd: EnvPath::new("/Users/dev/repo"),
        name: "user/feature".to_owned(),
    });
    for created in [
        WorktreeCreated::Ready {
            path: EnvPath::new("/Users/dev/repo/.zcode/worktrees/user+feature"),
            branch: "worktree-user+feature".to_owned(),
            head_commit: Some("abc123".to_owned()),
            existed: false,
        },
        WorktreeCreated::NotARepository,
        WorktreeCreated::Failed {
            message: "fatal".to_owned(),
        },
    ] {
        round_trip(&created);
    }
    round_trip(&WorktreeList {
        root: Some(EnvPath::new("/Users/dev/repo")),
        entries: vec![WorktreeEntry {
            path: EnvPath::new("/Users/dev/repo"),
            branch: Some("main".to_owned()),
        }],
    });
    round_trip(&WorktreeInspect {
        path: EnvPath::new("/Users/dev/repo/.zcode/worktrees/a"),
        base: None,
    });
    round_trip(&WorktreeChanges {
        changed_files: 2,
        commits: 1,
    });
    round_trip(&WorktreeRemove {
        path: EnvPath::new("/Users/dev/repo/.zcode/worktrees/a"),
        branch: Some("worktree-a".to_owned()),
    });
}

/// 新方法的默认值：没有会话 owner 时不改目录、顶层会话是自己、偏移按 UTC、没有定时任务。
#[tokio::test]
async fn l11b_defaults() {
    use crate::cron::{CronSignals, NoCron};
    use crate::orchestration::Interactions;
    struct Bare;
    #[async_trait::async_trait]
    impl Interactions for Bare {
        async fn ask(
            &self,
            _session: &crate::ids::SessionId,
            _request: crate::orchestration::InteractionRequest,
        ) -> Result<crate::orchestration::InteractionAnswer, crate::orchestration::OrchestrationError>
        {
            unreachable!("not asked")
        }
    }
    let session = crate::ids::SessionId::new("sess_1");
    assert_eq!(Bare.owner_session(&session).await, session);
    assert!(
        Bare.set_workspace(&session, crate::env::EnvPath::new("/Users/dev"))
            .await
            .is_err()
    );
    assert!(NoCron.take_due(&session).await.is_empty());
}

#[test]
fn l13_facts_commands_and_rules_round_trip() {
    use crate::permission::{PermissionRule, ProjectRules, RuleBehavior, RuleSource};
    round_trip(&crate::store::Fact::TitleGenerated {
        title: "Session titles".to_owned(),
        message: Some(MessageId::new("msg_1")),
    });
    round_trip(&crate::store::Fact::PermissionRulesAdded {
        behavior: RuleBehavior::Allow,
        rules: vec![PermissionRule {
            tool: "Bash".to_owned(),
            content: Some("git status:*".to_owned()),
        }],
    });
    round_trip(&crate::session_api::Command::Rename {
        title: "Mine".to_owned(),
    });
    let rules = ProjectRules {
        allow: vec![PermissionRule {
            tool: "Read".to_owned(),
            content: None,
        }],
        deny: Vec::new(),
        ask: vec![PermissionRule {
            tool: "Bash".to_owned(),
            content: Some("rm:*".to_owned()),
        }],
    };
    round_trip(&rules);
    let sourced = rules.sourced();
    assert_eq!(sourced.len(), 2);
    assert!(
        sourced
            .iter()
            .all(|rule| rule.source == RuleSource::LocalSettings)
    );
    assert_eq!(sourced[1].behavior, RuleBehavior::Ask);
    let main = crate::model::ModelSelection {
        provider: "anthropic".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        reasoning: Some("high".to_owned()),
        connection: None,
    };
    // 小模型槽位现在等于当前主模型（model.md §6.1）。
    assert_eq!(crate::model::small_fast_model(&main), main);
}

/// L19：项目持久模式的事实、不带参数的 `/rename`、structured output 的格式（只新增）。
#[test]
fn l19_facts_commands_and_options_round_trip() {
    use crate::permission::PermissionMode;
    round_trip(&crate::store::Fact::ProjectModeSet {
        mode: PermissionMode::AcceptEdits,
    });
    round_trip(&crate::session_api::Command::GenerateName);
    let options = crate::model::RequestOptions {
        effort: Some("medium".to_owned()),
        output_format: Some(crate::model::OutputFormat::JsonSchema {
            schema: json!({"type": "object", "properties": {"name": {"type": "string"}}}),
        }),
        ..crate::model::RequestOptions::default()
    };
    round_trip(&options);
    let text = serde_json::to_value(&options.output_format).unwrap();
    assert_eq!(text["type"], "json_schema");
    // 旧的序列化结果（没有 output_format）照常能读。
    let old: crate::model::RequestOptions = serde_json::from_value(json!({})).unwrap();
    assert_eq!(old.output_format, None);
}

/// F64：请求的回合与子代理（每请求头的 `x-query-id`、`x-zcode-session-type`，只新增）。
#[test]
fn f64_request_run_and_agent_round_trip() {
    use crate::ids::{AgentId, RunId};
    let options = crate::model::RequestOptions {
        run: Some(RunId::new("run_1")),
        agent: Some(AgentId::new("agent_1")),
        ..crate::model::RequestOptions::default()
    };
    round_trip(&options);
    // 没有值时不写出；旧的序列化结果照常能读。
    let empty = serde_json::to_value(crate::model::RequestOptions::default()).unwrap();
    assert!(empty.get("run").is_none() && empty.get("agent").is_none());
    let old: crate::model::RequestOptions = serde_json::from_value(json!({})).unwrap();
    assert_eq!((old.run, old.agent), (None, None));
}

/// 存储端口的项目持久模式缺省不支持：没有本机项目的存储（测试替身、云端）不必实现它。
#[tokio::test]
async fn project_mode_is_unsupported_by_default() {
    use crate::store::SessionStore;
    let store = crate::testing::MemoryStore::default();
    let workspace = crate::env::WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: crate::env::EnvPath::new("/Users/dev/project"),
    };
    assert!(matches!(
        store.project_mode(&workspace).await,
        Err(crate::store::StoreError::Unsupported { .. })
    ));
}

/// `RunSink::take_notices` 缺省为空：没有通知来源的输出端（会话 actor、测试替身）不必实现它。
#[tokio::test]
async fn run_sinks_have_no_notices_by_default() {
    use crate::orchestration::RunSink;
    let sink = crate::testing::RecordingSink::default();
    assert!(sink.take_notices().await.is_empty());
}

/// L18：只供显示的系统提示随事件流往返（前端经序列化收到它）。
#[test]
fn system_notices_round_trip() {
    use crate::session_api::{KernelEvent, NoticeLevel};
    let event = KernelEvent::SystemNotice {
        level: NoticeLevel::Warning,
        text: "UserPromptSubmit operation blocked by hook:\nno".to_owned(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "system_notice");
    assert_eq!(value["level"], "warning");
    assert_eq!(serde_json::from_value::<KernelEvent>(value).unwrap(), event);
}

/// L35：模型通知随子循环事件（子代理的进度）与内核事件（主线程）往返（stall-visibility.md §5）。
#[test]
fn model_notices_round_trip_in_query_and_kernel_events() {
    use crate::model::ModelNotice;
    use crate::orchestration::QueryEvent;
    use crate::session_api::KernelEvent;
    let notice = ModelNotice::Stalled {
        retry_in_ms: 280_000,
    };
    let event = QueryEvent::ModelNotice {
        notice: notice.clone(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "model_notice");
    assert_eq!(value["notice"]["type"], "stalled");
    assert_eq!(serde_json::from_value::<QueryEvent>(value).unwrap(), event);
    let event = KernelEvent::ModelNotice { notice };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "model_notice");
    assert_eq!(serde_json::from_value::<KernelEvent>(value).unwrap(), event);
}

/// `RunSink::take_absorbed` 缺省是先插话、后通知（子代理的输出端用它）。
#[tokio::test]
async fn run_sinks_absorb_nothing_by_default() {
    use crate::orchestration::RunSink;
    let sink = crate::testing::RecordingSink::default();
    assert!(sink.take_absorbed().await.is_empty());
}

/// 修复线 F2 的事实与端口缺省：`SubagentLinked` 往返；会话自己的规则集缺省为 None（规则在项目规则集）。
#[tokio::test]
async fn f2_subagent_link_round_trips_and_session_rules_default_to_none() {
    use crate::store::SessionStore;
    round_trip(&crate::store::Fact::SubagentLinked {
        tool_use: crate::ids::ToolUseId::new("toolu_1"),
        agent: crate::ids::AgentId::new("agent_1"),
        child: SessionId::new("sess_subagent_agent_1"),
        agent_type: "Explore".to_owned(),
    });
    let store = crate::testing::MemoryStore::default();
    assert_eq!(
        store
            .session_rules(&SessionId::new("sess_1"))
            .await
            .unwrap(),
        None
    );
}

/// F7：`Ids::command` / `generation` 的缺省是一个新的操作 ID 的文字，每次不同（只加的接口，旧实现不用改）。
#[test]
fn command_and_generation_ids_default_to_fresh_operation_ids() {
    struct Counting(std::sync::atomic::AtomicU64);
    impl crate::clock::Ids for Counting {
        fn session(&self) -> crate::ids::SessionId {
            unreachable!()
        }
        fn subagent(&self) -> (crate::ids::AgentId, crate::ids::SessionId) {
            unreachable!()
        }
        fn message(&self) -> crate::ids::MessageId {
            unreachable!()
        }
        fn turn(&self) -> crate::ids::TurnId {
            unreachable!()
        }
        fn run(&self) -> crate::ids::RunId {
            unreachable!()
        }
        fn operation(&self) -> crate::ids::OperationId {
            let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::ids::OperationId::new(format!("op_{n}"))
        }
        fn interaction(&self) -> crate::ids::InteractionId {
            unreachable!()
        }
    }
    use crate::clock::Ids as _;
    let ids = Counting(std::sync::atomic::AtomicU64::new(1));
    assert_eq!(ids.command().as_str(), "op_1");
    assert_eq!(ids.command().as_str(), "op_2");
    assert_eq!(ids.generation().as_str(), "op_3");
}

/// F10：`QueryRequest.prompt_hook_messages` 可缺省（旧的序列化结果照常能读），空时不写出。
#[test]
fn prompt_hook_messages_default_to_empty() {
    use crate::env::{EnvPath, WorkspaceRef};
    use crate::ids::{EnvironmentId, RunId, SessionId};
    use crate::model::{ModelSelection, QuerySource};
    use crate::orchestration::QueryRequest;
    use crate::permission::PermissionMode;
    let mut request = QueryRequest {
        session: SessionId::new("sess_1"),
        run: RunId::new("run_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        agent: None,
        messages: Vec::new(),
        selection: ModelSelection {
            provider: "anthropic".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            reasoning: None,
            connection: None,
        },
        mode: PermissionMode::Default,
        source: QuerySource::MainThread,
        max_turns: None,
        prompt_hook_messages: Vec::new(),
    };
    let text = serde_json::to_string(&request).unwrap();
    assert!(!text.contains("prompt_hook_messages"), "{text}");
    let back: QueryRequest = serde_json::from_str(&text).unwrap();
    assert_eq!(back, request);
    request.prompt_hook_messages = vec![assistant()];
    round_trip(&request);
}

/// F10：`ContextBuilder::turn_attachments` 的缺省为空（没有拆开的实现在 `build` 里给出全部 attachment）。
#[tokio::test]
async fn turn_attachments_default_to_empty() {
    use crate::env::{EnvPath, WorkspaceRef};
    use crate::ids::{EnvironmentId, SessionId};
    use crate::model::QuerySource;
    use crate::orchestration::{ContextBuilder, ContextRequest};
    use crate::permission::PermissionMode;
    let context = crate::testing::FixedContext(Vec::new());
    let request = ContextRequest {
        capabilities: None,
        session: SessionId::new("sess_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        agent: None,
        source: QuerySource::MainThread,
        mode: PermissionMode::Default,
        tool_names: Vec::new(),
        selection: crate::testing::selection(),
        plan_flags: Default::default(),
        mcp: None,
    };
    let attachments = context.turn_attachments(request, &[]).await.unwrap();
    assert!(attachments.is_empty());
}

/// F24：PDF 的 document 块与执行环境的两项 PDF 操作（docs/specs/rust-cli/tools-files.md §1.5）。
#[test]
fn pdf_documents_and_dtos_round_trip() {
    use crate::env::EnvPath;
    use crate::env_pdf::{PdfPageCount, PdfRender, RenderPdfPages};
    use crate::message::{ImageSource, ToolResultContent};
    let document = ToolResultContent::Document {
        source: ImageSource::Base64 {
            media_type: "application/pdf".to_owned(),
            data: "JVBERi0=".to_owned(),
        },
    };
    assert_eq!(
        serde_json::to_value(&document).unwrap(),
        json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERi0="}})
    );
    round_trip(&document);
    round_trip(&PdfPageCount {
        path: EnvPath::new("/w/doc.pdf"),
    });
    round_trip(&RenderPdfPages {
        path: EnvPath::new("/w/doc.pdf"),
        first_page: 1,
        last_page: Some(2),
    });
    round_trip(&PdfRender::Unavailable);
    round_trip(&PdfRender::Failed {
        exit_code: Some(99),
        stderr: "Syntax Error: Couldn't read xref table".to_owned(),
    });
    round_trip(&PdfRender::Rendered {
        pages: vec![vec![0xff, 0xd8, 0xff]],
    });
}
