//! ← 会话面板的数据（docs/specs/rust-cli/tui-panels.md §2.1）：本工作区的会话列表（与 `/resume` 同一个来源），
//! 合并本进程里打开着的会话的实时状态。实时状态问每个会话 actor 的 `view()`（它是会话状态的唯一 owner）；
//! 别的进程里在跑的会话这里看不到，只能是 Inactive（D3）。

use std::collections::HashMap;

use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::panels::{SessionActivity, WorkspaceSession};
use rust_cli_kernel_api::session_api::{SessionHandle, SessionView};

use super::*;

/// 打开着的会话的状态：有待处理交互 → 需要你；有回合或后台工作 → 运行中；否则就绪。
pub(crate) fn activity(view: &SessionView) -> SessionActivity {
    if !view.pending.is_empty() {
        SessionActivity::NeedsInput
    } else if view.running || !view.background.is_empty() {
        SessionActivity::Working
    } else {
        SessionActivity::Ready
    }
}

/// 最后一条 assistant 消息的文字。
pub(crate) fn last_reply(view: &SessionView) -> Option<String> {
    view.messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(assistant) => {
                let text: Vec<&str> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } if !text.trim().is_empty() => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect();
                (!text.is_empty()).then(|| text.join("\n"))
            }
            _ => None,
        })
}

impl LocalKernel {
    pub(crate) async fn overview(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<WorkspaceSession>, KernelError> {
        let workspace = filter.workspace.clone();
        let summaries = self.list_sessions(filter).await?;
        // 先在锁里取出句柄，放锁后再逐个问（view 要等 actor 回复）。只合并打开着的：加载中、关闭中的按库里的行。
        let open: Vec<Arc<Session>> = self
            .sessions()
            .values()
            .filter_map(crate::table::Slot::open)
            .cloned()
            .collect();
        let mut live: HashMap<SessionId, (SessionActivity, Option<String>, SessionMeta)> =
            HashMap::new();
        for session in open {
            let Ok(view) = session.view().await else {
                continue;
            };
            if view.meta.workspace != workspace || view.meta.kind != SessionKind::Interactive {
                continue;
            }
            live.insert(
                view.meta.id.clone(),
                (activity(&view), last_reply(&view), view.meta.clone()),
            );
        }
        let mut sessions: Vec<WorkspaceSession> = summaries
            .into_iter()
            .map(|summary| {
                let (activity, last_reply) = match live.remove(&summary.id) {
                    Some((activity, reply, _)) => (Some(activity), reply),
                    None => (None, None),
                };
                WorkspaceSession {
                    summary,
                    activity,
                    last_reply,
                }
            })
            .collect();
        // 打开着、但不在列表里的（超出条数上限）也列出来。
        for (id, (activity, last_reply, meta)) in live {
            sessions.push(WorkspaceSession {
                summary: SessionSummary {
                    id,
                    title: meta.title,
                    updated_at_ms: meta.created_at_ms,
                },
                activity: Some(activity),
                last_reply,
            });
        }
        Ok(sessions)
    }
}

#[cfg(test)]
#[path = "workspace_sessions_tests.rs"]
mod tests;
