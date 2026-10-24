//! node-store 的互操作辅助程序（docs/specs/rust-cli/node-store.md §6），由
//! `scripts/rust-cli/node-interop-check.mjs` 调用：
//!
//! - `write <storage-dir>`：用 NodeStore 写三段脚本化的会话（第二段带回退切点，第三段带 attachment、
//!   工具调用与结果、中断、meta 消息、未结的操作与待回答的交互；第四段带压缩边界；第五段带文件
//!   checkpoint 与对话回退），输出会话 id；
//! - `read <storage-dir> <session>`：输出一个会话的冷读取结果（`LoadedSession`）JSON；
//! - `list <storage-dir> <workspace>`：输出该目录的会话列表 JSON；
//! - `fork <storage-dir> <session> <through>`：分出新会话，复制到 `through`（`/fork` 按用户输入分叉的记录，`node_interop_parts/fork.rs`）；
//! - `usage-write`、`usage-side`、`metering`、`activity`、`request-stats`：用量表与 `/usage`、footer 的查询（`node_interop_parts/usage.rs`）；
//! - `compat-*`、`task-load`、`task-add`：与 Node 共用数据的兼容（`node_interop_parts/compat.rs`）；
//! - `manage-write`、`rename`、`delete`、`query`：会话的改名、删除与项目页的查询（修复线 F29，`node_interop_parts/manage.rs`）；
//! - `sentinel-write <storage-dir> <timing>`：续写占位落库的会话，`continue` 与新输入同一批、`open` 单独一批（工作线
//!   F77、F79，`node_interop_parts/sentinel.rs`）；
//! - `tool-order-write`：结果之间夹着附加上下文、补了中断结果的会话（工作线 F81，`node_interop_parts/tool_order.rs`）。
//!
//! 会话库是 `<storage-dir>/cli/db/db.sqlite`，artifacts 在 `<storage-dir>/cli/artifacts`。
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    CommandId, EnvironmentId, MessageId, OperationId, OwnerEpoch, SessionId, ToolUseId, TraceId,
};
use rust_cli_kernel_api::message::{
    ATTACHMENT_BLOCKS, AssistantMessage, AttachmentMessage, ContentBlock, ImageSource, Message,
    StopReason, ToolResultContent, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{
    Fact, FactBatch, RevertCursor, SessionFilter, SessionKind, SessionMeta, SessionStore,
    TurnAnchor,
};
use rust_cli_node_store::{LOCAL_ENVIRONMENT, NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use serde_json::{Value, json};

const WORKSPACE: &str = "/w/interop";

#[path = "node_interop_parts/checkpoint.rs"]
mod checkpoint;
#[path = "node_interop_parts/compact.rs"]
mod compact;
#[path = "node_interop_parts/compat.rs"]
mod compat;
#[path = "node_interop_parts/fork.rs"]
mod fork;
#[path = "node_interop_parts/manage.rs"]
mod manage;
#[path = "node_interop_parts/sentinel.rs"]
mod sentinel;
#[path = "node_interop_parts/settings.rs"]
mod settings;
#[path = "node_interop_parts/tool_order.rs"]
mod tool_order;
#[path = "node_interop_parts/usage.rs"]
mod usage;

fn store(dir: &str) -> Result<NodeStore> {
    let root = Path::new(dir);
    let db = root.join(rust_cli_paths::DEFAULT_SESSION_DB);
    let paths = StoragePaths::resolve(
        &StorageSettings {
            session_db_path: Some(db.to_str().context("storage dir is not UTF-8")?),
            storage_dir: Some(dir),
        },
        root,
        root,
    );
    // 会话行的版本由入口给（docs/specs/rust-cli/version.md §4）；这里用一个 release 形状的代表值。
    Ok(NodeStore::new(paths, Arc::new(ids())).with_version("0.1.1"))
}

/// 与入口相同的时钟与 ID（node-store 的 `SystemClock` / `NodeIds`；F7 之前这里另有 36 进制、uuid 与系统时间的拷贝）。
fn ids() -> NodeIds {
    NodeIds::new(Arc::new(SystemClock::utc()))
}

fn now_ms() -> u64 {
    ids().now_ms()
}

/// Node `createMessageId` 的格式。
fn message_id() -> MessageId {
    ids().message()
}

fn batch(command: Option<&str>, facts: Vec<Fact>) -> FactBatch {
    FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: command.map(CommandId::new),
        facts,
        turn: None,
    }
}

/// 带回合锚点的批次（会话 actor 的写法，node-store.md §4，修复线 F6）：`current` 是这个会话的当前轮，随之更新。
fn anchored(current: &mut Option<TurnAnchor>, facts: Vec<Fact>) -> FactBatch {
    let turn = TurnAnchor::after(current.as_ref(), &facts, || ids().turn());
    current.clone_from(&turn);
    FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None,
        facts,
        turn,
    }
}

fn user(id: &MessageId, text: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: id.clone(),
            content: vec![ContentBlock::Text { text: text.into() }],
            origin: UserOrigin::Human,
        }),
    }
}

fn reply(id: &MessageId, content: Vec<ContentBlock>, stop: StopReason) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: id.clone(),
            content,
            model: "m".into(),
            stop_reason: Some(stop),
            usage: Some(Usage {
                input_tokens: 12,
                output_tokens: 7,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 4,
            }),
        }),
    }
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

async fn create(store: &NodeStore) -> Result<SessionId> {
    let id = ids().session();
    store
        .create(SessionMeta {
            id: id.clone(),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
                root: EnvPath::new(WORKSPACE),
            },
            title: String::new(),
            trace: TraceId::new(id.as_str()),
            created_at_ms: now_ms(),
        })
        .await?;
    let selection = ModelSelection {
        provider: "p".into(),
        model: "m".into(),
        reasoning: Some("high".into()),
        connection: None,
    };
    let facts = vec![
        Fact::ModelSelected { selection },
        Fact::ModeChanged {
            mode: PermissionMode::AcceptEdits,
        },
    ];
    store.commit(&id, batch(None, facts)).await?;
    Ok(id)
}

async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let first = create(&store).await?;
    let (u1, a1) = (message_id(), message_id());
    store
        .commit(
            &first,
            batch(Some("cmd_1"), vec![user(&u1, "Hello from rust-cli")]),
        )
        .await?;
    let thinking = ContentBlock::Thinking {
        thinking: "Thinking it over".into(),
        signature: Some("sig-interop".into()),
    };
    let answer = vec![thinking, text("Hi! "), text("I am rust-cli.")];
    store
        .commit(
            &first,
            batch(None, vec![reply(&a1, answer, StopReason::EndTurn)]),
        )
        .await?;
    let (u2, a2) = (message_id(), message_id());
    let facts = vec![
        Fact::ModeChanged {
            mode: PermissionMode::DontAsk,
        },
        user(&u2, "Second question"),
        reply(&a2, vec![text("Second answer")], StopReason::MaxTokens),
        Fact::Entry {
            kind: "probe".into(),
            id: "1".into(),
            data: json!({"ok": true}),
        },
    ];
    store.commit(&first, batch(Some("cmd_2"), facts)).await?;
    // “总是允许”与生成的标题（node_interop_parts/settings.rs）。
    settings::write(&store, &first).await?;

    let second = create(&store).await?;
    let ids: Vec<MessageId> = (0..6).map(|_| message_id()).collect();
    let facts = vec![
        user(&ids[0], "one"),
        reply(&ids[1], vec![text("first")], StopReason::EndTurn),
        user(&ids[2], "two"),
        reply(&ids[3], vec![text("second")], StopReason::EndTurn),
    ];
    store.commit(&second, batch(None, facts)).await?;
    let cursor = RevertCursor {
        target: ids[2].clone(),
        kept: vec![ids[0].clone(), ids[1].clone()],
        cut_after: ids[3].clone(),
        generation: 1,
    };
    let facts = vec![
        Fact::RevertSet { cursor },
        user(&ids[4], "retry two"),
        reply(&ids[5], vec![text("second again")], StopReason::EndTurn),
    ];
    store.commit(&second, batch(None, facts)).await?;
    let third = tools(&store).await?;
    let fourth = compact::compacted(&store).await?;
    let fifth = checkpoint::checkpointed(&store).await?;
    print(&json!({
        "workspace": WORKSPACE,
        "sessions": [first, second, third, fourth, fifth],
        "checkpointFile": checkpoint::CHECKPOINT_FILE,
        "generatedTitle": settings::GENERATED_TITLE,
    }))
}

fn attachment(source: &str, text: &str, data: Value) -> Fact {
    Fact::MessageAppended {
        message: Message::Attachment(AttachmentMessage {
            id: message_id(),
            source: source.into(),
            text: text.into(),
            data,
        }),
    }
}

fn user_with(content: Vec<ContentBlock>, origin: UserOrigin) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: message_id(),
            content,
            origin,
        }),
    }
}

fn call(id: &str, name: &str, input: &Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: name.into(),
        input: input.clone(),
    }
}

fn result(owner: &MessageId, call: &str, content: Vec<ToolResultContent>, error: bool) -> Fact {
    let block = ContentBlock::ToolResult {
        tool_use_id: ToolUseId::new(call),
        content,
        is_error: error,
    };
    let origin = UserOrigin::ToolResult {
        assistant: owner.clone(),
    };
    user_with(vec![block], origin)
}

/// query 执行器的意图（fingerprint 是 `{"tool", "input"}` JSON）。
fn intent(operation: &str, tool: &str, input: &Value) -> Fact {
    Fact::OperationIntent {
        operation: OperationId::new(operation),
        fingerprint: json!({"tool": tool, "input": input}).to_string(),
    }
}

fn said(text: &str) -> ToolResultContent {
    ToolResultContent::Text { text: text.into() }
}

/// 第三段会话：attachment、两个并发工具（后声明的先结束，结果带图片）、被中断的工具、meta 消息；
/// 最后留一个没有 assistant 的意图（进程在流式响应中退出）和一个没回答的计划审批。
async fn tools(store: &NodeStore) -> Result<SessionId> {
    let id = create(store).await?;
    let (read, glob, bash) = (
        json!({"file_path": "/w/interop/a.png"}),
        json!({"pattern": "*.txt"}),
        json!({"command": "sleep 60"}),
    );
    // Read 整读 PDF 的结果（tools-files.md §1.5，修复线 F24）：说明一行加 document 块，Node 读成 PDF 的 file 块。
    let read_pdf = json!({"file_path": "/w/interop/doc.pdf"});
    let document = ToolResultContent::Document {
        source: ImageSource::Base64 {
            media_type: "application/pdf".into(),
            data: "JVBERi0xLjQKJSVFT0YK".into(),
        },
    };
    let (a1, a2, a3) = (message_id(), message_id(), message_id());
    let thinking = ContentBlock::Thinking {
        thinking: "Look around".into(),
        signature: Some("sig-tools".into()),
    };
    let image = ToolResultContent::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
    };
    // 人类输入带一张粘贴的图片（tui-input.md §3.2）：Node 读出同一张图。
    let pasted = ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
    };
    let facts = vec![
        user_with(
            vec![text("List the files [Image #1]"), pasted.clone()],
            UserOrigin::Human,
        ),
        // 输入那一步的当步 attachment 之后是 UserPromptSubmit 的附加上下文，再是日期（修复线 F10 的落库顺序，
        // query.md §1.1）：Node 按原样读出、续写。
        attachment(
            "total_tokens_reminder",
            "<system-reminder>\n<total_tokens>15000000 tokens left</total_tokens>\n</system-reminder>",
            Value::Null,
        ),
        attachment(
            "hook_additional_context",
            "<system-reminder>\nUserPromptSubmit hook additional context: prompt note\n</system-reminder>",
            json!({"hookName": "UserPromptSubmit", "hookEvent": "UserPromptSubmit", "content": ["prompt note"]}),
        ),
        attachment(
            "date",
            "<system-reminder>\nToday's date is 2026-10-17.\n</system-reminder>",
            json!({"date": "2026-10-17"}),
        ),
        attachment(
            "remote_session_change",
            "<system-reminder>\nAttribution changed.\n</system-reminder>",
            Value::Null,
        ),
        attachment(
            "max_turns_reached",
            "",
            json!({"maxTurns": 3, "turnCount": 3}),
        ),
        // `@` 提及的图片、带图片输出的 notebook 与长 PDF（tui-input.md §1.3）：内容块写成 text part 之后的 part，
        // Node 读出同样的文字与图片。
        attachment(
            "file",
            "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/w/interop/pic.png\"}\n</system-reminder>",
            json!({"filename": "/w/interop/pic.png", "displayPath": "pic.png", ATTACHMENT_BLOCKS: [pasted.clone()]}),
        ),
        attachment(
            "file",
            "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/w/interop/plot.ipynb\"}\n</system-reminder>",
            json!({"filename": "/w/interop/plot.ipynb", "displayPath": "plot.ipynb", ATTACHMENT_BLOCKS: [
                text("<system-reminder>\n<cell id=\"p\">show()</cell id=\"p\">\n</system-reminder>"),
                pasted.clone(),
                text("<system-reminder>\n<cell id=\"q\"><cell_type>markdown</cell_type>done</cell id=\"q\">\n</system-reminder>"),
            ]}),
        ),
        attachment(
            "pdf_reference",
            "<system-reminder>\nPDF file: /w/interop/big.pdf (page count unknown, 1MB). It was not attached because it may be too long.\n</system-reminder>",
            json!({"filename": "/w/interop/big.pdf", "pageCount": null, "fileSize": 1_100_000, "displayPath": "big.pdf"}),
        ),
        intent("op_1", "Read", &read),
        intent("op_2", "Glob", &glob),
        intent("op_5", "Read", &read_pdf),
        reply(
            &a1,
            vec![
                thinking,
                text("Looking."),
                call("toolu_1", "Read", &read),
                call("toolu_2", "Glob", &glob),
                call("toolu_5", "Read", &read_pdf),
            ],
            StopReason::ToolUse,
        ),
        result(&a1, "toolu_2", vec![said("a.txt\nb.txt")], false),
        result(&a1, "toolu_1", vec![said("An image:"), image], false),
        result(
            &a1,
            "toolu_5",
            vec![
                said("PDF file read: /w/interop/doc.pdf (14 bytes)"),
                document,
            ],
            false,
        ),
        reply(&a2, vec![text("Two files.")], StopReason::EndTurn),
    ];
    store.commit(&id, batch(Some("cmd_t1"), facts)).await?;
    let reject = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";
    let facts = vec![
        user(&message_id(), "Run it"),
        intent("op_3", "Bash", &bash),
        reply(
            &a3,
            vec![call("toolu_3", "Bash", &bash)],
            StopReason::ToolUse,
        ),
        result(&a3, "toolu_3", vec![said(reject)], true),
        user_with(
            vec![text("[Request interrupted by user for tool use]")],
            UserOrigin::Human,
        ),
        user_with(
            vec![text("Output token limit hit."), text("Resume directly.")],
            UserOrigin::Meta {
                source: "max_output_tokens_recovery".into(),
            },
        ),
        intent("op_4", "Bash", &json!({"command": "make"})),
        Fact::Entry {
            kind: "pending_interaction".into(),
            id: "ia_1".into(),
            data: serde_json::to_value(InteractionRequest::PlanApproval {
                tool_use: ToolUseId::new("toolu_9"),
                plan: "Ship it".into(),
            })?,
        },
    ];
    store.commit(&id, batch(Some("cmd_t2"), facts)).await?;
    Ok(id)
}

#[expect(clippy::print_stdout, reason = "辅助程序的输出就是 stdout 上的 JSON")]
fn print(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn run(args: &[String]) -> Result<()> {
    match args {
        [command, dir] if command == "write" => write(dir).await,
        [command, dir, session] if command == "read" => {
            let loaded = store(dir)?.load(&SessionId::new(session.as_str())).await?;
            print(&serde_json::to_value(loaded)?)
        }
        [command, dir, workspace] if command == "rules" => settings::rules(dir, workspace).await,
        [command, dir, workspace, change] if command == "rules-change" => {
            settings::rules_change(dir, workspace, change).await
        }
        [command, dir, session, through] if command == "fork" => {
            fork::fork(dir, session, through).await
        }
        [command, dir] if command == "usage-write" => usage::write(dir).await,
        [command, dir] if command == "usage-side" => usage::side(dir).await,
        [command, dir] if command == "manage-write" => manage::write(dir).await,
        [command, dir, timing] if command == "sentinel-write" => sentinel::write(dir, timing).await,
        [command, dir] if command == "tool-order-write" => tool_order::write(dir).await,
        [command, dir, session, title] if command == "rename" => {
            manage::rename(dir, session, title).await
        }
        [command, dir, session] if command == "delete" => manage::delete(dir, session).await,
        [command, dir] if command == "query" => manage::query(dir, None).await,
        [command, dir, root] if command == "query" => manage::query(dir, Some(root)).await,
        [command, dir] if command == "compat-write" => compat::write(dir).await,
        [command, dir, session] if command == "compat-checkpoints" => {
            compat::checkpoints(dir, session).await
        }
        [command, dir, session] if command == "compat-scope" => compat::scope(dir, session).await,
        [command, dir, session] if command == "task-load" => compat::task_load(dir, session).await,
        [command, dir, session, subject] if command == "task-add" => {
            compat::task_add(dir, session, subject).await
        }
        [command, dir, session] if command == "metering" => usage::metering(dir, session).await,
        [command, dir, session] if command == "request-stats" => {
            usage::request_stats(dir, session).await
        }
        [command, dir, range, today, offset] if command == "activity" => {
            usage::activity(dir, range, today, offset).await
        }
        [c, rest @ ..] if c.starts_with("mode") => settings::mode(c, rest).await,
        [command, dir, workspace] if command == "list" => {
            let filter = SessionFilter {
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
                    root: EnvPath::new(workspace.as_str()),
                },
                limit: 50,
            };
            print(&serde_json::to_value(store(dir)?.list(filter).await?)?)
        }
        _ => {
            bail!(
                "usage: node_interop write <dir> | read <dir> <session> | list <dir> <workspace> | rules <dir> <workspace>"
            )
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "互操作辅助程序（不是产品代码）读自己的命令行参数"
)]
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(&args))
}
