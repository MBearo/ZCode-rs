//! 子代理通知的去向（docs/specs/rust-cli/agents.md §5.1）：收件人、keepalive、停放后的唤醒与送顶层会话。
//!
//! 判断与转移都在 `live::Live` 的一把锁里完成；送顶层会话时在同一把锁里交给 `signals::Hub`（锁的顺序总是 Live →
//! Hub，没有反过来的）。所以 `-p` 判断“没有后台工作”时，一条通知要么还挂在某个在跑或停放的子代理
//! 上，要么已在 `signals::Hub` 里等 ack，没有两头都看不到的空档。要续跑的子代理在锁里先转成 Running，锁外再
//! 提交续跑消息、启动后台运行。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::{AgentId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::UserOrigin;
use rust_cli_kernel_api::orchestration::QueryError;
use rust_cli_prompts::agents as texts;

use crate::Core;
use crate::live::{AgentState, Agents, LiveAgent, Mail, Notice};
use crate::report::{self, Ending, Report};
use crate::signals::Hub;
use crate::spawn::{self, Finished};

/// 续跑时的第一条消息。
enum Resume {
    /// 发给它的通知（信封，按到达顺序）：一条 meta user 消息（来源 `task-notification`）。
    Notices(Vec<String>),
    /// SendMessage 排进来、上次运行没取走的消息（stranded）：人类来源的 user 消息。
    Message(String),
}

/// 要续跑的子代理与它的第一条消息。
pub(crate) struct Wake {
    agent: LiveAgent,
    resume: Resume,
}

/// 一次运行结束后的去向。
pub(crate) struct Settle {
    /// 没有结束：停放，或带着遗留的通知立即续跑（前台调用都回“已启动”）。
    pub(crate) parked: bool,
    pub(crate) wakes: Vec<Wake>,
}

/// 还有 owner 是 `owner`、在跑或停放的后台子代理（`agent:<id>` keepalive）。
fn has_live_children(agents: &Agents, owner: &AgentId) -> bool {
    agents
        .values()
        .any(|agent| agent.owner.as_ref() == Some(owner) && agent.background && agent.is_live())
}

fn to_root(hub: &Hub, root: &SessionId, notice: Notice) {
    let text = texts::notification_wrap(&notice.envelope);
    hub.notify(root, notice.from, notice.status, text);
}

/// 把通知送给 owner（§5.1 第 2 条）：在跑进邮箱；停放就续跑；已结束、不存在或没有 owner 时送顶层会话。
pub(crate) fn deliver(
    agents: &mut Agents,
    hub: &Hub,
    owner: Option<&AgentId>,
    root: &SessionId,
    notice: Notice,
) -> Option<Wake> {
    let Some(entry) = owner.and_then(|owner| agents.get_mut(owner)) else {
        to_root(hub, root, notice);
        return None;
    };
    match &mut entry.state {
        AgentState::Running { mailbox, .. } => {
            mailbox.push(Mail::Notice(notice));
            None
        }
        AgentState::Parked(_) => {
            entry.state = AgentState::Running {
                abort: Abort::new(),
                mailbox: Vec::new(),
            };
            Some(Wake {
                agent: entry.clone(),
                resume: Resume::Notices(vec![notice.envelope]),
            })
        }
        AgentState::Finished(_) => {
            to_root(hub, root, notice);
            None
        }
    }
}

fn notice_of(agent: &LiveAgent, ending: Ending, report: &Report, error: Option<&str>) -> Notice {
    Notice {
        from: agent.agent.clone(),
        status: ending.status(),
        envelope: report::envelope(
            agent.agent.as_str(),
            agent.tool_use.as_ref().map(ToolUseId::as_str),
            &agent.description,
            ending,
            report,
            error,
        ),
    }
}

/// 一次运行结束（§5.1 第 3、4 条）：停放、带着遗留通知续跑，或结束并送出自己的通知。
pub(crate) fn settle(core: &Core, id: &AgentId, finished: &Finished) -> Settle {
    let error = finished.error.as_ref().map(ToString::to_string);
    core.live.with(|agents| {
        let Some(entry) = agents.get(id).cloned() else {
            return Settle {
                parked: false,
                wakes: Vec::new(),
            };
        };
        let (aborted, leftover, mut stranded) = match &entry.state {
            AgentState::Running { abort, mailbox } => {
                let mut notices = Vec::new();
                let mut messages = Vec::new();
                for mail in mailbox {
                    match mail {
                        Mail::Notice(notice) => notices.push(notice.clone()),
                        Mail::Message(text) => messages.push(text.clone()),
                    }
                }
                (abort.is_aborted(), notices, messages)
            }
            AgentState::Parked(_) | AgentState::Finished(_) => (false, Vec::new(), Vec::new()),
        };
        let stopped = aborted || finished.ending == Ending::Killed;
        // 被中止时 SendMessage 留下的消息丢弃（停止时清掉发给它的排队命令）。
        if stopped {
            stranded.clear();
        }
        if !stopped && core.keep_alive && has_live_children(agents, id) && stranded.is_empty() {
            let mut wakes = Vec::new();
            if let Some(slot) = agents.get_mut(id) {
                // 前台子代理转成后台，之后的续跑按后台工具集。
                slot.background = true;
                if leftover.is_empty() {
                    slot.state = AgentState::Parked(finished.report.clone());
                } else {
                    // 停放的子代理有排着的通知就立即续跑（唤醒）。先换上新的运行状态再
                    // 取快照：续跑用快照里的 Abort，TaskStop 中止的是条目里的同一个。
                    slot.state = AgentState::Running {
                        abort: Abort::new(),
                        mailbox: Vec::new(),
                    };
                    wakes.push(Wake {
                        agent: slot.clone(),
                        resume: Resume::Notices(
                            leftover.into_iter().map(|notice| notice.envelope).collect(),
                        ),
                    });
                }
            }
            return Settle {
                parked: true,
                wakes,
            };
        }
        // 结束：没来得及并入的通知送顶层会话，再送出自己的通知。
        for notice in leftover {
            to_root(&core.hub, &entry.root, notice);
        }
        if let Some(slot) = agents.get_mut(id) {
            slot.state = AgentState::Finished(finished.ending);
        }
        let mut wakes = Vec::new();
        if entry.background {
            let notice = notice_of(&entry, finished.ending, &finished.report, error.as_deref());
            wakes.extend(deliver(
                agents,
                &core.hub,
                entry.owner.as_ref(),
                &entry.root,
                notice,
            ));
        }
        // 修正（L18）：以前 SendMessage 排进来、运行结束前没在工具边界取走的消息随运行结束丢了。现在
        // 完成时 `pendingMessages` 不空就用第一条续跑（后台），其余留给
        // 续跑的工具边界；自己的通知照常先送出（每次停下都通知）。
        if !stranded.is_empty()
            && let Some(slot) = agents.get_mut(id)
        {
            let first = stranded.remove(0);
            slot.background = true;
            slot.tool_use = None;
            slot.state = AgentState::Running {
                abort: Abort::new(),
                mailbox: stranded.into_iter().map(Mail::Message).collect(),
            };
            wakes.push(Wake {
                agent: slot.clone(),
                resume: Resume::Message(first),
            });
        }
        Settle {
            parked: false,
            wakes,
        }
    })
}

/// TaskStop 停放的子代理（§5.1 第 7 条）：结束，通知带最后的报告与用量（`killedBy` 缺省
/// `user`）。不是停放状态时返回 None。
pub(crate) fn stop_parked(core: &Core, id: &AgentId) -> Option<Vec<Wake>> {
    core.live.with(|agents| {
        let entry = agents.get(id).cloned()?;
        let AgentState::Parked(report) = &entry.state else {
            return None;
        };
        let notice = notice_of(&entry, Ending::KilledParked, report, None);
        if let Some(slot) = agents.get_mut(id) {
            slot.state = AgentState::Finished(Ending::KilledParked);
        }
        Some(
            deliver(agents, &core.hub, entry.owner.as_ref(), &entry.root, notice)
                .into_iter()
                .collect(),
        )
    })
}

/// 续跑：在子会话里提交第一条消息（通知是 meta、来源 `task-notification`；留下的 SendMessage 消息是人类
/// 来源），再在后台跑。提交失败时按失败结束，通知照常送出。
pub(crate) async fn wake(core: &Arc<Core>, wakes: Vec<Wake>) {
    for Wake { agent, resume } in wakes {
        let (prompt, origin) = match resume {
            Resume::Notices(envelopes) => (
                texts::notification_wrap(&envelopes.join("\n\n")),
                UserOrigin::Meta {
                    source: "task-notification".to_owned(),
                },
            ),
            // 续跑时按来自父代理的消息包一层说明（`The coordinator sent a message while you were
            // working:` …），作为 user 消息。
            Resume::Message(text) => (
                rust_cli_prompts::texts::render(texts::stranded_message(), &[("message", &text)]),
                UserOrigin::Human,
            ),
        };
        match spawn::resume(core, &agent, &prompt, origin).await {
            Ok(opening) => spawn::background(core, agent, opening),
            Err(error) => {
                tracing::warn!(agent = %agent.agent, %error, "the parked agent could not resume");
                let finished = Finished {
                    ending: Ending::Failed,
                    report: Report::default(),
                    error: Some(QueryError::Store(error)),
                };
                let settled = settle(core, &agent.agent, &finished);
                Box::pin(wake(core, settled.wakes)).await;
            }
        }
    }
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
