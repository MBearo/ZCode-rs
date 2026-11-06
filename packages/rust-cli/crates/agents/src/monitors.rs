//! 子代理里的 Monitor（docs/specs/rust-cli/agents.md §5.3，工作线 F83）。
//!
//! Monitor 工具在子代理里经 `ChildSink::arm_monitor` 把 monitor 交到这里；每个 monitor 一个转发任务（跟随进程），任务持有
//! 它的状态（与会话 actor 同一份纯状态机 `kernel_api::monitor_watch`，事件的分派也是它的 `on_event`），按游标读输出、在截止
//! 时刻送批与到期，事件与说明
//! 经 §5.1 的路由送出（在跑进邮箱、停放就续跑、已结束送顶层会话）。命令自己结束时，§5.2 的转发任务把子会话的终态通知
//! 交给这里（[`MonitorBoard::take`]），事件流也结束之后合成终态通知送出，再 ack。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::Origin;
use rust_cli_kernel_api::env_shell::{BackgroundStatus, BackgroundTask, ShellNotice, SocketEvent};
use rust_cli_kernel_api::ids::{AgentId, ShellTaskId};
use rust_cli_kernel_api::machine::CommandEvents;
use rust_cli_kernel_api::metering::format_duration_trimmed;
use rust_cli_kernel_api::monitor::{MonitorArmed, MonitorSource};
use rust_cli_kernel_api::monitor_watch::{Emit, Housekeeping, Next, Step, Watch};
use rust_cli_kernel_api::orchestration::OrchestrationError;
use rust_cli_prompts::monitor;
use tokio::sync::oneshot;

use crate::Core;
use crate::live::{LiveAgent, Notice};
use crate::routing;

/// 在跑的 monitor：属于哪个子代理，终态通知交给谁。
struct Slot {
    agent: AgentId,
    notice: Option<oneshot::Sender<ShellNotice>>,
}

/// 子代理的全部 monitor（只放登记与交接，状态在各自的任务里）。
#[derive(Default)]
pub(crate) struct MonitorBoard {
    slots: Mutex<HashMap<ShellTaskId, Slot>>,
}

impl MonitorBoard {
    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<ShellTaskId, Slot>> {
        self.slots.lock_unpoisoned()
    }

    /// 这个子代理还有在跑的 monitor（停放的条件，agents.md §5.3 第 3 条）。
    pub(crate) fn owns(&self, agent: &AgentId) -> bool {
        self.slots().values().any(|slot| &slot.agent == agent)
    }

    /// 这条后台命令有 monitor 在看（命令保活不算它，agents.md §5.2 第 5 条）。monitor 先 ack 终态通知再撤登记，所以
    /// 命令在执行环境里列着的时候一直认得出来。
    pub(crate) fn watches(&self, task: &str) -> bool {
        self.slots().keys().any(|watched| watched.as_str() == task)
    }

    /// 子会话的终态通知属于在看的 monitor 时交给它的任务（它送出之后自己 ack），返回 true。
    pub(crate) fn take(&self, notice: &ShellNotice) -> bool {
        let sender = self
            .slots()
            .get_mut(&notice.task)
            .and_then(|slot| slot.notice.take());
        match sender {
            Some(sender) => sender.send(notice.clone()).is_ok(),
            None => false,
        }
    }
}

/// 交到 agents：登记并起转发任务。不认识这个子代理时拒绝（调用方停掉命令）。
pub(crate) fn arm(
    core: &Arc<Core>,
    agent: &AgentId,
    armed: MonitorArmed,
    events: CommandEvents,
) -> Result<(), OrchestrationError> {
    let Some(live) = core.live.get(agent) else {
        return Err(OrchestrationError::Failed {
            message: "the subagent is gone".to_owned(),
        });
    };
    let (sender, receiver) = oneshot::channel();
    core.monitors.slots().insert(
        armed.task.clone(),
        Slot {
            agent: agent.clone(),
            notice: Some(sender),
        },
    );
    let task_core = core.clone();
    let _task = core
        .ports
        .spawner
        .clone()
        .spawn("subagent-monitor", async move {
            run(&task_core, live, armed, events, receiver).await;
        });
    Ok(())
}

/// Monitor 的说明的文字（结构化的种类换成 prompts 的原文；工作线 F131：以前经 runtime 注入的函数指针拿 query 的实现，
/// 会话一侧在 session 的 `monitors.rs`，两边等 Monitor 驱动合并后收成一份）。
fn housekeeping_text(notice: &Housekeeping) -> String {
    match notice {
        Housekeeping::Suppressed(count) => monitor::suppressed(*count),
        Housekeeping::TooMuchOutput {
            suppressed,
            seconds,
        } => monitor::too_much_output(*suppressed, *seconds),
        Housekeeping::Expired { timeout_ms, events } => {
            monitor::expired(&format_duration_trimmed(*timeout_ms), *events)
        }
        Housekeeping::Socket(SocketEvent::Error { message }) => monitor::ws_error(message),
        Housekeeping::Socket(SocketEvent::Rejected { status }) => monitor::ws_rejected(*status),
        Housekeeping::Socket(SocketEvent::Dropped { bytes }) => monitor::ws_dropped(*bytes),
        Housekeeping::Socket(SocketEvent::Closed { code, reason }) => {
            monitor::ws_closed(*code, reason)
        }
        Housekeeping::Stopped => monitor::stopped_notice().to_owned(),
    }
}

fn text(armed: &MonitorArmed, emit: &Emit) -> String {
    let body = match emit {
        Emit::Event(body) => body.clone(),
        Emit::Notice(notice) => housekeeping_text(notice),
    };
    rust_cli_prompts::monitor::event_envelope(armed.task.as_str(), &armed.description, &body)
}

/// 送给子代理（§5.1 的路由）。
async fn deliver(
    core: &Arc<Core>,
    live: &LiveAgent,
    task: &ShellTaskId,
    status: BackgroundStatus,
    envelope: String,
) {
    // 通知的来源标识是任务 ID（同子代理里的命令通知，shell_notices.rs）。
    let notice = Notice {
        from: AgentId::new(task.as_str()),
        status,
        envelope,
    };
    let wake = core
        .live
        .with(|agents| routing::deliver(agents, &core.hub, Some(&live.agent), &live.root, notice));
    routing::wake(core, wake.into_iter().collect()).await;
}

async fn send(core: &Arc<Core>, live: &LiveAgent, armed: &MonitorArmed, step: &Step) {
    for emit in &step.emits {
        deliver(
            core,
            live,
            &armed.task,
            BackgroundStatus::Running,
            text(armed, emit),
        )
        .await;
    }
    if step.stop {
        stop(core, live, armed).await;
    }
}

/// 到期或输出太多：停掉命令（之后没有终态通知）。
async fn stop(core: &Core, live: &LiveAgent, armed: &MonitorArmed) {
    // 修正（工作线 F130）：以前 agents 没有执行环境的 generation，这里写死 `subagent-monitor`；现在与会话 actor
    // 一样带内核的（`AgentsPorts::operations`，README §6 规则 3）。
    let context = core.ports.operations.next(
        live.workspace.environment.clone(),
        Origin::User {
            session: Some(live.child.clone()),
        },
    );
    let request = BackgroundTask {
        session: live.child.clone(),
        task: armed.task.clone(),
    };
    if let Err(error) = core
        .ports
        .machine_named(&armed.machine)
        .background()
        .stop_background_command(context, request)
        .await
    {
        tracing::warn!(%error, task = %armed.task, "a subagent monitor could not be stopped");
    }
}

async fn sleep_until(at: Option<u64>, now: u64) {
    match at {
        Some(at) => tokio::time::sleep(Duration::from_millis(at.saturating_sub(now))).await,
        None => std::future::pending().await,
    }
}

async fn run(
    core: &Arc<Core>,
    live: LiveAgent,
    armed: MonitorArmed,
    mut events: CommandEvents,
    notice: oneshot::Receiver<ShellNotice>,
) {
    let mut watch = Watch::new(armed.clone(), core.ports.clock.now_ms());
    // monitor 所在的机器（R3）：查询、停止、ack 与流出错后的续读都发给它。输出读 Monitor 工具交来的那条流（F119）。
    let machine = core.ports.machine_named(&armed.machine);
    let socket = armed.source == MonitorSource::Socket;
    loop {
        let now = core.ports.clock.now_ms();
        let deadline = watch.deadline();
        tokio::select! {
            event = events.next() => {
                // 流自己结束（没有 `Finished`）同事件流结束。
                let Some(event) = event else { break };
                if let Err(error) = &event {
                    tracing::warn!(%error, task = %armed.task, "a subagent monitor stream failed");
                }
                // 分派与会话 actor 同一份（`Watch::on_event`）：续读按游标去重，出错到上限按结束处理。
                match watch.on_event(event, core.ports.clock.now_ms()) {
                    Next::Step(step) => send(core, &live, &armed, &step).await,
                    Next::Resubscribe(cursor) => {
                        events = machine.commands().watch_command(armed.operation.clone(), cursor);
                    }
                    Next::Ended => break,
                }
            }
            () = sleep_until(deadline, now) => {
                let step = watch.on_due(core.ports.clock.now_ms());
                send(core, &live, &armed, &step).await;
            }
        }
    }
    let rest = watch.finish();
    let key = BackgroundTask {
        session: live.child.clone(),
        task: armed.task.clone(),
    };
    let pending = matches!(
        machine.background().background_command(key).await,
        Ok(Some(_))
    );
    let now = core.ports.clock.now_ms();
    if pending && let Ok(notice) = notice.await {
        // 命令自己结束：合成终态通知（剩下的行拿得到令牌就放进正文）；ws 来源只 ack（关闭说明已经送过）。
        if !socket {
            let mut step = Step::default();
            let events = rest.filter(|_| watch.admit_rest(now, &mut step));
            send(core, &live, &armed, &step).await;
            let status = notice.status();
            let envelope = rust_cli_prompts::notification::monitor_envelope(
                &notice.task_notice(),
                events.as_deref(),
            );
            deliver(core, &live, &armed.task, status, envelope).await;
        }
        if let Err(error) = machine
            .shell()
            .ack_shell_notice(&live.child, notice.seq)
            .await
        {
            tracing::warn!(%error, "a subagent monitor notice was not acknowledged");
        }
    } else {
        let step = watch.after_stop(rest, socket, now);
        send(core, &live, &armed, &step).await;
    }
    core.monitors.slots().remove(&armed.task);
    // 最后一个 monitor 结束：停放着、没有别的后台工作的子代理就此结束（§5.3 第 3 条）。
    let wakes = routing::release(core, &live.agent).await;
    routing::wake(core, wakes).await;
}

#[cfg(test)]
#[path = "monitors_tests.rs"]
mod tests;
