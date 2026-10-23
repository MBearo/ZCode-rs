//! 工具结束后才被中止：给模型的结果换成中断文本，写入的 checkpoint 保留（session-lifecycle.md §4.2）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{QueryEvent, QueryStop};
use rust_cli_kernel_api::testing::ScriptedModel;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::tests::tool_uses;
use crate::run::tests::{parts, request, scope};

/// 写完文件后、结果交付前用户中断（在工具里触发中止，时序确定）。
struct WriteThenInterrupted {
    abort: Abort,
}

#[async_trait]
impl Tool for WriteThenInterrupted {
    fn name(&self) -> &str {
        "Write"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        self.abort.abort(AbortReason::User);
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "File created successfully".to_owned(),
            }],
            is_error: false,
            display: json!({"type": "create", "checkpoint": {"path": "/work/a.txt", "additions": 1}}),
        })
    }
}

#[tokio::test]
async fn an_interrupt_after_the_write_keeps_its_checkpoint() {
    let abort = Abort::new();
    let tool = Arc::new(WriteThenInterrupted {
        abort: abort.clone(),
    });
    let model = Arc::new(ScriptedModel::new(vec![tool_uses(&[(
        "t1",
        "Write",
        json!({"file_path": "/work/a.txt"}),
    )])]));
    let request = request("go");
    let (scope, sink) = scope(parts(model, vec![tool]), &request, abort);
    assert_eq!(
        crate::run::run(scope, request).await.unwrap(),
        QueryStop::Aborted
    );
    let finished = sink
        .events()
        .into_iter()
        .find_map(|event| match event {
            QueryEvent::ToolFinished { output, .. } => Some(output),
            _ => None,
        })
        .unwrap();
    // 模型看到中断文本；只保留写入的回执，不保留其他界面数据。
    assert!(finished.is_error);
    assert!(
        format!("{:?}", finished.content).contains("The user doesn't want to proceed"),
        "{finished:?}"
    );
    assert_eq!(
        finished.display,
        json!({"checkpoint": {"path": "/work/a.txt", "additions": 1}})
    );
}
