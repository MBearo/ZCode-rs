//! 互操作的用量段（docs/specs/rust-cli/usage.md §9，由 `scripts/rust-cli/node-interop-usage.mjs` 调用）：
//!
//! - `usage-write <storage-dir>`：写一个会话：两次请求（第一次带一个工具调用）、一次工具调用、回合结束，以及一个
//!   子代理子会话里的一次请求（没有消息）；输出会话 id 与写入时用的时刻和值；
//! - `usage-side <storage-dir>`：另一个会话里会话标题的旁路请求（没有消息，工作线 F71；F73 起按产品的写法：单独一批、锚在
//!   首条输入、没有首字时间），之后这一轮结束；输出会话 id、写入时用的时刻与输入、请求的 id；
//! - `metering <storage-dir> <session>`：`SessionStore::metering` 的结果；
//! - `activity <storage-dir> <all|week|month> <today> <offset-minutes>`：`SessionStore::activity` 的结果；
//! - `request-stats <storage-dir> <session>`：`SessionStore::request_stats` 的结果与它给出的 TPS、命中率（footer 的
//!   请求统计，usage.md §4.1，修复线 F27）。
use rust_cli_kernel_api::metering::{
    ActivityQuery, ActivityRange, CallStatus, ModelCall, ToolCall,
};
use rust_cli_kernel_api::model::{QuerySource, SIDE_SESSION_TITLE};

use super::*;

/// 两次请求共用的用量（与 `reply` 的 assistant 用量相同）。
fn usage() -> Usage {
    Usage {
        input_tokens: 12,
        output_tokens: 7,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 4,
    }
}

fn requested() -> ModelSelection {
    ModelSelection {
        provider: "anthropic".into(),
        model: "claude-opus-5-5".into(),
        reasoning: Some("max".into()),
        connection: None,
    }
}

fn model(message: &MessageId, source: QuerySource, at: (u64, u64, u64), stop: StopReason) -> Fact {
    let (started, first, completed) = at;
    Fact::ModelCalled {
        call: ModelCall {
            message: message.clone(),
            source,
            selection: requested(),
            started_at_ms: started,
            first_token_at_ms: Some(first),
            completed_at_ms: completed,
            status: CallStatus::Completed,
            stop_reason: Some(stop),
            tool_calls: u32::from(stop == StopReason::ToolUse),
            usage: Some(usage()),
            retries: u32::from(stop == StopReason::ToolUse),
            error: None,
        },
    }
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    // 时刻取近一分钟：Node 的保留期按当前时间删 30 天以前的行。
    let base = now_ms() - 60_000;
    let session = create(&store).await?;
    let (u1, a1, r1, a2) = (message_id(), message_id(), message_id(), message_id());
    // 批次带回合锚点（会话 actor 的写法）：用量行的轮次与父消息取锚点，与 Node 逐列比较（修复线 F6）。
    let mut turn = None;
    store
        .commit(
            &session,
            anchored(&mut turn, vec![user(&u1, "Count the files")]),
        )
        .await?;
    let call = ToolUseId::new("toolu_usage_1");
    let use_block = ContentBlock::ToolUse {
        id: call.clone(),
        name: "Read".into(),
        input: json!({"file_path": "/w/interop/a.txt"}),
    };
    let facts = vec![
        reply(&a1, vec![use_block], StopReason::ToolUse),
        model(
            &a1,
            QuerySource::MainThread,
            (base, base + 300, base + 900),
            StopReason::ToolUse,
        ),
    ];
    store.commit(&session, anchored(&mut turn, facts)).await?;
    let result = Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: r1,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: call.clone(),
                content: vec![ToolResultContent::Text {
                    text: "hello".into(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: a1.clone(),
            },
        }),
    };
    let tool = Fact::ToolCalled {
        call: ToolCall {
            tool_use: call,
            name: "Read".into(),
            started_at_ms: base + 1_000,
            completed_at_ms: base + 1_400,
            status: CallStatus::Completed,
            output_bytes: 5,
        },
    };
    store
        .commit(&session, anchored(&mut turn, vec![result, tool]))
        .await?;
    let facts = vec![
        reply(&a2, vec![text("One file.")], StopReason::EndTurn),
        model(
            &a2,
            QuerySource::MainThread,
            (base + 1_500, base + 1_700, base + 2_100),
            StopReason::EndTurn,
        ),
    ];
    store.commit(&session, anchored(&mut turn, facts)).await?;
    let ended = Fact::TurnEnded {
        status: CallStatus::Completed,
    };
    store
        .commit(&session, anchored(&mut turn, vec![ended]))
        .await?;
    // 子代理：子会话里一次没有消息的请求（计量单独一批）。
    let (_, child) = ids().subagent();
    store
        .create(SessionMeta {
            id: child.clone(),
            kind: SessionKind::Subagent,
            parent: Some(session.clone()),
            workspace: WorkspaceRef {
                environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
                root: EnvPath::new(WORKSPACE),
            },
            title: String::new(),
            trace: TraceId::new(child.as_str()),
            created_at_ms: now_ms(),
        })
        .await?;
    let (cu, ca) = (message_id(), message_id());
    let mut child_turn = None;
    store
        .commit(
            &child,
            anchored(&mut child_turn, vec![user(&cu, "Look around")]),
        )
        .await?;
    let sub = model(
        &ca,
        QuerySource::Subagent("Explore".into()),
        (base + 1_000, base + 1_100, base + 1_500),
        StopReason::EndTurn,
    );
    store
        .commit(&child, anchored(&mut child_turn, vec![sub]))
        .await?;
    print(&json!({
        "session": session.as_str(),
        "child": child.as_str(),
        "base": base,
        "messages": {"user": u1.as_str(), "first": a1.as_str(), "second": a2.as_str(), "childUser": cu.as_str(), "childCall": ca.as_str()},
    }))
}

/// 会话标题的行（工作线 F71、F73，usage.md §2.1.1）：`query_source` 写 Node 的名字；按会话 actor 的写法单独一批、锚在
/// 首条输入，没有首字时间（Node 的标题请求不流式）。之后这一轮结束：回合用量不数标题行。WebFetch 摘要 Node 不写行，
/// 这里也不写（F73 起去掉）。
pub(super) async fn side(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let base = now_ms() - 60_000;
    let session = create(&store).await?;
    let (user_id, title) = (message_id(), message_id());
    let mut turn = None;
    store
        .commit(
            &session,
            anchored(&mut turn, vec![user(&user_id, "Name this session")]),
        )
        .await?;
    let call = Fact::ModelCalled {
        call: ModelCall {
            message: title.clone(),
            source: QuerySource::Side(SIDE_SESSION_TITLE.to_owned()),
            selection: requested(),
            started_at_ms: base,
            first_token_at_ms: None,
            completed_at_ms: base + 400,
            status: CallStatus::Completed,
            stop_reason: Some(StopReason::EndTurn),
            tool_calls: 0,
            usage: Some(usage()),
            retries: 0,
            error: None,
        },
    };
    store
        .commit(&session, anchored(&mut turn, vec![call]))
        .await?;
    let ended = Fact::TurnEnded {
        status: CallStatus::Completed,
    };
    store
        .commit(&session, anchored(&mut turn, vec![ended]))
        .await?;
    print(&json!({
        "session": session.as_str(),
        "base": base,
        "messages": {"user": user_id.as_str(), "title": title.as_str()},
    }))
}

pub(super) async fn metering(dir: &str, session: &str) -> Result<()> {
    let metering = store(dir)?.metering(&SessionId::new(session)).await?;
    print(&serde_json::to_value(metering)?)
}

pub(super) async fn activity(dir: &str, range: &str, today: &str, offset: &str) -> Result<()> {
    let range = match range {
        "all" => ActivityRange::All,
        "week" => ActivityRange::Week,
        "month" => ActivityRange::Month,
        other => bail!("unknown range {other}"),
    };
    let query = ActivityQuery {
        range,
        today: today.to_owned(),
        utc_offset_minutes: offset.parse()?,
    };
    let stats = store(dir)?.activity(query).await?;
    print(&serde_json::to_value(stats)?)
}

pub(super) async fn request_stats(dir: &str, session: &str) -> Result<()> {
    let stats = store(dir)?.request_stats(&SessionId::new(session)).await?;
    print(&json!({
        "stats": stats,
        "tps": stats.tokens_per_second(),
        "hitRate": stats.cache_hit_rate(),
    }))
}
