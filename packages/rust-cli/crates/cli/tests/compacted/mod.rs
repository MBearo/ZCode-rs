//! 压缩过几次的会话（docs/specs/rust-cli/compact.md §5.1）：经 NodeStore 的公开接口写进临时 HOME 下的会话库，ID 固定，
//! 给续写的测试与冷打开的测量用。
//!
//! 每一轮是 `u<i>`（输入）、`t<i>`（回复带一个工具调用）、`r<i>`（工具结果）、`a<i>`（回复正文）；每隔几轮的第 3、6、9 轮附带
//! 一条 attachment `x<i>`。第 k 次压缩（k 从 1 起）在 `after[k-1].0` 那一轮之后，保留段按 [`Keep`]。
//!
//! 压缩写成边界 `b<k>`、摘要 `s<k>`（`SUMMARY-<k>`）与一个文件 attachment `f<k>`，同一批提交。
#![allow(dead_code, reason = "不同的测试文件用到的辅助函数不同")]

use std::path::Path;
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, MessageId, OwnerEpoch, SessionId, ToolUseId, TraceId,
};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, CompactBoundary, CompactTrigger, ContentBlock, Message,
    PreservedSegment, StopReason, SystemMessage, ToolResultContent, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionKind, SessionMeta, SessionStore};
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use serde_json::json;

/// 会话 ID。
pub const SESSION: &str = "ses_f102compacted";

/// HOME 下的默认会话库（与入口的解析相同）。
pub fn store(home: &Path) -> NodeStore {
    let paths = StoragePaths::resolve(
        &StorageSettings {
            session_db_path: None,
            storage_dir: None,
        },
        home,
        home,
    );
    let clock = Arc::new(SystemClock::utc());
    let ids = Arc::new(NodeIds::new(clock));
    NodeStore::new(paths, ids).with_version("0.1.0")
}

fn id(prefix: &str, n: usize) -> MessageId {
    MessageId::new(format!("msg_{prefix}{n:05}"))
}

fn usage(n: usize) -> Option<Usage> {
    Some(Usage {
        input_tokens: 1000 + n as u64,
        output_tokens: 20,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
    })
}

fn user(n: usize) -> Message {
    Message::User(UserMessage {
        id: id("u", n),
        content: vec![ContentBlock::Text {
            text: format!("question {n}"),
        }],
        origin: UserOrigin::Human,
    })
}

fn call(n: usize) -> Message {
    Message::Assistant(AssistantMessage {
        id: id("t", n),
        content: vec![
            ContentBlock::Text {
                text: format!("looking {n}"),
            },
            ContentBlock::ToolUse {
                id: ToolUseId::new(format!("toolu_{n:05}")),
                name: "Read".to_owned(),
                input: json!({"file_path": format!("/w/f{n}.txt")}),
            },
        ],
        model: "m".to_owned(),
        stop_reason: Some(StopReason::ToolUse),
        usage: usage(n),
    })
}

fn result(n: usize) -> Message {
    Message::User(UserMessage {
        id: id("r", n),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(format!("toolu_{n:05}")),
            content: vec![ToolResultContent::Text {
                text: format!("     1\tcontent {n}"),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: id("t", n),
        },
    })
}

fn note(n: usize) -> Message {
    Message::Attachment(AttachmentMessage {
        id: id("x", n),
        source: "nested_memory".to_owned(),
        text: format!("<system-reminder>\nnote {n}\n</system-reminder>"),
        data: json!({"path": format!("/w/n{n}.md")}),
    })
}

fn answer(n: usize) -> Message {
    Message::Assistant(AssistantMessage {
        id: id("a", n),
        content: vec![ContentBlock::Text {
            text: format!("answer {n}"),
        }],
        model: "m".to_owned(),
        stop_reason: Some(StopReason::EndTurn),
        usage: usage(n),
    })
}

fn compaction(k: usize, preserved: Option<(MessageId, MessageId)>) -> Vec<Message> {
    let summary = id("s", k);
    vec![
        Message::System(SystemMessage::CompactBoundary(CompactBoundary {
            id: id("b", k),
            trigger: if k.is_multiple_of(2) {
                CompactTrigger::Auto
            } else {
                CompactTrigger::Manual
            },
            pre_tokens: 5000 + k as u64,
            summary: summary.clone(),
            preserved: preserved.map(|(head, tail)| PreservedSegment {
                head,
                anchor: summary.clone(),
                tail,
            }),
            discovered_tools: Vec::new(),
        })),
        Message::User(UserMessage {
            id: summary,
            content: vec![ContentBlock::Text {
                text: format!(
                    "This session is being continued from a previous conversation that ran out of context. \
                     The summary below covers the earlier portion of the conversation.\n\nSummary:\nSUMMARY-{k}"
                ),
            }],
            origin: UserOrigin::CompactSummary,
        }),
        Message::Attachment(AttachmentMessage {
            id: id("f", k),
            source: "file".to_owned(),
            text: format!(
                "<system-reminder>\nCalled the Read tool with the following input: {{\"file_path\":\"/w/f{k}.txt\"}}\n</system-reminder>"
            ),
            data: json!({"filename": format!("/w/f{k}.txt")}),
        }),
    ]
}

/// 一次压缩的保留段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// 保留这一轮的调用、结果与正文（尾是正文）。
    Answer,
    /// 在这一轮的工具结果之后压缩：保留调用与结果（尾是工具结果），正文在压缩之后才到。
    Result,
    /// 没有保留段。
    Nothing,
}

fn batch(facts: Vec<Fact>) -> FactBatch {
    FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: None,
        facts,
        turn: None,
    }
}

fn appended(messages: Vec<Message>) -> Vec<Fact> {
    messages
        .into_iter()
        .map(|message| Fact::MessageAppended { message })
        .collect()
}

/// 写一个 `turns` 轮、在 `after` 列出的各轮之后压缩的会话（工作区是 `root`）。返回写入的消息数（不含边界）。
pub async fn seed(store: &NodeStore, root: &Path, turns: usize, after: &[(usize, Keep)]) -> usize {
    let session = SessionId::new(SESSION);
    store
        .create(SessionMeta {
            id: session.clone(),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new(rust_cli_node_store::LOCAL_ENVIRONMENT),
                root: EnvPath::new(root.to_str().unwrap()),
            },
            title: "Compacted work".to_owned(),
            trace: TraceId::new(SESSION),
            created_at_ms: 1_000_000,
        })
        .await
        .unwrap();
    let setup = vec![
        Fact::ModelSelected {
            selection: ModelSelection {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                reasoning: None,
                connection: None,
            },
        },
        Fact::ModeChanged {
            mode: PermissionMode::Default,
        },
    ];
    store.commit(&session, batch(setup)).await.unwrap();
    let mut count = 0;
    let commit = |messages: Vec<Message>| {
        let facts = appended(messages);
        let session = session.clone();
        async move {
            store.commit(&session, batch(facts)).await.unwrap();
        }
    };
    for n in 1..=turns {
        let k = after
            .iter()
            .position(|&(turn, _)| turn == n)
            .map(|index| (index + 1, after[index].1));
        let mut first = vec![user(n)];
        if n % 3 == 0 {
            first.push(note(n));
        }
        count += first.len();
        commit(first).await;
        commit(vec![call(n)]).await;
        commit(vec![result(n)]).await;
        count += 2;
        match k {
            Some((k, Keep::Result)) => {
                let messages = compaction(k, Some((id("t", n), id("r", n))));
                count += messages.len() - 1;
                commit(messages).await;
                commit(vec![answer(n)]).await;
                count += 1;
            }
            Some((k, keep)) => {
                commit(vec![answer(n)]).await;
                count += 1;
                let preserved = (keep == Keep::Answer).then(|| (id("t", n), id("a", n)));
                let messages = compaction(k, preserved);
                count += messages.len() - 1;
                commit(messages).await;
            }
            None => {
                commit(vec![answer(n)]).await;
                count += 1;
            }
        }
    }
    count
}

/// 入口同样的部件组成的内核（NodeStore、LocalEnvironment、指向不可达地址的 Anthropic 端点）：打开会话、取视图不调模型。
pub async fn kernel(home: &Path, root: &Path) -> rust_cli_runtime::LocalKernel {
    use rust_cli_base::secret::RedactedString;
    use rust_cli_model::{AnthropicEndpoint, ProviderModel};
    use rust_cli_net::{Egress, NetworkPolicy};

    let clock = Arc::new(SystemClock::utc());
    let ids = Arc::new(NodeIds::new(clock.clone()));
    let paths = StoragePaths::resolve(
        &StorageSettings {
            session_db_path: None,
            storage_dir: None,
        },
        home,
        home,
    );
    let store = Arc::new(NodeStore::new(paths, ids.clone()).with_version("0.1.0"));
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let endpoint = AnthropicEndpoint {
        base_url: "http://127.0.0.1:9".to_owned(),
        api_key: RedactedString::new("sk-test"),
    };
    let ports = rust_cli_runtime::Ports {
        model: Arc::new(ProviderModel::anthropic(egress, endpoint, clock.clone())),
        environments: rust_cli_kernel_api::testing::environments(Arc::new(
            rust_cli_env_local::LocalEnvironment::new(Some(root.to_path_buf())),
        )),
        store: store.clone(),
        artifacts: store,
        clock,
        ids,
    };
    let defaults = rust_cli_runtime::Defaults {
        selection: ModelSelection {
            provider: String::new(),
            model: String::new(),
            reasoning: None,
            connection: None,
        },
        mode: PermissionMode::Default,
        max_turns: None,
        search_tools: false,
        task_tools: None,
        limits: Default::default(),
    };
    rust_cli_runtime::LocalKernel::new(ports, defaults)
}
