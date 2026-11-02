//! 人类输入开的回合（docs/specs/rust-cli/session-lifecycle.md §3.1 的时序）：斜杠命令展开 → `@` 提及 →
//! UserPromptSubmit → 提交输入 → 主循环，都在回合任务里，actor 照常处理中断与 `view`（工作线 F130 从 hooks.rs 移来：
//! hooks.rs 只留 hook 事件的组装与会话侧的 hook）。

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::hooks::HookOutcome;
use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::orchestration::{QueryStop, RunSink};
use rust_cli_kernel_api::session_api::KernelEvent;

use crate::actor::Actor;

impl Actor {
    /// 人类输入开的回合：UserPromptSubmit → 提交输入 → 主循环（当步 attachment 之后提交 hook 消息），都在回合
    /// 任务里（spec §3.1 的时序图）。`screened` 是插话在并入点已跑过的结果，有值时不再跑 hook。`content` 是输入的
    /// 内容块（文字，加粘贴的图片，tui-input.md §3.2）；UPS 与 `@` 提及只看文字块。
    ///
    /// `more` 是成批进这个回合的后续输入（修复线 L-A，query.md §4.3）：第一条之后按顺序各提交一条人类消息，它们在
    /// 工具边界筛查过的 hook 消息随第一条的一起交给主循环。
    pub(crate) fn start_human_turn(
        &mut self,
        command: CommandId,
        content: Vec<ContentBlock>,
        screened: Option<Box<HookOutcome>>,
        more: Vec<crate::turn::MorePrompt>,
    ) {
        let prompt = self.deps.ids.message();
        let text = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let user = Message::User(UserMessage {
            id: prompt.clone(),
            content,
            origin: UserOrigin::Human,
        });
        let turn = self.deps.ids.turn();
        self.log.append(KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: Some(command.clone()),
        });
        let hook = self.user_prompt_submit(&text);
        // `/goal` 是内置的本地命令：消息按 actor 现在的目标算好，不经 skill 展开（goal.md §2.1）。
        let goal = crate::goal::command(&text).map(|args| self.goal_inputs(args, prompt.clone()));
        let (opening, request) = self.open_main_turn(turn);
        let hooks = self.deps.hooks.clone();
        let runner = self.deps.runner.clone();
        let commands = self.deps.commands.clone();
        let ids = self.deps.ids.clone();
        let sink = opening.sink.clone();
        let context = self.command_context(&request.run, &opening.abort, sink.clone());
        let mention_context = self.command_context(&request.run, &opening.abort, sink.clone());
        let user_inputs = self.deps.inputs.clone();
        let signal = opening.abort.clone();
        let task = self.deps.spawner.spawn("query-run", async move {
            // 斜杠命令先展开（UPS 在展开之后，ext.md §2.7）；不是命令时就是这条输入本身。
            let mut inputs = match goal {
                Some(inputs) => inputs,
                None => crate::slash::expand(commands.as_ref(), ids.as_ref(), user, context).await,
            };
            // 普通输入的 `@` 提及（在 UPS 之前）：attachment 排在输入之后提交
            // （docs/specs/rust-cli/tui-input.md §1.1）。
            if inputs.plain && inputs.query {
                let mentioned = user_inputs.mentions(&text, mention_context).await;
                inputs
                    .messages
                    .extend(mentioned.into_iter().map(|attachment| {
                        Message::Attachment(AttachmentMessage {
                            id: ids.message(),
                            source: attachment.source,
                            text: attachment.text,
                            data: attachment.data,
                        })
                    }));
            }
            // 修 bug（F37）：以前展开之后、hook 之后各看一次中止，已中止就返回 `Aborted`、什么都不提交，空档里按 esc、
            // 收到退出信号或关闭时用户刚发的输入没了。处理输入之后不看中止（session-lifecycle.md §3.1）：输入照常
            // 提交，主循环在发请求之前看到中止、写中断标记（query.md §1.2）。阻塞与停止仍按 hook 的结果。
            // 退出（`Shutdown`，修复线 F38）不同：收到退出信号不中止、直接退出，这条输入还没进对话，所以什么都不写。
            let shutdown = || signal.reason() == Some(AbortReason::Shutdown);
            if shutdown() {
                return Ok(QueryStop::Aborted);
            }
            // 不调模型的本地结果（`user-invocable: false`）：只提交，不跑 UPS。
            if !inputs.query {
                for message in inputs.messages {
                    sink.commit_message(message).await?;
                }
                return Ok(QueryStop::Completed);
            }
            // 已中止时 hook 收到已取消的令牌，按取消收尾（结果里只有已完成的 hook 给出的）。
            let outcome = match screened {
                Some(outcome) => *outcome,
                None => hooks.run(hook, signal.token().clone()).await,
            };
            if shutdown() {
                return Ok(QueryStop::Aborted);
            }
            if outcome.blocking_error.is_some() {
                let message = outcome.notices.first().cloned().unwrap_or_default();
                return Ok(QueryStop::Blocked { message });
            }
            for message in &inputs.messages {
                sink.commit_message(message.clone()).await?;
            }
            let mut hook_messages = outcome.messages.clone();
            for extra in more {
                let message = Message::User(UserMessage {
                    id: ids.message(),
                    content: vec![ContentBlock::Text { text: extra.text }],
                    origin: UserOrigin::Human,
                });
                sink.commit_message(message.clone()).await?;
                inputs.messages.push(message);
                if let Some(screened) = extra.screened {
                    hook_messages.extend(screened.messages);
                }
            }
            if let Some(title) = outcome.session_title {
                sink.title(title).await;
            }
            if outcome.prevent_continuation {
                if !outcome.messages.is_empty() {
                    sink.commit_messages(outcome.messages.clone()).await?;
                }
                return Ok(QueryStop::Completed);
            }
            // 修 bug（F10）：以前 hook 消息在这里提交，排在当步 attachment（环境段等）之前；应先给输入
            // 产生当步 attachment，再追加 UPS 的结果。交给主循环在当步 attachment
            // 之后提交（query.md §1.1）。
            let mut request = request;
            request.messages.extend(inputs.messages);
            request.prompt_hook_messages = hook_messages;
            runner.run(request, sink, signal).await
        });
        self.launch(opening, task, Some(prompt), Some(command));
    }

    /// 本回合开头那条 user 消息的提交带上命令 ID（Node `anchor.sourceCommandId`）。
    pub(crate) fn command_of(&self, message: &MessageId) -> Option<CommandId> {
        self.running
            .as_ref()
            .filter(|running| running.prompt.as_ref() == Some(message))
            .and_then(|running| running.command.clone())
    }
}
