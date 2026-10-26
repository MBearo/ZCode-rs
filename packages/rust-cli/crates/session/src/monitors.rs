//! Monitor 的事件（docs/specs/rust-cli/tools-more.md §19.6）：主线程的 monitor 由会话 actor 持有。
//!
//! Monitor 工具起好后台命令后经 `Interactions::arm_monitor` 交到这里（[`Actor::arm_monitor`]）。切行、批、限流与到期是
//! 纯状态机 `kernel_api::monitor_watch`（子代理的 monitor 在 agents 里用同一份）；这里只做 IO 与送达：按游标从
//! `watch_command(operation, cursor)` 读 stdout（流出错时从记下的游标重新订阅），批的截止时刻与到期是 `select!` 里的
//! 一支（[`deadline`]）。事件与说明直接排进 next 通道（`Waiting::MonitorEvent`，不需要 ack）；命令自己结束时，事件流的
//! `Finished` 与执行环境的终态通知先后不定，两者都到了才合成终态通知（`Waiting::MonitorEnd`，提交后 ack）。

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use futures_util::StreamExt;
use futures_util::stream::{BoxStream, SelectAll};
use rust_cli_kernel_api::env::{CommandEvent, EnvError, OperationContext, Origin, OutputStream};
use rust_cli_kernel_api::env_shell::BackgroundTask;
use rust_cli_kernel_api::ids::{OwnerEpoch, ShellTaskId};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::machine_notices::MachineNotice;
use rust_cli_kernel_api::monitor::{MonitorArmed, MonitorSource};
use rust_cli_kernel_api::monitor_watch::{Emit, Step, Watch};
use rust_cli_kernel_api::orchestration::OrchestrationError;
use rust_cli_kernel_api::session_api::KernelEvent;

use crate::actor::Actor;
use crate::lane::{Lane, Waiting};

/// 流连续出错这么多次就不再重新订阅（这个 monitor 之后只等终态通知）。
const STREAM_RETRIES: u8 = 3;

type Tagged = BoxStream<'static, (ShellTaskId, Result<CommandEvent, EnvError>)>;

/// 一个 monitor 的记录。
struct Entry {
    watch: Watch,
    /// 事件流到了 `Finished`：剩下的行（可能没有）。
    ended: Option<Option<String>>,
    /// 执行环境的终态通知先到了（带机器名）。
    notice: Option<MachineNotice>,
    errors: u8,
}

/// actor 里全部 monitor 的状态。
#[derive(Default)]
pub(crate) struct Monitors {
    entries: HashMap<ShellTaskId, Entry>,
    /// 各 monitor 的输出流。包一层 Mutex 只是让 Actor 仍是 `Sync`（同 `Watched`）。
    streams: Mutex<SelectAll<Tagged>>,
}

impl Monitors {
    /// 最近的截止时刻（批或到期）。
    pub(crate) fn next_deadline(&self) -> Option<u64> {
        self.entries
            .values()
            .filter_map(|entry| entry.watch.deadline())
            .min()
    }

    fn subscribe(&self, stream: Tagged) {
        self.streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(stream);
    }
}

/// 下一个输出事件；没有 monitor 时一直等。
pub(crate) async fn next_event(
    monitors: &mut Monitors,
) -> (ShellTaskId, Result<CommandEvent, EnvError>) {
    let streams = monitors
        .streams
        .get_mut()
        .unwrap_or_else(PoisonError::into_inner);
    if streams.is_empty() {
        return std::future::pending().await;
    }
    match streams.next().await {
        Some(item) => item,
        None => std::future::pending().await,
    }
}

/// 截止时刻（批或到期，[`Monitors::next_deadline`]，时钟的毫秒数）到了；没有时一直等。醒来后按时钟判断哪些到点了。
pub(crate) async fn deadline(at: Option<u64>, now_ms: u64) {
    match at {
        Some(at) => {
            tokio::time::sleep(std::time::Duration::from_millis(at.saturating_sub(now_ms))).await;
        }
        None => std::future::pending().await,
    }
}

/// 事件或说明的全文：信封外面的说明与包装同后台命令通知。
fn emit_text(armed: &MonitorArmed, emit: &Emit) -> String {
    let body = match emit {
        Emit::Event(body) => body.clone(),
        Emit::Notice(notice) => rust_cli_query::monitor_notice_text(notice),
    };
    rust_cli_prompts::agents::notification_wrap(&rust_cli_prompts::monitor::event_envelope(
        armed.task.as_str(),
        &armed.description,
        &body,
    ))
}

fn watched_stream(actor: &Actor, armed: &MonitorArmed, cursor: u64) -> Tagged {
    let task = armed.task.clone();
    actor
        .monitor_machine(armed)
        .commands()
        .watch_command(armed.operation.clone(), cursor)
        .map(move |event| (task.clone(), event))
        .boxed()
}

impl Actor {
    /// monitor 所在的机器（R3）：本机，或 `armed.machine` 那台；没连上时是“没连上”的机器（流报错，按重订阅的规则放弃）。
    fn monitor_machine(&self, armed: &MonitorArmed) -> MachineEnv {
        match &armed.machine {
            None => self.deps.machine.clone(),
            Some(name) => self
                .deps
                .machines
                .machine(name)
                .unwrap_or_else(|| MachineEnv::unavailable(name)),
        }
    }

    /// Monitor 工具起好的 monitor（[`rust_cli_kernel_api::orchestration::Interactions::arm_monitor`]）。
    pub(crate) fn arm_monitor(&mut self, armed: MonitorArmed) -> Result<(), OrchestrationError> {
        if self.closing.is_some() {
            return Err(OrchestrationError::Failed {
                message: "the session is closing".to_owned(),
            });
        }
        let stream = watched_stream(self, &armed, 0);
        self.monitors.subscribe(stream);
        self.monitors.entries.insert(
            armed.task.clone(),
            Entry {
                watch: Watch::new(armed, self.deps.clock.now_ms()),
                ended: None,
                notice: None,
                errors: 0,
            },
        );
        Ok(())
    }

    /// 输出流的一个事件。
    pub(crate) async fn on_monitor_event(
        &mut self,
        task: ShellTaskId,
        event: Result<CommandEvent, EnvError>,
    ) {
        let now = self.deps.clock.now_ms();
        let Some(entry) = self.monitors.entries.get_mut(&task) else {
            return;
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                // 流断了：从记下的游标重新订阅，读过的不再送一遍。
                entry.errors += 1;
                tracing::warn!(%error, task = %task, "monitor output stream failed");
                if entry.errors <= STREAM_RETRIES {
                    let armed = entry.watch.armed.clone();
                    let cursor = entry.watch.cursor;
                    let stream = watched_stream(self, &armed, cursor);
                    self.monitors.subscribe(stream);
                }
                return;
            }
        };
        entry.errors = 0;
        match event {
            CommandEvent::Output {
                stream: OutputStream::Stdout,
                data,
                cursor,
            } => {
                // 重新订阅时游标之前的不再处理。
                if cursor > entry.watch.cursor {
                    let fresh = usize::try_from(cursor - entry.watch.cursor).unwrap_or(data.len());
                    let start = data.len().saturating_sub(fresh);
                    entry.watch.on_data(&data[start..], now);
                    entry.watch.cursor = cursor;
                }
            }
            CommandEvent::Output {
                stream: OutputStream::Stderr,
                data,
                ..
            } => {
                if entry.watch.armed.source == MonitorSource::Socket {
                    let step = entry.watch.on_lifecycle(&data, now);
                    self.emit(&task, step).await;
                }
            }
            CommandEvent::Started { .. }
            | CommandEvent::Backgrounded
            | CommandEvent::BackgroundedBy { .. } => {}
            CommandEvent::Finished { .. } => self.monitor_ended(task).await,
        }
    }

    /// 截止时刻到了：送出到点的批，处理到期。
    pub(crate) async fn on_monitor_due(&mut self) {
        let now = self.deps.clock.now_ms();
        let mut steps = Vec::new();
        for (task, entry) in &mut self.monitors.entries {
            let step = entry.watch.on_due(now);
            if !step.emits.is_empty() || step.stop {
                steps.push((task.clone(), step));
            }
        }
        for (task, step) in steps {
            self.emit(&task, step).await;
        }
    }

    /// 把通知排进 next 通道；要停时停掉命令（之后没有终态通知）。
    async fn emit(&mut self, task: &ShellTaskId, step: Step) {
        let Some(entry) = self.monitors.entries.get(task) else {
            return;
        };
        let armed = entry.watch.armed.clone();
        for emit in &step.emits {
            self.lane
                .push_back(Lane::Notice(Waiting::MonitorEvent(emit_text(&armed, emit))));
        }
        if step.stop {
            self.stop_monitor(&armed).await;
        }
        if self.running.is_none() && !self.lane.is_empty() {
            self.start_next().await;
        }
    }

    async fn stop_monitor(&self, armed: &MonitorArmed) {
        let context = OperationContext {
            environment: self.meta.workspace.environment.clone(),
            generation: self.deps.generation.clone(),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: self.deps.ids.operation(),
            origin: Origin::User {
                session: Some(self.meta.id.clone()),
            },
        };
        let request = BackgroundTask {
            session: self.meta.id.clone(),
            task: armed.task.clone(),
        };
        if let Err(error) = self
            .monitor_machine(armed)
            .background()
            .stop_background_command(context, request)
            .await
        {
            tracing::warn!(%error, task = %armed.task, "the monitor could not be stopped");
        }
    }

    /// 事件流结束：取出剩下的行。命令被停掉（执行环境已不认识它）时剩下的行作为普通事件送；否则等终态通知。
    async fn monitor_ended(&mut self, task: ShellTaskId) {
        let Some(entry) = self.monitors.entries.get_mut(&task) else {
            return;
        };
        let rest = entry.watch.finish();
        let armed = entry.watch.armed.clone();
        let machine = self.monitor_machine(&armed);
        let key = BackgroundTask {
            session: self.meta.id.clone(),
            task: task.clone(),
        };
        let pending = matches!(
            machine.background().background_command(key).await,
            Ok(Some(_))
        );
        let now = self.deps.clock.now_ms();
        let Some(entry) = self.monitors.entries.get_mut(&task) else {
            return;
        };
        if pending || entry.notice.is_some() {
            entry.ended = Some(rest);
            if entry.notice.is_some() {
                self.monitor_settled(task).await;
            }
            return;
        }
        let socket = entry.watch.armed.source == MonitorSource::Socket;
        let step = entry.watch.after_stop(rest, socket, now);
        self.emit(&task, step).await;
        self.monitors.entries.remove(&task);
    }

    /// 终态通知到了：属于在看的 monitor 时返回 true（流已结束就合成，否则记下等它）。
    pub(crate) async fn monitor_notice(&mut self, notice: &MachineNotice) -> bool {
        let Some(entry) = self.monitors.entries.get_mut(&notice.notice.task) else {
            return false;
        };
        entry.notice = Some(notice.clone());
        if entry.ended.is_some() {
            self.monitor_settled(notice.notice.task.clone()).await;
        }
        true
    }

    /// 流已结束、终态通知也到了：命令来源合成终态通知（剩下的行拿得到令牌就放进正文）；ws 来源只 ack（关闭说明已经
    /// 送过）。
    async fn monitor_settled(&mut self, task: ShellTaskId) {
        let Some(mut entry) = self.monitors.entries.remove(&task) else {
            return;
        };
        let Some(notice) = entry.notice.take() else {
            return;
        };
        if entry.watch.armed.source == MonitorSource::Socket {
            self.ack(&notice).await;
            return;
        }
        let mut step = Step::default();
        let now = self.deps.clock.now_ms();
        let rest = entry.ended.flatten();
        let events = rest.filter(|_| entry.watch.admit_rest(now, &mut step));
        // 拿令牌时补的压下说明排在终态通知之前。
        for emit in &step.emits {
            self.lane
                .push_back(Lane::Notice(Waiting::MonitorEvent(emit_text(
                    &entry.watch.armed,
                    emit,
                ))));
        }
        let text = rust_cli_prompts::agents::notification_wrap(&rust_cli_query::monitor_envelope(
            &notice.notice,
            events.as_deref(),
        ));
        let work = notice.notice.task.to_string();
        let status = rust_cli_query::task_status(&notice.notice).to_owned();
        if self
            .enqueue_notice(Waiting::MonitorEnd {
                notice: Box::new(notice),
                text,
            })
            .await
        {
            self.log
                .append(KernelEvent::BackgroundWork { work, status });
        }
        if self.running.is_none() && !self.lane.is_empty() {
            self.start_next().await;
        }
    }
}
