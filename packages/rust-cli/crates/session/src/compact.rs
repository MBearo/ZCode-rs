//! 手动压缩（`Command::Compact`，docs/specs/rust-cli/compact.md §4.2）与压缩结果的提交。
//!
//! `/compact` 作为一个回合运行：与主循环共用 [`Running`]，所以中断、排队与收尾都走同一条路径；
//! 压缩任务经 [`ActorSink`] 把边界、摘要与附件交给 actor，在一个存储事务里提交。

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::compaction::{ContextNotice, ContextOccupancy};
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::message::{CompactTrigger, Message, SystemMessage};
use rust_cli_kernel_api::orchestration::{CompactRequest, QueryStop, RunSink};
use rust_cli_kernel_api::session_api::KernelEvent;
use rust_cli_kernel_api::store::{Fact, StoreError};
use rust_cli_kernel_api::user_command::{LocalCommandOutput, LocalCommandRecord, dim};
use tokio::sync::mpsc;

use crate::actor::{Actor, Running};

/// 压缩任务提交请求的缓冲。
const SINK_REQUESTS: usize = 16;

/// `/compact` 成功时的输出（对照实现的原文，带终端的暗色）。
const COMPACTED: &str = "Compacted (ctrl+o to see full summary)";

/// `/compact` 的命令记录：前面加固定说明（对照实现的 local 类型）。
fn compact_record(args: &str, output: LocalCommandOutput) -> LocalCommandRecord {
    LocalCommandRecord {
        name: "compact".to_owned(),
        args: args.to_owned(),
        output,
        caveat: true,
        meta: Vec::new(),
    }
}

impl Actor {
    pub(crate) fn start_compaction(
        &mut self,
        command: CommandId,
        instructions: Option<String>,
    ) -> Result<(), String> {
        if self.busy() {
            return Err("a turn is running; compact after it finishes".to_owned());
        }
        let turn = self.deps.ids.turn();
        self.log.append(KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: Some(command),
        });
        self.log.append(KernelEvent::CompactStarted {
            trigger: CompactTrigger::Manual,
        });
        // `/compact` 的命令记录（修复线 L-A，tui-manage.md §11.2）：照对照实现排在压缩结果之后、同一批（保留的消息
        // 之后接固定说明、命令输入与输出）；失败时记失败原因。
        let args = instructions.clone().unwrap_or_default();
        let ids = self.deps.ids.clone();
        let request = CompactRequest {
            session: self.meta.id.clone(),
            workspace: self.meta.workspace.clone(),
            trigger: CompactTrigger::Manual,
            selection: self.selection.clone(),
            instructions,
            messages: self.messages.clone(),
            mode: Some(self.mode()),
            agent: None,
            initial_token_gap: None,
        };
        let abort = Abort::new();
        let (sender, requests) = mpsc::channel(SINK_REQUESTS);
        let sink = self.sink(sender);
        let compactor = self.deps.compactor.clone();
        let signal = abort.clone();
        // SessionStart（`compact`）在压缩器里跑，消息已在 `restored` 末尾（compact.md §7），与边界、
        // 摘要一个事务提交；这里不再单独跑（以前只有 `/compact` 跑，自动 / 反应式压缩漏掉了）。
        let task = self.deps.spawner.spawn("compact-run", async move {
            match compactor.compact(request, signal.token().clone()).await {
                Ok(result) => {
                    let mut messages = vec![
                        Message::System(SystemMessage::CompactBoundary(result.boundary)),
                        Message::User(result.summary),
                    ];
                    messages.extend(result.restored);
                    messages.extend(crate::local_record::record_messages(
                        ids.as_ref(),
                        &compact_record(&args, LocalCommandOutput::Stdout(dim(COMPACTED))),
                    ));
                    sink.commit_messages(messages).await?;
                    Ok(QueryStop::Completed)
                }
                Err(_) if signal.is_aborted() => Ok(QueryStop::Aborted),
                // 失败原因是给用户的原文（例如 `Not enough messages to compact.`）。
                Err(error) => {
                    let message = error.to_string();
                    let record = compact_record(&args, LocalCommandOutput::Stderr(message.clone()));
                    sink.commit_messages(crate::local_record::record_messages(
                        ids.as_ref(),
                        &record,
                    ))
                    .await?;
                    Ok(QueryStop::ApiError { message })
                }
            }
        });
        self.running = Some(Running {
            turn,
            run: None,
            abort,
            requests,
            task,
            failure: None,
            prompt: None,
            command: None,
            timing: crate::duration::TurnTiming::start(
                self.deps.clock.now_ms(),
                !self.pending.is_empty(),
            ),
        });
        Ok(())
    }

    /// 一组消息一个事务（`RunSink::commit_messages`）：全部成功才进入历史并发事件；含压缩边界时
    /// 最后发 `CompactDone`。
    pub(crate) async fn record_all(&mut self, messages: Vec<Message>) -> Result<(), StoreError> {
        if let Some(reason) = self.running.as_ref().and_then(|r| r.failure.clone()) {
            return Err(StoreError::Storage { message: reason });
        }
        let facts = messages
            .iter()
            .map(|message| Fact::MessageAppended {
                message: message.clone(),
            })
            .collect();
        if let Err(reason) = self.commit(None, facts).await {
            if let Some(running) = &mut self.running {
                running
                    .abort
                    .abort(rust_cli_kernel_api::abort::AbortReason::User);
                running.failure.get_or_insert_with(|| reason.clone());
            }
            return Err(StoreError::Storage { message: reason });
        }
        let boundary = messages.iter().find_map(|message| match message {
            Message::System(SystemMessage::CompactBoundary(boundary)) => Some(boundary.id.clone()),
            _ => None,
        });
        for message in messages {
            self.goal_committed(&message);
            self.messages.push(message.clone());
            self.log.append(KernelEvent::MessageDone { message });
        }
        if let Some(boundary) = boundary {
            // 压缩后自主说明已不在上下文里：下一次 `/loop` 触发重新发全文（压缩后的清理）。
            self.reset_loop_ticks();
            self.log.append(KernelEvent::CompactDone { boundary });
            // 缺陷修复（F14）：上下文提示应在消息变化时重算；以前只在提交 assistant 时算，压缩后到下一条回复
            // 之前仍显示压缩前的百分比（compact.md §2.1）。F27 起 footer 的 ctx 同一处重算。
            self.context_changed();
        }
        Ok(())
    }

    /// 按当前历史与模型重算“上下文快满”的提示与 footer 的 ctx 并发出（压缩模块给出级别、数字与 `(token, 有效窗口)`，
    /// compact.md §2.1、tui-display.md §2.6）；压缩模块不知道的那一项不发。调用处：提交带用量的 assistant、提交压缩
    /// 结果、换模型（F27：窗口随模型变）。快照（`view` 方法）用同样两个函数。
    pub(crate) fn context_changed(&self) {
        if let Some(notice) = self.context_notice() {
            self.log.append(KernelEvent::ContextNotice { notice });
        }
        if let Some(ContextOccupancy {
            used_tokens,
            max_tokens,
        }) = self.context_occupancy()
        {
            self.log.append(KernelEvent::ContextUsage {
                used_tokens,
                max_tokens,
            });
        }
    }

    pub(crate) fn context_notice(&self) -> Option<ContextNotice> {
        self.deps
            .compactor
            .context_notice(&self.messages, &self.selection)
    }

    pub(crate) fn context_occupancy(&self) -> Option<ContextOccupancy> {
        self.deps
            .compactor
            .context_usage(&self.messages, &self.selection)
            .map(|(used_tokens, max_tokens)| ContextOccupancy {
                used_tokens,
                max_tokens,
            })
    }
}

/// 主循环开始压缩时的事件。
pub(crate) fn compact_started(actor: &Actor, trigger: CompactTrigger) {
    actor.log.append(KernelEvent::CompactStarted { trigger });
}

#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "compacted_view_tests.rs"]
mod compacted_view_tests;
