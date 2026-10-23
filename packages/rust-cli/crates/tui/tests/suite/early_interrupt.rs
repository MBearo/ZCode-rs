//! 输入还没提交时被中断（修复线 F37，docs/specs/rust-cli/session-lifecycle.md §3.1）：UserPromptSubmit hook 还在跑时
//! 按 esc 或收到退出信号，输入照常留在对话里，接着是中断标记；下一条输入的请求带着它们。
//! hook 的命令在执行环境里一直不结束，只有回合被中止（hook 的取消令牌经 `Environment::cancel` 结束它）
//! 才往下走，所以中断一定落在空档里。旧代码以 `Aborted` 结束、什么都不提交：对话里没有这条输入，下一条请求也没有。
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use crossterm::event::KeyCode;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, Environment, FileContent, OperationContext, Outcome,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::testing::{MemoryStore, UnusedEnvironment, text_response};
use rust_cli_runtime::{HookSource, HooksConfig, HooksSettings};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use super::support::{GatedModel, Setup, request_text, with_ui};

const MARKER: &str = "[Request interrupted by user]";

/// hook 命令的执行环境：第一条命令一直不结束，直到它被取消（`cancel`）；之后的命令立即以 0 结束、没有输出。
/// 其余操作同 [`UnusedEnvironment`]。
struct HeldHook {
    first: Mutex<Option<OperationId>>,
    started: Notify,
    cancelled: watch::Sender<bool>,
}

impl HeldHook {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            first: Mutex::new(None),
            started: Notify::new(),
            cancelled: watch::Sender::new(false),
        })
    }

    fn is_first(&self, operation: &OperationId) -> bool {
        self.first
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            == Some(operation)
    }
}

fn finished(outcome: Outcome, exit_code: Option<i32>) -> Result<CommandEvent, EnvError> {
    Ok(CommandEvent::Finished {
        outcome,
        exit_code,
        timed_out: false,
        cwd_reset: None,
        output_bytes: 0,
        output_limit: None,
    })
}

#[async_trait]
impl Environment for HeldHook {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        UnusedEnvironment.facts(workspace).await
    }

    async fn read_file(
        &self,
        workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        UnusedEnvironment.read_file(workspace, request).await
    }

    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        UnusedEnvironment
            .write_file(workspace, context, request)
            .await
    }

    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        context: OperationContext,
        _request: StartCommand,
    ) -> Result<(), EnvError> {
        let mut first = self.first.lock().unwrap_or_else(PoisonError::into_inner);
        if first.is_none() {
            *first = Some(context.operation);
            self.started.notify_one();
        }
        Ok(())
    }

    fn watch_command(
        &self,
        operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        let started = Ok(CommandEvent::Started {
            output: EnvPath::new("/tmp/hook-out"),
        });
        if !self.is_first(&operation) {
            return stream::iter([started, finished(Outcome::Completed, Some(0))]).boxed();
        }
        let mut cancelled = self.cancelled.subscribe();
        let end = stream::once(async move {
            let _ = cancelled.wait_for(|cancelled| *cancelled).await;
            finished(Outcome::Cancelled, None)
        });
        stream::iter([started]).chain(end).boxed()
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        if self.is_first(&operation) {
            self.cancelled.send_replace(true);
        }
        Ok(())
    }

    async fn operation_status(&self, _operation: OperationId) -> Result<Outcome, EnvError> {
        Ok(Outcome::Unknown)
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

/// 一个 UserPromptSubmit command hook（命令本身由 `HeldHook` 扮演）。
fn prompt_hook() -> HooksConfig {
    let settings = serde_json::json!({"hooks": {
        "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "slow"}]}],
    }});
    let (parsed, warnings) = HooksSettings::from_json(&settings);
    assert!(warnings.is_empty(), "{warnings:?}");
    HooksConfig {
        version: 1,
        sources: vec![(HookSource::User, parsed)],
        ..HooksConfig::default()
    }
}

/// esc：输入框里留着草稿（这时不把输入放回输入框，session-lifecycle.md §8），对话里是第一条输入与中断的提示；
/// 回车发出草稿，模型收到第一条输入、中断标记与草稿。
#[tokio::test(flavor = "multi_thread")]
async fn esc_while_the_prompt_hook_runs_keeps_the_input_with_the_marker() {
    let env = HeldHook::new();
    let model = GatedModel::scripted(vec![text_response(&["second reply"])]);
    let setup = Setup {
        env: env.clone(),
        hooks: Some(prompt_hook()),
        ..Setup::default()
    };
    let held = env.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first prompt");
        held.started.notified().await;
        ui.wait_for("esc to interrupt").await;
        ui.type_text("second prompt");
        ui.wait_for("second prompt").await;
        ui.key(KeyCode::Esc);
        let screen = ui.wait_for("Interrupted").await;
        assert!(screen.contains("first prompt"), "{screen}");
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| {
            s.contains("second reply") && !s.contains("esc to interrupt")
        })
        .await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 1, "no request for the interrupted turn");
    let text = request_text(&requests[0]);
    let first = text.find("first prompt").unwrap_or(usize::MAX);
    let marker = text.find(MARKER).unwrap_or(usize::MAX);
    let second = text.find("second prompt").unwrap_or(usize::MAX);
    assert!(
        first < marker && marker < second && second != usize::MAX,
        "{text}"
    );
}

/// 退出信号（R5-2）落在空档里（修复线 F38，session-lifecycle.md §3.5 D4）：收到退出信号不中止 query、直接退出，这条
/// 输入还没进对话——库里什么都没有，不调模型。F37 时按 esc
/// 记下输入与中断标记；再之前（F35 的 `exit_signal` 偶发失败）同样什么都没有，但那是判断错误的副作用。
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_signal_while_the_prompt_hook_runs_records_nothing() {
    let env = HeldHook::new();
    let (model, senders) = GatedModel::new(1);
    let stop = CancellationToken::new();
    let store = Arc::new(MemoryStore::default());
    let setup = Setup {
        env: env.clone(),
        hooks: Some(prompt_hook()),
        stop: Some(stop.clone()),
        store: store.clone(),
        ..Setup::default()
    };
    let held = env.clone();
    let run = with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first prompt");
        held.started.notified().await;
        ui.wait_for("esc to interrupt").await;
        stop.cancel();
    });
    // 期限只用于让失败的测试结束。
    let session = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("the loop left on the stop signal")
        .expect("a session was created");
    drop(senders);
    let facts = format!("{:?}", store.facts(&SessionId::new(session)));
    assert!(
        !facts.contains("first prompt") && !facts.contains(MARKER),
        "{facts}"
    );
    assert!(
        model.requests().is_empty(),
        "no request for the interrupted turn"
    );
}
