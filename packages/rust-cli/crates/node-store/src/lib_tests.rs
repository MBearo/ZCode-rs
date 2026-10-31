//! 端口行为：创建、提交、冷读取、列表、事务原子性、模式与切点、artifact。
use std::sync::atomic::{AtomicU64, Ordering};

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId, MessageId, OwnerEpoch, TraceId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, SystemMessage, Usage, UserMessage,
    UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::Fact;
use rust_cli_paths::StorageSettings;
use serde_json::Value;

use super::*;

/// 固定时钟与顺序 uuid（第 n 个是 `00000000-0000-4000-8000-<n 补足 12 位>`，与 Node 夹具脚本相同）。
pub(crate) struct FixedSource {
    pub now: AtomicU64,
    next: AtomicU64,
}

impl FixedSource {
    pub(crate) fn new(now: u64) -> Arc<Self> {
        Arc::new(Self {
            now: AtomicU64::new(now),
            next: AtomicU64::new(0),
        })
    }

    pub(crate) fn at(&self, now: u64) {
        self.now.store(now, Ordering::SeqCst);
    }
}

impl rust_cli_kernel_api::clock::Clock for FixedSource {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

impl UuidSource for FixedSource {
    fn uuid(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        format!("00000000-0000-4000-8000-{n:012}")
    }
}

/// 固定时钟与顺序 uuid 的 `NodeIds`（内核与记录共用的那一份）。
pub(crate) fn fixed_ids(source: &Arc<FixedSource>) -> Arc<NodeIds> {
    Arc::new(NodeIds::with_uuids(source.clone(), source.clone()))
}

/// 测试库写进会话行的版本：Node 夹具（`fixtures/node-records.json`）按这个版本生成。
pub(crate) const FIXTURE_VERSION: &str = "0.1.0";

pub(crate) struct TestStore {
    pub dir: tempfile::TempDir,
    pub store: NodeStore,
    pub source: Arc<FixedSource>,
}

impl TestStore {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cli/db/db.sqlite");
        let paths = StoragePaths::resolve(
            &StorageSettings {
                session_db_path: db.to_str(),
                storage_dir: dir.path().to_str(),
            },
            dir.path(),
            dir.path(),
        );
        let source = FixedSource::new(1000);
        let store = NodeStore::new(paths, fixed_ids(&source)).with_version(FIXTURE_VERSION);
        Self { dir, store, source }
    }

    /// 另一条连接（模拟 Node 进程）。
    pub(crate) fn connection(&self) -> Connection {
        Connection::open(self.store.paths().database().as_path()).unwrap()
    }

    /// 表的全部行（列按表定义的顺序，值转成 JSON）。
    pub(crate) fn rows(&self, sql: &str) -> Vec<Vec<Value>> {
        let conn = self.connection();
        let mut statement = conn.prepare(sql).unwrap();
        let width = statement.column_count();
        statement
            .query_map([], |row| {
                (0..width)
                    .map(|i| {
                        Ok(match row.get_ref(i)? {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(v) => v.into(),
                            rusqlite::types::ValueRef::Real(v) => v.into(),
                            rusqlite::types::ValueRef::Text(v) => {
                                String::from_utf8_lossy(v).into_owned().into()
                            }
                            rusqlite::types::ValueRef::Blob(_) => panic!("blob column"),
                        })
                    })
                    .collect()
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
}

pub(crate) fn meta(id: &str, root: &str, title: &str) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
            root: EnvPath::new(root),
        },
        title: title.to_owned(),
        trace: TraceId::new(id),
        created_at_ms: 1000,
    }
}

pub(crate) fn batch(command: Option<&str>, facts: Vec<Fact>) -> FactBatch {
    FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: command.map(CommandId::new),
        facts,
        turn: None,
    }
}

pub(crate) fn user(id: &str, text: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text { text: text.into() }],
            origin: UserOrigin::Human,
        }),
    }
}

pub(crate) fn assistant(id: &str, content: Vec<ContentBlock>, usage: Option<Usage>) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content,
            model: "m".into(),
            stop_reason: Some(StopReason::EndTurn),
            usage,
        }),
    }
}

pub(crate) fn text(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

pub(crate) fn selection() -> ModelSelection {
    ModelSelection {
        provider: "p".into(),
        model: "m".into(),
        reasoning: Some("high".into()),
        connection: None,
    }
}

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

pub(crate) async fn started(t: &TestStore, id: &str) {
    t.store.create(meta(id, "/w", "")).await.unwrap();
    let facts = vec![
        Fact::ModelSelected {
            selection: selection(),
        },
        Fact::ModeChanged {
            mode: PermissionMode::Default,
        },
    ];
    t.store.commit(&sid(id), batch(None, facts)).await.unwrap();
}

#[tokio::test]
async fn a_text_turn_reads_back_as_committed() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let user = user("msg_ru_u1", "Hello");
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 5,
        cache_creation_input_tokens: 3,
        cache_read_input_tokens: 2,
    };
    let reply = assistant(
        "msg_rv_a1",
        vec![
            ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: Some("sig".into()),
            },
            ContentBlock::RedactedThinking { data: "x".into() },
            text("Hi"),
        ],
        Some(usage),
    );
    let first = t
        .store
        .commit(&sid("sess_1"), batch(Some("cmd"), vec![user.clone()]))
        .await;
    let second = t
        .store
        .commit(&sid("sess_1"), batch(None, vec![reply.clone()]))
        .await;
    assert_eq!((first.unwrap().sequence, second.unwrap().sequence), (2, 3));
    let loaded = t.store.load(&sid("sess_1")).await.unwrap();
    let message = |fact: Fact| match fact {
        Fact::MessageAppended { message } => message,
        _ => unreachable!(),
    };
    assert_eq!(loaded.messages, vec![message(user), message(reply)]);
    assert_eq!(loaded.model, Some(selection()));
    assert_eq!(loaded.mode, Some(PermissionMode::Default));
    assert_eq!(loaded.meta.title, "Hello");
    assert_eq!(loaded.meta.trace, TraceId::new("sess_1"));
    assert_eq!(loaded.meta.workspace.root, EnvPath::new("/w"));
}

/// 一批事实里任何一条失败（这里是不支持的 system 消息），整批都不落库，回执序号也不前进。
#[tokio::test]
async fn a_failing_fact_rolls_back_the_whole_batch() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let facts = vec![
        user("msg_ru_u1", "Hello"),
        Fact::TitleChanged { title: "x".into() },
        Fact::MessageAppended {
            message: Message::System(SystemMessage::Interrupted {
                id: MessageId::new("msg_ru_s1"),
            }),
        },
    ];
    let error = t.store.commit(&sid("sess_1"), batch(None, facts)).await;
    assert!(
        matches!(error, Err(StoreError::Unsupported { .. })),
        "{error:?}"
    );
    assert!(t.rows("select * from message").is_empty());
    assert!(t.rows("select * from part").is_empty());
    let intents = "select id from session_entry where type = 'rust-cli/operation_intent'";
    assert!(t.rows(intents).is_empty());
    let title = t.rows("select title, title_source from session");
    assert_eq!(title, vec![vec![Value::from(""), Value::from("default")]]);
    let next = t
        .store
        .commit(&sid("sess_1"), batch(None, vec![]))
        .await
        .unwrap();
    assert_eq!(next.sequence, 2);
}

#[tokio::test]
async fn a_repeated_message_id_is_refused() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let facts = vec![user("msg_ru_u1", "a")];
    t.store
        .commit(&sid("sess_1"), batch(None, facts.clone()))
        .await
        .unwrap();
    let again = t.store.commit(&sid("sess_1"), batch(None, facts)).await;
    assert!(
        matches!(again, Err(StoreError::Storage { .. })),
        "{again:?}"
    );
    assert_eq!(t.rows("select id from part").len(), 1);
}

#[tokio::test]
async fn missing_sessions_and_stale_owners_are_refused() {
    let t = TestStore::new();
    let missing = t.store.commit(&sid("sess_x"), batch(None, vec![])).await;
    assert!(matches!(missing, Err(StoreError::NotFound { .. })));
    assert!(matches!(
        t.store.load(&sid("sess_x")).await,
        Err(StoreError::NotFound { .. })
    ));
    started(&t, "sess_1").await;
    let mut stale = batch(None, vec![]);
    stale.owner_epoch = OwnerEpoch(2);
    let stale = t.store.commit(&sid("sess_1"), stale).await;
    assert!(matches!(stale, Err(StoreError::StaleOwner { .. })));
    let assistant_first = assistant("msg_rv_a1", vec![text("x")], None);
    let orphan = t
        .store
        .commit(&sid("sess_1"), batch(None, vec![assistant_first]))
        .await;
    assert!(matches!(orphan, Err(StoreError::Storage { .. })));
    let remote = SessionMeta {
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("remote"),
            root: EnvPath::new("/w"),
        },
        ..meta("sess_2", "/w", "")
    };
    assert!(matches!(
        t.store.create(remote).await,
        Err(StoreError::Unsupported { .. })
    ));
}

/// 本目录的顶层会话与 fork 出的会话，按活动时间倒序（session-lifecycle.md §6.2）；子代理子会话、
/// selection side chat 与其他目录不列出。
#[tokio::test]
async fn sessions_and_forks_are_listed_per_directory() {
    let t = TestStore::new();
    t.store.create(meta("sess_a", "/w", "A")).await.unwrap();
    t.source.at(3000);
    let mut b = meta("sess_b", "/w", "B");
    b.created_at_ms = 3000;
    t.store.create(b).await.unwrap();
    let child = SessionMeta {
        kind: SessionKind::Subagent,
        parent: Some(sid("sess_a")),
        ..meta("sess_c", "/w", "C")
    };
    t.store.create(child).await.unwrap();
    t.store.create(meta("sess_d", "/other", "D")).await.unwrap();
    t.source.at(4000);
    let fork = |id| SessionMeta {
        kind: SessionKind::Fork,
        parent: Some(sid("sess_a")),
        created_at_ms: 4000,
        ..meta(id, "/w", "Fork of A")
    };
    t.store.create(fork("sess_f")).await.unwrap();
    // fork 的标题来源同 Node `buildForkedSessionInput`：generated。
    let titled = t.rows("select title, title_source from session where id = 'sess_f'");
    assert_eq!(
        titled,
        vec![vec![Value::from("Fork of A"), Value::from("generated")]]
    );
    // App 的 selection side chat 冷读取也是 `SessionKind::Fork`，但不进列表。
    t.store.create(fork("sess_g")).await.unwrap();
    t.connection()
        .execute(
            "update session set task_type = 'selection_side_chat' where id = 'sess_g'",
            [],
        )
        .unwrap();
    t.source.at(5000);
    t.store
        .commit(&sid("sess_a"), batch(None, vec![user("msg_3uw_u", "x")]))
        .await
        .unwrap();
    let filter = |limit| SessionFilter {
        workspace: meta("", "/w", "").workspace,
        limit,
    };
    let listed = t.store.list(filter(10)).await.unwrap();
    let ids: Vec<(&str, u64)> = listed
        .iter()
        .map(|s| (s.id.as_str(), s.updated_at_ms))
        .collect();
    assert_eq!(
        ids,
        vec![("sess_a", 5000), ("sess_f", 4000), ("sess_b", 3000)]
    );
    assert_eq!(t.store.list(filter(1)).await.unwrap().len(), 1);
    let row = t.rows("select task_type, parent_id from session where id = 'sess_c'");
    assert_eq!(
        row,
        vec![vec![Value::from("subagent_child"), Value::from("sess_a")]]
    );
}

#[path = "lib_mode_tests.rs"]
mod mode_and_revert;

/// 会话行的 `version` 是入口给的版本（docs/specs/rust-cli/version.md §4）；没给时为空串。
#[tokio::test]
async fn the_session_row_carries_the_version_given_by_the_entry() {
    let t = TestStore::new();
    let paths = t.store.paths().clone();
    let given = NodeStore::new(paths.clone(), fixed_ids(&t.source)).with_version("0.1.42");
    given.create(meta("sess_v1", "/w", "")).await.unwrap();
    NodeStore::new(paths, fixed_ids(&t.source))
        .create(meta("sess_v2", "/w", ""))
        .await
        .unwrap();
    assert_eq!(
        t.rows("select id, version from session order by id"),
        vec![
            vec![Value::from("sess_v1"), Value::from("0.1.42")],
            vec![Value::from("sess_v2"), Value::from("")],
        ]
    );
}
