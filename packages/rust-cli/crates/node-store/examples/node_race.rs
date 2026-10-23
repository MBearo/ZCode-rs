//! node-store 的双进程并发写与迁移竞争辅助程序（docs/specs/rust-cli/node-store.md §6 第 12 段），由
//! `scripts/rust-cli/node-interop-race.mjs` 调用。stdout 上每行一个 JSON。要对齐起跑的子命令先输出
//! `{"ready": …}`，再等 stdin 的一行（起跑信号）：
//!
//! - `open <storage-dir>`：起跑后第一次使用 NodeStore（打开会话库并补齐迁移），输出起止时间与错误；
//! - `write <storage-dir> <session|new> <n> <tag> [<pause-ms>]`：起跑前打开会话库（`new` 时先建会话），起跑后
//!   提交 n 轮（每轮 user + assistant 一个 commit，文本是 `<tag> q<i>` 与 `<tag> a<i>`，轮间停 pause 毫秒），输出
//!   每轮的回执序号、起止时间与错误；
//! - `hold <db> <ms> [wal]`：用另一个 rusqlite 连接开 `begin immediate`（`wal` 时先切 WAL），输出
//!   `{"held": …}`，持锁 ms 毫秒后提交，输出 `{"released": …}`（模拟长事务与长迁移）；
//! - `read <storage-dir> <session>`：输出冷读取的消息（`LoadedSession.messages`）。
//!
//! 会话库是 `<storage-dir>/cli/db/db.sqlite`。时间是 Unix 毫秒（node-store 的 `SystemClock`），与 Node 的
//! `Date.now()` 可比。
use std::io::BufRead;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    CommandId, EnvironmentId, MessageId, OwnerEpoch, SessionId, TraceId,
};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{
    Fact, FactBatch, SessionFilter, SessionKind, SessionMeta, SessionStore,
};
use rust_cli_node_store::{LOCAL_ENVIRONMENT, NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use serde_json::{Value, json};
use tokio::runtime::Runtime;

const WORKSPACE: &str = "/w/race";

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
    Ok(NodeStore::new(paths, Arc::new(ids())).with_version("0.1.1"))
}

fn ids() -> NodeIds {
    NodeIds::new(Arc::new(SystemClock::utc()))
}

fn now_ms() -> u64 {
    ids().now_ms()
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
        root: EnvPath::new(WORKSPACE),
    }
}

/// 第一次使用 NodeStore 时打开会话库并补齐迁移；列表是最轻的一次使用。
async fn touch(store: &NodeStore) -> Result<()> {
    let filter = SessionFilter {
        workspace: workspace(),
        limit: 1,
    };
    store.list(filter).await?;
    Ok(())
}

fn batch(facts: Vec<Fact>) -> FactBatch {
    FactBatch {
        owner_epoch: OwnerEpoch::LOCAL,
        command: Some(CommandId::new("race")),
        facts,
        // 没有回合锚点：按库推导当前轮（node-store.md §4，修复线 F6；与互操作夹具相同）。
        turn: None,
    }
}

fn user(id: MessageId, text: String) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id,
            content: vec![ContentBlock::Text { text }],
            origin: UserOrigin::Human,
        }),
    }
}

fn reply(id: MessageId, text: String) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id,
            content: vec![ContentBlock::Text { text }],
            model: "m".into(),
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 12,
                output_tokens: 7,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
        }),
    }
}

/// 与 `node_interop` 相同的新会话：会话行、模型选择与 acceptEdits。
async fn create(store: &NodeStore) -> Result<SessionId> {
    let id = ids().session();
    store
        .create(SessionMeta {
            id: id.clone(),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: workspace(),
            title: String::new(),
            trace: TraceId::new(id.as_str()),
            created_at_ms: now_ms(),
        })
        .await?;
    let selection = ModelSelection {
        provider: "p".into(),
        model: "m".into(),
        reasoning: None,
        connection: None,
    };
    let facts = vec![
        Fact::ModelSelected { selection },
        Fact::ModeChanged {
            mode: PermissionMode::AcceptEdits,
        },
    ];
    store.commit(&id, batch(facts)).await?;
    Ok(id)
}

#[expect(clippy::print_stdout, reason = "辅助程序的输出就是 stdout 上的 JSON")]
fn print(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

/// 起跑信号：协调脚本同时给两边写一行。
fn gate() -> Result<()> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(())
}

fn open(runtime: &Runtime, dir: &str) -> Result<()> {
    let store = store(dir)?;
    print(&json!({"ready": true}))?;
    gate()?;
    let start = now_ms();
    let clock = Instant::now();
    let result = runtime.block_on(touch(&store));
    print(&json!({
        "start": start,
        "end": now_ms(),
        "elapsed": clock.elapsed().as_millis() as u64,
        "error": result.err().map(|e| e.to_string()),
    }))
}

/// `pause` 是轮间的停顿（真实回合的提交之间隔着模型请求）。
fn write(
    runtime: &Runtime,
    dir: &str,
    session: &str,
    n: usize,
    tag: &str,
    pause: Duration,
) -> Result<()> {
    let store = store(dir)?;
    let id = runtime.block_on(async {
        // 起跑前打开会话库：起跑后的提交只等写锁，不含迁移。
        touch(&store).await?;
        if session == "new" {
            create(&store).await
        } else {
            Ok(SessionId::new(session))
        }
    })?;
    print(&json!({"ready": true, "session": id.as_str()}))?;
    gate()?;
    let rounds = runtime.block_on(async {
        let mut rounds = vec![];
        for i in 0..n {
            let start = now_ms();
            let facts = vec![
                user(ids().message(), format!("{tag} q{i}")),
                reply(ids().message(), format!("{tag} a{i}")),
            ];
            let result = store.commit(&id, batch(facts)).await;
            rounds.push(json!({
                "start": start,
                "end": now_ms(),
                "sequence": result.as_ref().ok().map(|r| r.sequence),
                "error": result.err().map(|e| e.to_string()),
            }));
            if !pause.is_zero() {
                tokio::time::sleep(pause).await;
            }
        }
        rounds
    });
    print(&json!({"session": id.as_str(), "rounds": rounds}))
}

fn hold(db: &str, ms: u64, wal: bool) -> Result<()> {
    let conn = rusqlite::Connection::open(db)?;
    if wal {
        let mode: String = conn.query_row("pragma journal_mode = wal", [], |r| r.get(0))?;
        if mode != "wal" {
            bail!("journal mode is {mode}");
        }
    }
    conn.execute_batch("begin immediate")?;
    print(&json!({"held": now_ms()}))?;
    // 模拟长事务：本程序只有这一个线程在用连接。
    std::thread::sleep(Duration::from_millis(ms));
    // 释放时间取在提交之前：等锁的一方只能在这之后拿到锁。
    let released = now_ms();
    conn.execute_batch("commit")?;
    print(&json!({ "released": released }))
}

async fn read(dir: &str, session: &str) -> Result<()> {
    let loaded = store(dir)?.load(&SessionId::new(session)).await?;
    print(&serde_json::to_value(&loaded.messages)?)
}

#[expect(
    clippy::disallowed_methods,
    reason = "互操作辅助程序（不是产品代码）读自己的命令行参数"
)]
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match args.as_slice() {
        [command, dir] if command == "open" => open(&runtime, dir),
        [command, dir, session, n, tag, rest @ ..] if command == "write" && rest.len() <= 1 => {
            let pause = rest.first().map(|ms| ms.parse()).transpose()?.unwrap_or(0);
            write(
                &runtime,
                dir,
                session,
                n.parse()?,
                tag,
                Duration::from_millis(pause),
            )
        }
        [command, db, ms] if command == "hold" => hold(db, ms.parse()?, false),
        [command, db, ms, wal] if command == "hold" && wal == "wal" => hold(db, ms.parse()?, true),
        [command, dir, session] if command == "read" => runtime.block_on(read(dir, session)),
        _ => bail!(
            "usage: node_race open <dir> | write <dir> <session|new> <n> <tag> [<pause-ms>] | hold <db> <ms> [wal] | read <dir> <session>"
        ),
    }
}
