//! ← 项目页与 `/resume` 页的集成测试共用（修复线 F29）：在内存存储里预先放几个目录的会话，信任检查的替身。
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, OwnerEpoch, SessionId, TraceId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionKind, SessionMeta, SessionStore};
use rust_cli_kernel_api::testing::MemoryStore;
use rust_cli_tui::trust::TrustPrompt;

/// 一个预先放进库里的会话：首条输入与一条回复（`reply` 为空时没有回复：被中断的会话），给定的创建与活动时间（毫秒）。
pub struct Seed {
    pub id: &'static str,
    pub root: &'static str,
    pub title: &'static str,
    pub prompt: &'static str,
    pub reply: &'static str,
    pub created: u64,
    pub updated: u64,
}

pub async fn seed(store: &MemoryStore, seeds: &[Seed]) {
    for seed in seeds {
        let id = SessionId::new(seed.id);
        store
            .create(SessionMeta {
                id: id.clone(),
                kind: SessionKind::Interactive,
                parent: None,
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new("local"),
                    root: EnvPath::new(seed.root),
                },
                title: seed.title.to_owned(),
                trace: TraceId::new(seed.id),
                created_at_ms: seed.created,
            })
            .await
            .unwrap();
        let user = Message::User(UserMessage {
            id: MessageId::new(format!("msg_u_{}", seed.id)),
            content: vec![ContentBlock::Text {
                text: seed.prompt.to_owned(),
            }],
            origin: UserOrigin::Human,
        });
        let mut facts = vec![Fact::MessageAppended { message: user }];
        if !seed.reply.is_empty() {
            facts.push(Fact::MessageAppended {
                message: Message::Assistant(AssistantMessage {
                    id: MessageId::new(format!("msg_a_{}", seed.id)),
                    content: vec![ContentBlock::Text {
                        text: seed.reply.to_owned(),
                    }],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: None,
                    model: "scripted".to_owned(),
                }),
            });
        }
        store
            .commit(
                &id,
                FactBatch {
                    owner_epoch: OwnerEpoch::LOCAL,
                    command: None,
                    facts,
                    turn: None,
                },
            )
            .await
            .unwrap();
        store.set_updated(&id, seed.updated);
    }
}

/// 信任检查的替身：`untrusted` 里的目录要问；记下同意过的目录。
#[derive(Default)]
pub struct Trust {
    pub untrusted: Vec<&'static str>,
    pub accepted: Mutex<Vec<String>>,
}

impl rust_cli_tui::services::WorkspaceTrust for Trust {
    fn check(&self, workspace: WorkspaceRef) -> BoxFuture<'static, Option<TrustPrompt>> {
        let root = workspace.root.as_str().to_owned();
        let ask = self.untrusted.contains(&root.as_str())
            && !self.accepted.lock().unwrap().contains(&root);
        async move {
            ask.then_some(TrustPrompt {
                path: root,
                backstop: false,
                grants: None,
            })
        }
        .boxed()
    }

    fn accept(&self, workspace: WorkspaceRef) -> BoxFuture<'static, ()> {
        self.accepted
            .lock()
            .unwrap()
            .push(workspace.root.as_str().to_owned());
        async {}.boxed()
    }
}

pub fn services(trust: &Arc<Trust>) -> rust_cli_tui::Services {
    rust_cli_tui::Services {
        trust: Some(trust.clone()),
        ..rust_cli_tui::Services::default()
    }
}

/// 有 `~/project` 与 `~/other` 两个目录的执行环境（`/export` 与 `/cd` 要它）。
pub fn env() -> Arc<rust_cli_kernel_api::testing::MemoryEnvironment> {
    Arc::new(
        rust_cli_kernel_api::testing::MemoryEnvironment::new(rust_cli_kernel_api::env::EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".into(),
            os_version: "x".into(),
            shell: "zsh".into(),
            is_git_repo: false,
            additional_directories: Vec::new(),
            home: Some(EnvPath::new("/Users/dev")),
        })
        .with_file("/Users/dev/project/AGENTS.md", "project rules")
        .with_file("/Users/dev/other/AGENTS.md", "other rules"),
    )
}
