//! 压缩里的 hooks 顺序与 SessionStart（`compact`）的消息（compact.md §7）。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::ids::{AgentId, MessageId};
use rust_cli_kernel_api::message::{AttachmentMessage, CompactTrigger, Message};
use rust_cli_kernel_api::orchestration::{HookEvent, HookEventName, HookOutcome, HookRunner};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::engine::Engine;
use crate::test_support::{Reply, ScriptedRunner, human, no_tools, ports, reply, request};

#[derive(Default)]
struct Recorded(Mutex<Vec<HookEvent>>);

#[async_trait]
impl HookRunner for Recorded {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        let name = event.name;
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
        if name != HookEventName::SessionStart {
            return HookOutcome::default();
        }
        HookOutcome {
            messages: vec![Message::Attachment(AttachmentMessage {
                id: MessageId::new("hook_ctx"),
                source: "hook_additional_context".to_owned(),
                text: "<system-reminder>\nSessionStart hook additional context: again\n</system-reminder>".to_owned(),
                data: Value::Null,
            })],
            ..HookOutcome::default()
        }
    }
}

#[tokio::test]
async fn session_start_runs_inside_every_compaction_before_post_compact() {
    for trigger in [
        CompactTrigger::Manual,
        CompactTrigger::Auto,
        CompactTrigger::Reactive,
    ] {
        let hooks = Arc::new(Recorded::default());
        let runner = ScriptedRunner::new(vec![Reply::Text("<summary>S</summary>".to_owned())]);
        let mut ports = ports(runner, no_tools());
        ports.hooks = hooks.clone();
        let mut request = request(vec![human("u1", "hi"), reply("a1", "ok", None)], trigger);
        request.agent = Some(AgentId::new("agent_1"));
        let result = Engine { ports: &ports }
            .run(&request, true, &CancellationToken::new())
            .await
            .unwrap();
        let seen = hooks
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let names: Vec<HookEventName> = seen.iter().map(|event| event.name).collect();
        assert_eq!(
            names,
            [
                HookEventName::PreCompact,
                HookEventName::SessionStart,
                HookEventName::PostCompact
            ]
        );
        let start = &seen[1];
        // 对 compact 不传标题：只有 source 与 model；子代理里带 agent_id。
        assert_eq!(
            start.payload,
            json!({"source": "compact", "model": GLM_5_3})
        );
        assert_eq!(start.context.agent, Some(AgentId::new("agent_1")));
        // hook 消息排在附件之后，与边界、摘要一起交给调用方提交。
        assert_eq!(
            result.restored.last().map(|message| message.id().as_str()),
            Some("hook_ctx")
        );
    }
}
