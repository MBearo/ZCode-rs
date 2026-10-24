//! TUI 的机器服务（docs/specs/rust-cli/remote-exec.md §21.2）：把 Fleet（清单、R2 的状态机、注册表）换算成 tui 自己的
//! 类型。TUI 不认识 machines crate；换算只在这里。
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_machines::{MachineEntry, MachineError, MachineState, MachinesError};
use rust_cli_tui::machines::{
    Changed, ExecutorInfo, MachineFailure, MachineProblem, MachineRow, MachineStatus, Machines,
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
        MachineError::Refused(message) => MachineFailure::Refused(message),
        MachineError::Lost(message) => MachineFailure::Lost(message),
    }
}

fn status(state: &MachineState) -> MachineStatus {
    match state {
        MachineState::Unconnected => MachineStatus::Disconnected,
        MachineState::Provisioning | MachineState::Handshaking => MachineStatus::Connecting,
        MachineState::Ready(_) => MachineStatus::Ready,
        MachineState::Reconnecting { attempt } => MachineStatus::Reconnecting { attempt: *attempt },
        MachineState::Failed { error } => MachineStatus::Failed(failure(error)),
    }
}

fn row(seen: Seen) -> MachineRow {
    MachineRow {
        status: status(&seen.state),
        latency_ms: seen
            .latency
            .map(|latency| u64::try_from(latency.as_millis()).unwrap_or(u64::MAX)),
        executor: seen.welcome.map(|welcome| ExecutorInfo {
            os: welcome.os,
            arch: welcome.arch,
            version: welcome.server_version,
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
        self.0.snapshot().into_iter().map(row).collect()
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

    fn remove(&self, name: String) -> BoxFuture<'static, Result<(), MachineProblem>> {
        let fleet = self.0.clone();
        Box::pin(async move { fleet.remove(&name).await.map_err(problem) })
    }
}

#[cfg(test)]
#[path = "tui_machines_tests.rs"]
mod tests;
