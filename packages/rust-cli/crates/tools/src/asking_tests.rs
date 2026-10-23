use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, QuestionAnswer,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{MemoryStore, UnusedEnvironment};
use rust_cli_kernel_api::tool::ToolUseContext;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::*;

/// 永远不回答的交互 owner；记下撤回的询问。
#[derive(Default)]
struct Silent {
    withdrawn: Mutex<Vec<(SessionId, ToolUseId)>>,
}

#[async_trait]
impl Interactions for Silent {
    async fn ask(
        &self,
        _session: &SessionId,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        std::future::pending().await
    }

    async fn cancel_ask(&self, session: &SessionId, tool_use: &ToolUseId) {
        self.withdrawn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((session.clone(), tool_use.clone()));
    }
}

fn context(interactions: Arc<dyn Interactions>, cancel: CancellationToken) -> ToolUseContext {
    ToolUseContext {
        session: SessionId::new("sess_1"),
        run: RunId::new("run_1"),
        tool_use: ToolUseId::new("toolu_1"),
        agent: None,
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        mode: PermissionMode::Default,
        cancel,
        env: Arc::new(UnusedEnvironment),
        artifacts: Arc::new(MemoryStore::default()),
        interactions,
        operation: OperationContext {
            environment: EnvironmentId::new("local"),
            generation: Generation::new("g1"),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: OperationId::new("op_1"),
            origin: Origin::Agent {
                session: SessionId::new("sess_1"),
                run: RunId::new("run_1"),
            },
        },
    }
}

/// 工具被取消：撤回这个调用的询问后返回 `Cancelled`（R1-5）。
#[tokio::test]
async fn a_cancelled_ask_is_withdrawn() {
    let owner = Arc::new(Silent::default());
    let cancel = CancellationToken::new();
    let context = context(owner.clone(), cancel.clone());
    let request = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_1"),
        questions: Value::Null,
    };
    cancel.cancel();
    let asked = ask::<QuestionAnswer>(&context, request).await;
    assert!(asked.is_err());
    assert_eq!(
        *owner
            .withdrawn
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        [(SessionId::new("sess_1"), ToolUseId::new("toolu_1"))]
    );
}

/// 读不出的回答按没有回答（调用方当作拒绝）。
#[tokio::test]
async fn an_unreadable_answer_is_none() {
    struct Odd;
    #[async_trait]
    impl Interactions for Odd {
        async fn ask(
            &self,
            _session: &SessionId,
            request: InteractionRequest,
        ) -> Result<InteractionAnswer, OrchestrationError> {
            Ok(InteractionAnswer {
                interaction: rust_cli_kernel_api::ids::InteractionId::new(
                    request.tool_use().as_str(),
                ),
                answer: Value::String("yes".to_owned()),
            })
        }
    }
    let context = context(Arc::new(Odd), CancellationToken::new());
    let request = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_1"),
        questions: Value::Null,
    };
    assert!(matches!(
        ask::<QuestionAnswer>(&context, request).await,
        Ok(None)
    ));
}
