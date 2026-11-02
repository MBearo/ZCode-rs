//! 子代理在工具边界收到的通知（docs/specs/rust-cli/query.md §5.1）：插话之后，每条通知一个
//! `queued_command`（`commandMode: task-notification`），文本是通知全文。

use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
use rust_cli_kernel_api::orchestration::{Absorbed, QueryStop, RunSink};
use rust_cli_kernel_api::testing::{
    RecordingSink, ScriptedModel, text_response, tool_use_response,
};
use serde_json::json;

use super::tests::{parts, request};
use crate::RunScope;

const NOTICE: &str = "<system-reminder>\n[SYSTEM NOTIFICATION - NOT USER INPUT]\n<task-notification>\n<task-id>agent_2</task-id>\n</task-notification>\n</system-reminder>";

/// 记录消息的输出端，另带一队通知。
struct NoticeSink {
    inner: Arc<RecordingSink>,
    notices: Mutex<Vec<String>>,
}

impl rust_cli_kernel_api::testing::OpsOverride for NoticeSink {}

/// 询问交给内层的记录输出端。
impl rust_cli_kernel_api::testing::AskOverride for NoticeSink {
    fn asks(&self) -> &dyn rust_cli_kernel_api::orchestration::Interactions {
        self.inner.as_ref()
    }
}

/// 插话（内层记下的）在前、这一队通知在后。
#[async_trait]
impl rust_cli_kernel_api::testing::SinkOverride for NoticeSink {
    fn sink(&self) -> &dyn RunSink {
        self.inner.as_ref()
    }

    async fn take_absorbed(&self) -> Vec<Absorbed> {
        let mut absorbed = self.inner.take_absorbed().await;
        let notices = std::mem::take(&mut *self.notices.lock_unpoisoned());
        absorbed.extend(Absorbed::ordered(Vec::new(), notices));
        absorbed
    }
}

#[tokio::test]
async fn notices_join_the_run_after_the_steering_input() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Read", &json!({})),
        text_response(&["done"]),
    ]));
    let first = request("go");
    let inner = Arc::new(RecordingSink::default());
    inner.steer("also check b");
    let sink = Arc::new(NoticeSink {
        inner: inner.clone(),
        notices: Mutex::new(vec![NOTICE.to_owned()]),
    });
    let scope = Arc::new(RunScope::new(
        &first,
        sink,
        parts(model.clone(), Vec::new()),
        Abort::new(),
    ));
    assert_eq!(
        super::run(scope, first).await.unwrap(),
        QueryStop::Completed
    );
    let messages = inner.messages();
    let [Message::Attachment(prompt), Message::Attachment(notice)] = &messages[2..4] else {
        panic!("expected the steering input, then the notice: {messages:?}");
    };
    assert_eq!(prompt.data["commandMode"], "prompt");
    assert_eq!(notice.source, "queued_command");
    assert_eq!(notice.text, NOTICE);
    assert_eq!(
        notice.data,
        json!({"prompt": NOTICE, "commandMode": "task-notification"})
    );
    // 不是 user 角色的 attachment：模型支持对话中间的 `role: system` 时去掉包装折进去；这里的测试模型不支持，按 user 角色带着包装跟在插话后面，两段都并进前面的工具结果
    //（context.md §3 第 9 条）。
    let requests = model.requests();
    let last = requests[1].messages.last().unwrap();
    let ContentBlock::ToolResult { content, .. } = &last.content.last().unwrap().block else {
        panic!("expected the tool result: {last:?}");
    };
    let [ToolResultContent::Text { text }] = content.as_slice() else {
        panic!("expected one text in the tool result: {content:?}");
    };
    assert!(
        text.ends_with(&format!("</system-reminder>\n\n{NOTICE}")),
        "{text}"
    );
}
