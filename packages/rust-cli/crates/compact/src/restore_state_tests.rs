//! 压缩后重读与 readFileState（compact.md §3.4）。

use std::sync::{Arc, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::message::{CompactTrigger, Message, ToolResultContent};
use rust_cli_kernel_api::testing::ListTools;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::engine::Engine;
use crate::test_support::{
    FakeFileStates, Reply, ScriptedRunner, human, ports_with_files, request, texts, tool_round,
};

/// 像真的 Read 一样看 readFileState：状态还记着这个文件时回 `Wasted call`，否则回内容。
struct StatefulRead(Arc<FakeFileStates>);

#[async_trait]
impl Tool for StatefulRead {
    fn name(&self) -> &str {
        "Read"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let path = input["file_path"].as_str().unwrap_or_default().to_owned();
        let mut recent = self.0.recent.lock().unwrap_or_else(PoisonError::into_inner);
        let text = if recent.contains(&path) {
            "Wasted call — file unchanged since your last Read. Refer to that earlier tool_result instead."
        } else {
            recent.push(path);
            "1\talpha\n2\t"
        };
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: text.to_owned(),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// 缺陷：重读前没有清空 readFileState，文件附件是 `Wasted call`。应先复制再清空，用复制品选。
#[tokio::test]
async fn files_are_reread_after_the_state_is_cleared() {
    let runner = ScriptedRunner::new(vec![Reply::Text("<summary>S</summary>".to_owned())]);
    let files = FakeFileStates::with(&["/Users/dev/project/a.txt"]);
    let tools = Arc::new(ListTools(vec![Arc::new(StatefulRead(files.clone()))]));
    let ports = ports_with_files(runner, tools, files.clone());
    let mut history = vec![human("u1", "go")];
    history.extend(tool_round(1, "Read", "/Users/dev/project/a.txt"));
    history.push(human("u2", "next"));
    let result = Engine { ports: &ports }
        .run(
            &request(history, CompactTrigger::Manual),
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let [Message::Attachment(attachment)] = result.restored.as_slice() else {
        panic!("{:?}", result.restored);
    };
    let text = texts(&Message::Attachment(attachment.clone()));
    assert!(
        text.ends_with("Result of calling the Read tool:\n1\talpha\n2\t\n</system-reminder>"),
        "{text}"
    );
    // 重读之后状态里又记着它（重读同样写回 readFileState）。
    assert_eq!(*files.recent.lock().unwrap(), ["/Users/dev/project/a.txt"]);
}
