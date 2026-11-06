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
            machine: Default::default(),
        }],
        code: true,
    });
}

/// F98（remote-exec.md §25.4.5、§25.7）：远端的文件改动带机器；本机的不写这个字段，旧形状照常能读。
#[test]
fn file_changes_carry_their_machine_only_when_remote() {
    let remote = crate::session_api::FileChange {
        path: "/srv/proj/README.md".to_owned(),
        additions: 1,
        deletions: 1,
        machine: crate::ids::MachineName::new("dev-a"),
    };
    round_trip(&remote);
    assert_eq!(serde_json::to_value(&remote).unwrap()["machine"], "dev-a");
    let local: crate::session_api::FileChange = serde_json::from_value(
        serde_json::json!({"path": "/work/a.txt", "additions": 1, "deletions": 0}),
    )
    .unwrap();
    assert_eq!(local.machine, Default::default());
    assert!(
        serde_json::to_value(&local)
            .unwrap()
            .get("machine")
            .is_none()
    );
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

/// 不持有会话时（`testing::NoSession`，工作线 F134 以前是 `Interactions` 的缺省体）：不改目录、顶层会话是自己；没有
/// 定时任务。
#[tokio::test]
async fn l11b_defaults() {
    use crate::cron::{CronSignals, NoCron};
    use crate::orchestration::SessionOps;
    use crate::testing::NoSession;
    let session = crate::ids::SessionId::new("sess_1");
    assert_eq!(NoSession.owner_session(&session).await, session);
    assert!(
        NoSession
            .set_workspace(crate::env::EnvPath::new("/Users/dev"))
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
    round_trip(&crate::session_api::Command::RenameRecorded {
        title: "Mine".to_owned(),
        record: crate::user_command::LocalCommandRecord {
            name: "rename".to_owned(),
            args: "Mine".to_owned(),
            output: crate::user_command::LocalCommandOutput::Stdout(
                "Session renamed to: Mine".to_owned(),
            ),
            caveat: false,
            meta: vec!["The user named this session \"Mine\".".to_owned()],
        },
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
        reasoning: Some("high".to_owned()),
        ..crate::model::ModelSelection::new(
            "anthropic".to_owned(),
            rust_cli_base::models::GLM_5_3.to_owned(),
        )
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

/// 测试的输出端只并入记下的插话，没有通知（工作线 F134：`RunSink` 只有 `take_absorbed`，以前另有缺省为空的
/// `take_notices`）。
#[tokio::test]
async fn the_recording_sink_absorbs_only_its_steering() {
    use crate::orchestration::{Absorbed, RunSink};
    let sink = crate::testing::RecordingSink::default();
    sink.steer("also b");
    let absorbed = sink.take_absorbed().await;
    assert!(
        matches!(&absorbed[..], [Absorbed::Prompt(steered)] if steered.text == "also b"),
        "{absorbed:?}"
    );
    assert!(sink.take_absorbed().await.is_empty());
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

/// 修复线 F2 的事实：`SubagentLinked` 往返；内存存储里没有群协作会话，会话自己的规则集为 None（规则在项目规则集）。
#[tokio::test]
async fn f2_subagent_link_round_trips_and_session_rules_default_to_none() {
    use crate::store::LocalSettings;
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
    let mut request = QueryRequest::new(
        SessionId::new("sess_1"),
        RunId::new("run_1"),
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        Vec::new(),
        ModelSelection::new(
            "anthropic".to_owned(),
            rust_cli_base::models::GLM_5_3.to_owned(),
        ),
        PermissionMode::Default,
        QuerySource::MainThread,
    );
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
    let request = ContextRequest::new(
        SessionId::new("sess_1"),
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        QuerySource::MainThread,
        PermissionMode::Default,
        crate::testing::selection(),
    );
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

/// 读过的文件被改动的结果与调用过的 skill：字段名与序列化往返。
#[test]
fn changed_files_and_invoked_skills_round_trip() {
    let changed = crate::tool::ChangedFile {
        path: "/w/a.txt".to_owned(),
        snippet: "1\tnew".to_owned(),
    };
    let value = serde_json::to_value(&changed).expect("json");
    assert_eq!(
        value,
        serde_json::json!({"path": "/w/a.txt", "snippet": "1\tnew"})
    );
    assert_eq!(
        serde_json::from_value::<crate::tool::ChangedFile>(value).expect("back"),
        changed
    );
    let skill = crate::listing::InvokedSkill {
        name: "deploy".to_owned(),
        path: "projectSettings:deploy".to_owned(),
        content: "Deploy it.".to_owned(),
    };
    let value = serde_json::to_value(&skill).expect("json");
    assert_eq!(
        serde_json::from_value::<crate::listing::InvokedSkill>(value).expect("back"),
        skill
    );
}

/// 续写时补的 assistant：模型名与文字都对上才算。
#[test]
fn the_resume_sentinel_is_recognized() {
    use crate::message::{AssistantMessage, ContentBlock, NO_RESPONSE_REQUESTED, SYNTHETIC_MODEL};
    let sentinel = |model: &str, text: &str| AssistantMessage {
        id: crate::ids::MessageId::new("a"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        model: model.to_owned(),
        stop_reason: None,
        usage: None,
    };
    assert!(sentinel(SYNTHETIC_MODEL, NO_RESPONSE_REQUESTED).is_resume_sentinel());
    assert!(!sentinel(rust_cli_base::models::GLM_5, NO_RESPONSE_REQUESTED).is_resume_sentinel());
    assert!(!sentinel(SYNTHETIC_MODEL, "Prompt is too long").is_resume_sentinel());
}

/// 操作上下文的工厂（工作线 F134）：本机 owner、这个实例的 generation；`next` 从共用的 Ids 取新的操作 ID（与别的 ID
/// 同一个序列），`with_id` 用给定的 ID。
#[test]
fn operations_build_local_contexts_from_the_shared_ids() {
    use std::sync::Arc;

    use crate::clock::Ids;
    use crate::env::{Operations, Origin};
    use crate::ids::{EnvironmentId, Generation, OperationId, OwnerEpoch, SessionId};

    let ids: Arc<dyn Ids> = Arc::new(crate::testing::SequentialIds::default());
    let operations = Operations::new(Generation::new("g1"), ids.clone());
    let origin = Origin::User {
        session: Some(SessionId::new("sess_1")),
    };
    let first = operations.next(EnvironmentId::new("local"), origin.clone());
    let message = ids.message();
    let second = operations.next(EnvironmentId::new("local"), origin.clone());
    assert_eq!(
        (first.generation.as_str(), first.owner_epoch),
        ("g1", OwnerEpoch::LOCAL)
    );
    // 同一个序列：操作 ID 与消息 ID 交替取号。
    assert_eq!(
        (
            first.operation.as_str(),
            message.as_str(),
            second.operation.as_str()
        ),
        ("op_1", "msg_2", "op_3")
    );
    let given = operations.with_id(
        EnvironmentId::new("local"),
        OperationId::new("op_x"),
        origin,
    );
    assert_eq!(given.operation, OperationId::new("op_x"));
}
