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
use crate::shell_notices::commands_of;
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

/// 推迟的保活（§5.1 第 3 条）：有 owner 是它、在跑或停放的后台子代理，或在跑的 monitor（工作线 F83，§5.3）。只在交互
/// 模式（agents.md §5.1 第 5 条）；`-p` 没有这种保活。
fn deferred(core: &Core, agents: &Agents, id: &AgentId) -> bool {
    core.keep_alive && (has_live_children(agents, id) || core.monitors.owns(id))
}

/// 命令保活要看的命令（§5.2 第 5 条，两种模式都有）：子会话里还在跑或通知未 ack 的，去掉 monitor 在看的。monitor 的
/// 命令由 monitor 的保活管（§5.3），不另算，所以 `-p` 不因 monitor 停放。
async fn commands_held(core: &Core, entry: &LiveAgent) -> Vec<AgentId> {
    let mut listed = commands_of(core, entry).await;
    listed.retain(|task| !core.monitors.watches(task.as_str()));
    listed
}

/// 命令保活（§5.2 第 5 条）：执行环境列着、还没交给它的命令（交出之后、ack 之前列表里还有它），或邮箱里还没并入的通知
/// 里有它自己命令的（交出之后可能已经 ack，列表里没有了）。
fn holds_commands(entry: &LiveAgent, listed: &[AgentId], leftover: &[Notice]) -> bool {
    listed.iter().any(|task| !entry.delivered.contains(task))
        || leftover
            .iter()
            .any(|notice| entry.delivered.contains(&notice.from))
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
        AgentState::Parked { .. } => {
            // 修正（2026-11-05）：被通知唤醒的续跑不属于派生它的那次 Agent 调用，之后的完成通知不带 `<tool-use-id>`
            // （agents.md §5）；以前这里留着它，第二次通知多一行。同 SendMessage 留下的消息续跑（`settle` 清掉它）。
            entry.tool_use = None;
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

fn notice_of(
    agent: &LiveAgent,
    ending: Ending,
    report: &Report,
    error: Option<&str>,
    keepalive: bool,
) -> Notice {
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
            keepalive,
        ),
    }
}

/// 一次运行结束（§5.1 第 3、4 条）：停放、带着遗留通知续跑，或结束并送出自己的通知。
pub(crate) async fn settle(core: &Core, id: &AgentId, finished: &Finished) -> Settle {
    let error = finished.error.as_ref().map(ToString::to_string);
    // 命令保活（§5.2 第 5 条）：锁外先问执行环境，锁里再去掉已经交给它的。命令恰好在收尾时结束也不丢不重：通知在锁里的
    // 判断之前交出 → 在邮箱里（续跑）或已在工具边界并入（记在 `delivered`，不算）；之后交出 → 它已停放（续跑）。
    let listed = match core.live.get(id) {
        Some(entry) if entry.background => commands_held(core, &entry).await,
        _ => Vec::new(),
    };
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
            AgentState::Parked { .. } | AgentState::Finished(_) => (false, Vec::new(), Vec::new()),
        };
        let stopped = aborted || finished.ending == Ending::Killed;
        // 被中止时 SendMessage 留下的消息丢弃（停止时清掉发给它的排队命令）。
        if stopped {
            stranded.clear();
        }
        // 后台子代理与 monitor 的保活推迟自己的通知（只在交互模式）。
        let deferred = deferred(core, agents, id);
        // 修 bug（2026-11-04，7 个子代理的长任务）：后台子代理起了 Docker 构建的后台命令，回复“后台任务结束会唤醒我”后
        // 停下；以前它就此结束，命令的通知按 §5.2 第 1 条送了顶层会话，子代理再也不醒，直到父代理手动 SendMessage。原因是
        // 停放只看后台子代理与 monitor，不看子会话的后台命令。现在后台子代理还有没交给它的命令时也停放（命令保活，
        // §5.2 第 5 条），命令的通知让它续跑；自己的通知不推迟，一直不结束的命令不会挡住父代理。
        // `-p` 同样这样做（用户决定 2026-11-05，agents.md §5.2 第 5 条）：以前只在交互模式，`-p` 里同样的子代理结束、
        // 命令的通知送了顶层会话。
        let commands = entry.background && holds_commands(&entry, &listed, &leftover);
        if !stopped && (deferred || commands) && stranded.is_empty() {
            let mut wakes = Vec::new();
            let mut notice = None;
            if let Some(slot) = agents.get_mut(id) {
                // 前台子代理转成后台，之后的续跑按后台工具集。
                slot.background = true;
                if leftover.is_empty() {
                    slot.state = AgentState::Parked {
                        report: finished.report.clone(),
                        notified: !deferred,
                    };
                    if !deferred {
                        notice = Some(notice_of(
                            &entry,
                            finished.ending,
                            &finished.report,
                            error.as_deref(),
                            true,
                        ));
                    }
                } else {
                    // 停放的子代理有排着的通知就立即续跑（唤醒）。先换上新的运行状态再
                    // 取快照：续跑用快照里的 Abort，TaskStop 中止的是条目里的同一个。
                    // 续跑不属于派生它的那次 Agent 调用，之后的通知不带 `<tool-use-id>`（同 `deliver`）。
                    slot.tool_use = None;
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
            if let Some(notice) = notice {
                wakes.extend(deliver(
                    agents,
                    &core.hub,
                    entry.owner.as_ref(),
                    &entry.root,
                    notice,
                ));
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
            let notice = notice_of(
                &entry,
                finished.ending,
                &finished.report,
                error.as_deref(),
                false,
            );
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

/// 一个保活原因没了（agents.md §5.3 第 3 条：最后一个 monitor 结束；§5.2 第 5 条：TaskStop 停掉了它子会话的命令）：停放着、
/// 没有别的后台子代理与 monitor 的子代理，还有没交给它的命令时留在停放，否则以最后一次运行的报告结束。推迟的通知这时送出
/// （留在停放时用 keepalive 的 note）；已经送过的（只为命令停放的）不再送。没有要做的时候为空。
pub(crate) async fn release(core: &Core, id: &AgentId) -> Vec<Wake> {
    let Some(entry) = core.live.get(id) else {
        return Vec::new();
    };
    let listed = commands_held(core, &entry).await;
    core.live.with(|agents| {
        let Some(entry) = agents.get(id).cloned() else {
            return Vec::new();
        };
        let AgentState::Parked { report, notified } = &entry.state else {
            return Vec::new();
        };
        if deferred(core, agents, id) {
            return Vec::new();
        }
        let holds = holds_commands(&entry, &listed, &[]);
        if let Some(slot) = agents.get_mut(id) {
            slot.state = if holds {
                AgentState::Parked {
                    report: report.clone(),
                    notified: true,
                }
            } else {
                AgentState::Finished(Ending::Completed)
            };
        }
        if *notified {
            return Vec::new();
        }
        let notice = notice_of(&entry, Ending::Completed, report, None, holds);
        deliver(agents, &core.hub, entry.owner.as_ref(), &entry.root, notice)
            .into_iter()
            .collect()
    })
}

/// TaskStop 停放的子代理（§5.1 第 7 条）：结束，通知带最后的报告与用量（`killedBy` 缺省
/// `user`）。不是停放状态时返回 None。
pub(crate) fn stop_parked(core: &Core, id: &AgentId) -> Option<Vec<Wake>> {
    core.live.with(|agents| {
        let entry = agents.get(id).cloned()?;
        let AgentState::Parked { report, .. } = &entry.state else {
            return None;
        };
        let notice = notice_of(&entry, Ending::KilledParked, report, None, false);
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
                    source: rust_cli_kernel_api::hooks::queued_command::TASK_NOTIFICATION
                        .to_owned(),
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
                let settled = settle(core, &agent.agent, &finished).await;
                Box::pin(wake(core, settled.wakes)).await;
            }
        }
    }
}

#[cfg(test)]
#[path = "routing_tests.rs"]
mod tests;
