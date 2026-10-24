//! `/usage` 的查询经内核到存储（docs/specs/rust-cli/usage.md §4、§5）。
use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, OwnerEpoch, SessionId};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::session_api::{CreateSession, Kernel, KernelError};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryStore, ScriptedModel, SequentialIds, UnusedEnvironment, environments,
    selection,
};

use super::*;

fn kernel(store: Arc<MemoryStore>) -> LocalKernel {
    LocalKernel::new(
        Ports {
            model: Arc::new(ScriptedModel::new(Vec::new())),
            environments: environments(Arc::new(UnusedEnvironment)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
    )
}

fn call(message: &str, duration: u64, status: CallStatus) -> Fact {
    Fact::ModelCalled {
        call: ModelCall {
            message: MessageId::new(message),
            source: QuerySource::MainThread,
            selection: selection(),
            started_at_ms: 10,
            first_token_at_ms: None,
            completed_at_ms: 10 + duration,
            status,
            stop_reason: None,
            tool_calls: 0,
            usage: None,
            retries: 0,
            error: None,
        },
    }
}

#[tokio::test]
async fn api_duration_comes_from_the_stored_calls() {
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone());
    let session = kernel
        .create_session(CreateSession {
            id: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/work"),
            },
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let facts = vec![
        call("msg_1", 1_200, CallStatus::Completed),
        call("msg_2", 800, CallStatus::Error),
        call("msg_3", 300, CallStatus::Completed),
    ];
    store
        .commit(
            &session,
            FactBatch {
                owner_epoch: OwnerEpoch::LOCAL,
                command: None,
                facts,
                turn: None,
            },
        )
        .await
        .unwrap();
    let metering = kernel.session_metering(&session).await.unwrap();
    assert_eq!(metering.api_duration_ms, 1_500);
    assert!(matches!(
        kernel.session_metering(&SessionId::new("sess_none")).await,
        Err(KernelError::NotFound { .. })
    ));
}

#[tokio::test]
async fn activity_asks_the_store_with_the_clock_day() {
    use rust_cli_kernel_api::metering::{ActivityRange, ActivityStats};
    let store = Arc::new(MemoryStore::default());
    store.set_activity(
        ActivityRange::Week,
        ActivityStats {
            sessions: 3,
            ..ActivityStats::default()
        },
    );
    let kernel = kernel(store);
    assert_eq!(
        kernel.activity(ActivityRange::Week).await.unwrap().sessions,
        3
    );
    // 没有预设的范围：MemoryStore 回传查询里的今天（FixedClock(1_000) 是 1970-01-01）。
    let all = kernel.activity(ActivityRange::All).await.unwrap();
    assert_eq!(all.today, "1970-01-01");
    assert_eq!(all.range, Some(ActivityRange::All));
}

/// footer 的请求统计（usage.md §4.1，F27）经内核到存储：MemoryStore 与 node-store 同一个口径（只算完成的主回合请求，
/// 输入含缓存读写；最近一个是最后提交的）。
#[tokio::test]
async fn request_stats_come_from_the_stored_calls() {
    use rust_cli_kernel_api::message::Usage;
    let store = Arc::new(MemoryStore::default());
    let kernel = kernel(store.clone());
    let session = kernel
        .create_session(CreateSession {
            id: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/work"),
            },
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let with_usage = |fact: Fact, first: u64, usage: Usage| match fact {
        Fact::ModelCalled { mut call } => {
            call.first_token_at_ms = Some(first);
            call.usage = Some(usage);
            Fact::ModelCalled { call }
        }
        other => other,
    };
    let usage = |input, read, output| Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: read,
    };
    let facts = vec![
        with_usage(
            call("msg_1", 5_000, CallStatus::Completed),
            2_010,
            usage(200, 800, 120),
        ),
        with_usage(call("msg_2", 800, CallStatus::Error), 20, usage(999, 0, 1)),
    ];
    store
        .commit(
            &session,
            FactBatch {
                owner_epoch: OwnerEpoch::LOCAL,
                command: None,
                facts,
                turn: None,
            },
        )
        .await
        .unwrap();
    let stats = kernel.request_stats(&session).await.unwrap();
    assert_eq!(stats.requests, 1);
    assert_eq!(stats.cache_hit_rate(), Some(0.8));
    assert_eq!(stats.tokens_per_second(), Some(40.0));
    assert!(matches!(
        kernel.request_stats(&SessionId::new("sess_none")).await,
        Err(KernelError::NotFound { .. })
    ));
}
