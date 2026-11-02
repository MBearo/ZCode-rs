use rust_cli_kernel_api::message::Message;

use super::{candidates, render_file};
use crate::test_support::tool_round;

/// 候选来自 readFileState 的复制品（最近的在前）：去掉保留尾段里 Read 过的，最多 5 个。
#[test]
fn candidates_skip_files_read_in_the_kept_tail_and_cap_at_five() {
    let recent: Vec<String> = (0..7).rev().map(|index| format!("/w/{index}.rs")).collect();
    let kept: Vec<Message> = tool_round(6, "Read", "/w/6.rs").to_vec();
    let kept_refs: Vec<&Message> = kept.iter().collect();
    assert_eq!(
        candidates(&recent, &kept_refs),
        ["/w/5.rs", "/w/4.rs", "/w/3.rs", "/w/2.rs", "/w/1.rs"]
    );
}

/// 调用说明与结果说明之间只隔一个换行（折进 system 消息的文件附件是
/// `Called the Read tool …\nResult of calling the Read tool:\n…`）。
#[test]
fn the_file_attachment_joins_the_call_and_the_result_with_one_newline() {
    let text = render_file("/Users/dev/a.txt", "1\talpha\n");
    assert_eq!(
        text,
        "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/Users/dev/a.txt\"}\nResult of calling the Read tool:\n1\talpha\n\n</system-reminder>"
    );
}

/// 记下重读时工具看到的上下文。
#[derive(Default)]
struct RecordingRead(std::sync::Mutex<Vec<rust_cli_kernel_api::tool::ToolUseContext>>);

#[async_trait::async_trait]
impl rust_cli_kernel_api::tool::Tool for RecordingRead {
    fn name(&self) -> &str {
        "Read"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        _input: serde_json::Value,
        context: rust_cli_kernel_api::tool::ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<rust_cli_kernel_api::tool::ToolProgress>,
    ) -> Result<rust_cli_kernel_api::tool::ToolOutput, rust_cli_kernel_api::tool::ToolError> {
        self.0.lock().unwrap().push(context);
        Ok(rust_cli_kernel_api::tool::ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: "1\talpha\n".to_owned(),
            }],
            is_error: false,
            display: serde_json::Value::Null,
        })
    }
}

/// 一次重读是一次调用：工具上下文的运行与操作来源的运行是同一个 ID，调用 ID 与操作 ID 也是同一个（工作线 F131：
/// 以前一次调用里各取了两次 `ids.run()` 与 `ids.operation()`，同一次重读带着两个运行、两个操作）。
#[tokio::test]
async fn one_reread_carries_one_run_and_one_operation() {
    use std::sync::Arc;

    use rust_cli_kernel_api::env::Origin;
    use rust_cli_kernel_api::message::CompactTrigger;
    use rust_cli_kernel_api::testing::{
        ListTools, MemoryStore, SequentialIds, UnusedEnvironment, controller_services, machine,
    };

    let read = Arc::new(RecordingRead::default());
    let tools = ListTools(vec![read.clone()]);
    let ids: Arc<dyn rust_cli_kernel_api::clock::Ids> = Arc::new(SequentialIds::default());
    let env = Arc::new(UnusedEnvironment);
    let restore = super::Restore {
        tools: &tools,
        machine: machine(env.clone()),
        controller: controller_services(env),
        artifacts: Arc::new(MemoryStore::default()),
        ids: ids.as_ref(),
        operations: rust_cli_kernel_api::env::Operations::new(
            rust_cli_kernel_api::ids::Generation::new("g1"),
            ids.clone(),
        ),
    };
    let request = crate::test_support::request(Vec::new(), CompactTrigger::Auto);
    let files = restore
        .files(
            &request,
            &["/w/a.rs".to_owned()],
            &[],
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert_eq!(files.len(), 1);
    let seen = read.0.lock().unwrap();
    let context = &seen[0];
    let Origin::Agent { run, .. } = &context.operation.origin else {
        panic!(
            "the reread is the agent's operation: {:?}",
            context.operation
        );
    };
    assert_eq!(run, &context.run);
    assert_eq!(
        context.tool_use.as_str(),
        context.operation.operation.as_str()
    );
}
