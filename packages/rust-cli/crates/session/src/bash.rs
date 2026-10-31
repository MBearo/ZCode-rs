//! bash 模式的回合与带内容块的输入（docs/specs/rust-cli/tui-input.md §2.2、§3.2）：经 [`UserInputs::bash`]
//! 执行，记 `<bash-input>` 与输出两条人类消息后调模型；被中断时
//! 记 caveat、输入与中断消息，不调模型。
//!
//! 运行中提交的 bash 模式输入与带图片的输入只进 `Queue`（D9），它们与普通文字输入的差别记在 `queued_kinds`。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::orchestration::{QueryRequest, QueryStop, RunSink};
use rust_cli_kernel_api::session_api::{Delivery, KernelEvent, QueuedInput};
use rust_cli_kernel_api::user_command::LOCAL_COMMAND_CAVEAT_SOURCE;
use rust_cli_kernel_api::user_input::{BASH_CAVEAT, bash_input, bash_output};
use tokio::sync::mpsc;

use crate::actor::{Actor, Running};

/// 中断后的人类消息（工具外的那一条，kernel-api 的定义，F55）。
const INTERRUPTED: &str = rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[0];
// 主循环提交请求的缓冲（与人类输入的回合相同，F55）。
use crate::turn::SINK_REQUESTS;

/// 排队输入里不是普通文字的那些（按命令 ID）。
pub(crate) enum QueuedKind {
    /// bash 模式的命令原文。
    Bash(String),
    /// 带内容块的输入（粘贴的图片）。
    Content(Vec<ContentBlock>),
}

fn user(id: rust_cli_kernel_api::ids::MessageId, text: String, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id,
        content: vec![ContentBlock::Text { text }],
        origin,
    })
}

impl Actor {
    /// `Command::Bash`：空闲时开回合，运行中排到回合结束后（D9）。
    pub(crate) fn bash_command(&mut self, command: CommandId, line: String) {
        if self.busy() {
            self.queued_kinds
                .insert(command.clone(), QueuedKind::Bash(line.clone()));
            self.queue.push_back(QueuedInput {
                command,
                text: bash_input(&line),
                delivery: Delivery::Queue,
            });
            self.emit_queue();
            return;
        }
        self.start_bash_turn(command, line);
    }

    /// `Command::SubmitContent`：空闲时开回合，运行中排到回合结束后（D9）。
    pub(crate) async fn content_command(
        &mut self,
        command: CommandId,
        content: Vec<ContentBlock>,
    ) -> Result<(), String> {
        if self.busy() {
            let text = content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.queued_kinds
                .insert(command.clone(), QueuedKind::Content(content));
            self.queue.push_back(QueuedInput {
                command,
                text,
                delivery: Delivery::Queue,
            });
            self.emit_queue();
            return Ok(());
        }
        self.start_turn(crate::turn::TurnInput::Content { command, content })
            .await
    }

    pub(crate) fn start_bash_turn(&mut self, command: CommandId, line: String) {
        let prompt = self.deps.ids.message();
        let turn = self.deps.ids.turn();
        self.log.append(KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: Some(command.clone()),
        });
        let abort = Abort::new();
        let (sender, requests) = mpsc::channel(SINK_REQUESTS);
        let run = self.deps.ids.run();
        let request = QueryRequest {
            session: self.meta.id.clone(),
            run: run.clone(),
            workspace: self.meta.workspace.clone(),
            agent: None,
            messages: self.messages.clone(),
            selection: self.selection.clone(),
            mode: self.mode(),
            source: QuerySource::MainThread,
            max_turns: self.deps.max_turns,
            prompt_hook_messages: Vec::new(),
        };
        let sink = Arc::new(self.sink(sender));
        let context = self.command_context(&request.run, &abort, sink.clone());
        let inputs = self.deps.inputs.clone();
        let runner = self.deps.runner.clone();
        let ids = self.deps.ids.clone();
        let signal = abort.clone();
        let input_id = prompt.clone();
        let task = self.deps.spawner.spawn("bash-mode", async move {
            // 执行的进度不进对话（TUI 看回合行的运行状态）。
            let (progress, _) = mpsc::channel(1);
            let run = inputs.bash(&line, context, progress).await;
            let input = user(input_id, bash_input(&line), UserOrigin::Human);
            let Some(output) = bash_output(&run).filter(|_| !signal.is_aborted()) else {
                // 被中断：caveat、输入与中断消息（不调模型）。
                let caveat = user(
                    ids.message(),
                    BASH_CAVEAT.to_owned(),
                    UserOrigin::Meta {
                        source: LOCAL_COMMAND_CAVEAT_SOURCE.to_owned(),
                    },
                );
                let interrupted = user(ids.message(), INTERRUPTED.to_owned(), UserOrigin::Human);
                // 逐条提交：输入是本回合的 prompt（checkpoint 与首条输入的标题认它）。
                for message in [caveat, input, interrupted] {
                    sink.commit_message(message).await?;
                }
                return Ok(QueryStop::Aborted);
            };
            let output = user(ids.message(), output, UserOrigin::Human);
            sink.commit_message(input.clone()).await?;
            sink.commit_message(output.clone()).await?;
            // 缺省回应 bash 命令：两条消息之后调模型（不跑 UPS）。
            let mut request = request;
            request.messages.extend([input, output]);
            runner.run(request, sink, signal).await
        });
        self.running = Some(Running {
            turn,
            run: Some(run),
            abort,
            requests,
            task,
            failure: None,
            prompt: Some(prompt),
            command: Some(command),
            timing: crate::duration::TurnTiming::start(
                self.deps.clock.now_ms(),
                !self.pending.is_empty(),
            ),
        });
    }
}
