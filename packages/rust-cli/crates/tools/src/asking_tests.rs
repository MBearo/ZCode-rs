use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, RunId, SessionId, ToolUseId,
};
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError, QuestionAnswer,
};
use rust_cli_kernel_api::testing::{UnusedEnvironment, controller_services, machine};
use rust_cli_kernel_api::tool::ToolUseContext;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::*;

/// 永远不回答的交互 owner；记下撤回的询问。
#[derive(Default)]
struct Silent {
    withdrawn: Mutex<Vec<ToolUseId>>,
}

#[async_trait]
impl rust_cli_kernel_api::testing::AskOverride for Silent {
    async fn ask(
        &self,
        _request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        std::future::pending().await
    }

    async fn cancel_ask(&self, tool_use: &ToolUseId) {
        self.withdrawn.lock_unpoisoned().push(tool_use.clone());
    }
}

fn context(interactions: Arc<dyn Interactions>, cancel: CancellationToken) -> ToolUseContext {
    let env = Arc::new(UnusedEnvironment);
    ToolUseContext {
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        cancel,
        machine: machine(env.clone()),
        controller: controller_services(env),
        interactions,
        session_ops: Arc::new(rust_cli_kernel_api::testing::NoSession),
        operation: OperationContext::new(
            EnvironmentId::new("local"),
            Generation::new("g1"),
            OperationId::new("op_1"),
            Origin::Agent {
                session: SessionId::new("sess_1"),
                run: RunId::new("run_1"),
            },
        ),
        ..rust_cli_kernel_api::testing::tool_use_context()
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
        *owner.withdrawn.lock_unpoisoned(),
        [ToolUseId::new("toolu_1")]
    );
}

/// 别的种类的回答按没有回答（调用方当作拒绝；工作线 F134 以前是读不出的 JSON）。
#[tokio::test]
async fn an_answer_of_another_kind_is_none() {
    let odd = rust_cli_kernel_api::testing::ScriptedInteractions::always(
        rust_cli_kernel_api::orchestration::PermissionAnswer::Unavailable,
    );
    let context = context(Arc::new(odd), CancellationToken::new());
    let request = InteractionRequest::Question {
        tool_use: ToolUseId::new("toolu_1"),
        questions: Value::Null,
    };
    assert!(matches!(
        ask::<QuestionAnswer>(&context, request).await,
        Ok(None)
    ));
}
