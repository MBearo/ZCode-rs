//! TUI 的机器服务（docs/specs/rust-cli/remote-exec.md §21.2）：把 Fleet（清单、R2 的状态机、注册表）换算成 tui 自己的
//! 类型。TUI 不认识 machines crate；换算只在这里。
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_machines::{MachineEntry, MachineError, MachineState, MachinesError};
use rust_cli_tui::machines::{
    Changed, ConnectStep, ExecutorInfo, MachineFailure, MachineProblem, MachineRow, MachineStatus,
    Machines,
};

use crate::machines::{Fleet, FleetError, Seen};

struct TuiMachines(Arc<Fleet>);

pub(crate) fn service(fleet: Arc<Fleet>) -> Arc<dyn Machines> {
    Arc::new(TuiMachines(fleet))
}

fn failure(error: &MachineError) -> MachineFailure {
    match error.clone() {
        MachineError::Unreachable(message) => MachineFailure::Unreachable(message),
        MachineError::Unsupported { os, arch } => MachineFailure::Unsupported { os, arch },
        MachineError::NoArtifact { version, target } => {
            MachineFailure::NoArtifact { version, target }
        }
        MachineError::Incompatible {
            version,
            protocol,
            busy,
        } => MachineFailure::Incompatible {
            version,
            protocol,
            busy,
        },
        MachineError::Upload(message) => MachineFailure::Upload(message),
        MachineError::Probe(message) => MachineFailure::Probe(message),
        MachineError::DownloadFailed {
            version,
            target,
            reason,
        } => MachineFailure::DownloadFailed {
            version,
            target,
            reason,
        },
        MachineError::Musl => MachineFailure::Musl,
        MachineError::OldGlibc { glibc, needs } => MachineFailure::OldGlibc { glibc, needs },
        MachineError::Refused(message) => MachineFailure::Refused(message),
        MachineError::Lost(message) => MachineFailure::Lost(message),
    }
}

/// 墙上时间（毫秒），与界面的时钟相同：把状态机里下一次重连的时刻换成界面能比的时间（F96）。
fn wall_ms() -> u64 {
    use rust_cli_kernel_api::clock::Clock;
    rust_cli_node_store::SystemClock::utc().now_ms()
}

/// `now` 是（单调时钟，墙上时间）的同一时刻。
fn status(state: &MachineState, now: (tokio::time::Instant, u64)) -> MachineStatus {
    match state {
        MachineState::Unconnected => MachineStatus::Disconnected,
        // 第一次连接分步（F98，remote-exec.md §25.4.2）。
        MachineState::Provisioning => MachineStatus::Connecting(ConnectStep::Checking),
        // F100（remote-exec.md §26.6）：下载执行器。
        MachineState::Downloading => MachineStatus::Connecting(ConnectStep::Downloading),
        MachineState::Uploading { bytes } => {
            MachineStatus::Connecting(ConnectStep::Uploading { bytes: *bytes })
        }
        MachineState::Handshaking => MachineStatus::Connecting(ConnectStep::Handshaking),
        MachineState::Ready(_) => MachineStatus::Ready,
        // 状态机记的是“已经开始了几次”；界面写下一次（等着时）或正在进行的是第几次（remote-exec.md §23.4）。
        MachineState::Reconnecting {
            attempt,
            retry_at,
            reason,
        } => MachineStatus::Reconnecting {
            attempt: if retry_at.is_some() {
                attempt.saturating_add(1)
            } else {
                *attempt
            },
            retry_at_ms: retry_at.map(|at| {
                let left = at.saturating_duration_since(now.0);
                now.1
                    .saturating_add(u64::try_from(left.as_millis()).unwrap_or(u64::MAX))
            }),
            reason: reason.clone(),
        },
        MachineState::Failed { error } => MachineStatus::Failed(failure(error)),
    }
}

fn row(seen: Seen, now: (tokio::time::Instant, u64)) -> MachineRow {
    MachineRow {
        status: status(&seen.state, now),
        latency_ms: seen
            .latency
            .map(|latency| u64::try_from(latency.as_millis()).unwrap_or(u64::MAX)),
        executor: seen.welcome.map(|welcome| ExecutorInfo {
            refreshes_env: welcome
                .capabilities
                .contains(rust_cli_exec_wire::handshake::Capability::Env),
            os: welcome.os,
            arch: welcome.arch,
            version: welcome.server_version,
            home: welcome.home,
        }),
        name: seen.entry.name,
        ssh: seen.entry.ssh,
        working_directory: seen.entry.working_directory,
    }
}

fn problem(error: FleetError) -> MachineProblem {
    match error {
        FleetError::Connection(error) => MachineProblem::Connection(failure(&error)),
        FleetError::List(MachinesError::Name(name)) => MachineProblem::InvalidName(name),
        FleetError::List(MachinesError::Exists(name)) => MachineProblem::Exists(name),
        FleetError::List(MachinesError::Missing(name)) => MachineProblem::Missing(name),
        FleetError::List(other) => MachineProblem::File(other.to_string()),
    }
}

fn changed(changed: crate::machines::Changed) -> Changed {
    match changed {
        crate::machines::Changed::Done => Changed::Done,
        crate::machines::Changed::Already => Changed::Already,
    }
}

impl Machines for TuiMachines {
    fn snapshot(&self) -> Vec<MachineRow> {
        let now = (tokio::time::Instant::now(), wall_ms());
        self.0
            .snapshot()
            .into_iter()
            .map(|seen| row(seen, now))
            .collect()
    }

    fn changes(&self) -> BoxStream<'static, ()> {
        futures_util::stream::unfold(self.0.subscribe(), |mut receiver| async move {
            receiver.changed().await.ok()?;
            receiver.borrow_and_update();
            Some(((), receiver))
        })
        .boxed()
    }

    fn add(
        &self,
        name: String,
        ssh: String,
        working_directory: Option<String>,
    ) -> BoxFuture<'static, Result<(), MachineProblem>> {
        let fleet = self.0.clone();
        Box::pin(async move {
            let entry = MachineEntry::new(&name, &ssh, working_directory.as_deref());
            fleet.add(entry).await.map_err(problem)
        })
    }

    fn connect(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>> {
        let fleet = self.0.clone();
        Box::pin(async move { fleet.connect(&name).await.map(changed).map_err(problem) })
    }

    fn disconnect(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>> {
        let fleet = self.0.clone();
        Box::pin(async move { fleet.disconnect(&name).await.map(changed).map_err(problem) })
    }

    fn retry(&self, name: String) -> BoxFuture<'static, Result<Changed, MachineProblem>> {
        let fleet = self.0.clone();
        Box::pin(async move { fleet.retry(&name).await.map(changed).map_err(problem) })
    }

    fn remove(&self, name: String) -> BoxFuture<'static, Result<(), MachineProblem>> {
        let fleet = self.0.clone();
        Box::pin(async move { fleet.remove(&name).await.map_err(problem) })
    }
}

#[cfg(test)]
#[path = "tui_machines_tests.rs"]
mod tests;
