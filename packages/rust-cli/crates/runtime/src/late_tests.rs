//! 先建后接的压缩器（R1-13）：没接上时报错，接上后每个方法都转发。

use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::message::CompactTrigger;

use super::*;

/// 只回答用量与“要压缩”的压缩器。
struct Fixed;

#[async_trait]
impl Compactor for Fixed {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        true
    }

    fn context_usage(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
    ) -> Option<(u64, u64)> {
        Some((150_000, 180_000))
    }

    fn blocking_limit(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
        failure: Option<&str>,
    ) -> Option<String> {
        Some(format!("blocked {failure:?}"))
    }

    async fn compact(
        &self,
        _request: CompactRequest,
        _cancel: CancellationToken,
    ) -> Result<CompactResult, OrchestrationError> {
        Err(OrchestrationError::Failed {
            message: "not in this test".to_owned(),
        })
    }
}

fn selection() -> ModelSelection {
    ModelSelection {
        provider: "anthropic".to_owned(),
        model: GLM_5_3.to_owned(),
        reasoning: None,
        connection: None,
    }
}

fn request() -> CompactRequest {
    CompactRequest {
        session: SessionId::new("sess_1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/project"),
        },
        trigger: CompactTrigger::Auto,
        selection: selection(),
        instructions: None,
        messages: Vec::new(),
        mode: None,
        agent: None,
        initial_token_gap: None,
    }
}

/// 旧代码没接上时回 `NotNeeded`，悄悄跳过自动压缩。
#[tokio::test]
async fn an_unassembled_compactor_fails_instead_of_skipping() {
    let late = LateCompactor::default();
    let outcome = late
        .auto_compact(
            request(),
            &mut CompactTracking::default(),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(outcome, Err(OrchestrationError::Failed { .. })));
    assert!(
        late.compact(request(), CancellationToken::new())
            .await
            .is_err()
    );
}

/// 旧代码不转发 `context_usage`（总是缺省的 None）。
#[test]
fn an_assembled_compactor_forwards_every_method() {
    let late = LateCompactor::default();
    late.set(Arc::new(Fixed));
    assert!(late.should_auto_compact(&[], &selection()));
    assert_eq!(
        late.context_usage(&[], &selection()),
        Some((150_000, 180_000))
    );
    // 阻塞线（compact.md §2.2）同样转发；没有转发时主循环看到的是缺省的“不拦”。
    assert_eq!(
        late.blocking_limit(&[], &selection(), Some("why")),
        Some("blocked Some(\"why\")".to_owned())
    );
    assert_eq!(
        LateCompactor::default().blocking_limit(&[], &selection(), None),
        None
    );
}
