//! TUI 的机器服务（docs/specs/rust-cli/remote-exec.md §21.2）：把 Fleet（清单、R2 的状态机、注册表）换算成 tui 自己的
//! 类型。TUI 不认识 machines crate；换算只在这里。
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_machines::{
    MachineEntry, MachineError, MachineState, MachinesError, ProvisionError, Retry,
};
use rust_cli_tui_ports::machines::{
    Changed, ConnectStep, ExecutorInfo, MachineFailure, MachineProblem, MachineRow, MachineStatus,
    Machines, SshHost,
};

use crate::machines::{Fleet, FleetError, Seen};

struct TuiMachines(Arc<Fleet>, Option<PathBuf>);

/// `ssh_config`：列表与添加页建议的主机从这个文件读（remote-exec.md §21.3）；不知道 home 时没有。
pub(crate) fn service(fleet: Arc<Fleet>, ssh_config: Option<PathBuf>) -> Arc<dyn Machines> {
    Arc::new(TuiMachines(fleet, ssh_config))
}

/// `~/.ssh/config` 的主机（remote-exec.md §21.3）：`Host` 行里每个不带通配与否定的名字一项，按出现的次序、去重；
/// `HostName`、`User` 取那一段里第一次出现的。不跟 `Include`，不展开 `%h` 一类的写法；不能当机器名的别名不列。
pub(crate) fn ssh_hosts(text: &str) -> Vec<SshHost> {
    // 一段：它的名字与 HostName、User。
    let mut blocks: Vec<(Vec<String>, Option<String>, Option<String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // 关键字与值之间是空白或 `=`。
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((key, value)) => (
                key,
                value
                    .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
                    .trim(),
            ),
            None => continue,
        };
        let value = value.trim_matches('"');
        match key.to_ascii_lowercase().as_str() {
            "host" => blocks.push((
                value
                    .split_whitespace()
                    .filter(|name| !name.contains(['*', '?', '!']))
                    .map(str::to_owned)
                    .collect(),
                None,
                None,
            )),
            // `Match` 开的段不是主机：它后面的 HostName 不算到前一段上。
            "match" => blocks.push((Vec::new(), None, None)),
            "hostname" => {
                if let Some(block) = blocks.last_mut() {
                    block.1.get_or_insert_with(|| value.to_owned());
                }
            }
            "user" => {
                if let Some(block) = blocks.last_mut() {
                    block.2.get_or_insert_with(|| value.to_owned());
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<SshHost> = Vec::new();
    for (names, hostname, user) in blocks {
        for alias in names {
            if !rust_cli_machines::list::valid_name(&alias) || out.iter().any(|h| h.alias == alias)
            {
                continue;
            }
            let host = hostname.clone().unwrap_or_else(|| alias.clone());
            let target = match &user {
                Some(user) => format!("{user}@{host}"),
                None => host,
            };
            out.push(SshHost { alias, target });
        }
    }
    out
}

fn failure(error: &MachineError) -> MachineFailure {
    match error.clone() {
        MachineError::Provision(error) => provision_failure(error),
        MachineError::Unreachable(message) => MachineFailure::Unreachable(message),
        MachineError::Refused(message) => MachineFailure::Refused(message),
        MachineError::Lost(message) => MachineFailure::Lost(message),
        MachineError::Stopped => MachineFailure::Stopped,
    }
}

/// 部署失败 → 界面的类型（tui 不认识 machines，换算只在这里）。
fn provision_failure(error: ProvisionError) -> MachineFailure {
    match error {
        ProvisionError::Unreachable { target: _, message } => MachineFailure::Unreachable(message),
        ProvisionError::Unsupported { os, arch } => MachineFailure::Unsupported { os, arch },
        ProvisionError::NoArtifact { version, target } => {
            MachineFailure::NoArtifact { version, target }
        }
        ProvisionError::Incompatible {
            version,
            protocol,
            busy,
        } => MachineFailure::Incompatible {
            version,
            protocol,
            busy,
        },
        ProvisionError::Upload(message) => MachineFailure::Upload(message),
        ProvisionError::Probe(message) => MachineFailure::Probe(message),
        ProvisionError::DownloadFailed {
            version,
            target,
            reason,
        } => MachineFailure::DownloadFailed {
            version,
            target,
            reason,
        },
        ProvisionError::Musl => MachineFailure::Musl,
        ProvisionError::OldGlibc { glibc, needs } => MachineFailure::OldGlibc { glibc, needs },
        ProvisionError::LatestIncompatible {
            version,
            protocol,
            latest,
            latest_protocol,
            target,
        } => MachineFailure::LatestIncompatible {
            version,
            protocol,
            latest,
            latest_protocol,
            target,
        },
        ProvisionError::NoPackage { package } => MachineFailure::NoPackage { package },
        ProvisionError::Unsigned => MachineFailure::Unsigned,
    }
}

/// 墙上时间（毫秒），与界面的时钟相同：把状态机里下一次重连的时刻换成界面能比的时间（F96）。
fn wall_ms() -> u64 {
    rust_cli_base::clock::SystemClock::utc().now_ms()
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
        MachineState::Reconnecting(Retry {
            attempt,
            retry_at,
            reason,
        }) => MachineStatus::Reconnecting {
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

    fn ssh_hosts(&self) -> BoxFuture<'static, Vec<SshHost>> {
        let path = self.1.clone();
        Box::pin(async move {
            let Some(path) = path else {
                return Vec::new();
            };
            match rust_cli_fs::read_optional(&path).await {
                Ok(Some(bytes)) => ssh_hosts(&String::from_utf8_lossy(&bytes)),
                Ok(None) => Vec::new(),
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "the ssh config could not be read");
                    Vec::new()
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "tui_machines_tests.rs"]
mod tests;
