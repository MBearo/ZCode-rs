//! 后台任务的查看与停止（docs/specs/rust-cli/tui-display.md §3.1，工作线 L28）。
//!
//! 后台命令的事实在执行环境的账本里，后台子代理在 agents 里；这里只汇总给前端看，停止交给 agents 的
//! 同一条 TaskStop 路径，会话 actor 不另记任务表。远端执行 R4（docs/specs/rust-cli/remote-exec.md §21.6）：连上的远端
//! 机器上的后台命令也列出来（各台机器的执行器有自己的账本），条目带机器名；Shell 详情按任务 ID 依次问各台机器。

use rust_cli_kernel_api::env::{OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::env_files::{ReadText, TextRead};
use rust_cli_kernel_api::env_shell::{BackgroundStatus, BackgroundTask};
use rust_cli_kernel_api::ids::{OwnerEpoch, SessionId};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind, OUTPUT_TAIL_LINES};
use rust_cli_kernel_api::panels::{SHELL_DETAIL_LINES, ShellDetail, ShellOutput, TasksSnapshot};
use rust_cli_kernel_api::session_api::Ack;
use serde_json::json;

use crate::SessionDeps;
use crate::actor::Actor;

/// 输出文件超过这个大小时不读（详情页只显示末尾）。
const OUTPUT_MAX_BYTES: u64 = 1 << 20;

/// 会话里还在跑的后台命令（带输出末尾）与后台子代理。在调用方的任务里跑，不占用 actor。
pub(crate) async fn background_entries(
    deps: &SessionDeps,
    workspace: &WorkspaceRef,
    session: &SessionId,
) -> Vec<BackgroundEntry> {
    let mut entries = running_shells(deps, workspace, session, true).await;
    entries.extend(deps.agents.background_entries(session).await);
    entries
}

/// 子代理列表与后台任务的同一份快照（修复线 F34，docs/specs/rust-cli/tui-panels.md §0）：先取在跑的后台命令（不读
/// 输出末尾：界面不用，而这份快照在 agents 每次变化后都重取），最后取 agents 的那一半（条目与后台子代理同一次读取）。
/// 后台任务的顺序同 [`background_entries`]：命令在前、子代理在后。在调用方的任务里跑，不占用 actor。
pub(crate) async fn snapshot(
    deps: &SessionDeps,
    workspace: &WorkspaceRef,
    session: &SessionId,
) -> TasksSnapshot {
    let mut background = running_shells(deps, workspace, session, false).await;
    let agents = deps.agents.tasks_snapshot(session).await;
    background.extend(agents.background);
    TasksSnapshot {
        subagents: agents.subagents,
        background,
    }
}

/// 一台机器：执行端口、它上面的工作区、远端时的名字（本机为 None）。
struct Place {
    machine: MachineEnv,
    workspace: WorkspaceRef,
    name: Option<String>,
}

/// 本机在前，再是连上的远端机器（按名字，注册表的快照）。
fn places(deps: &SessionDeps, workspace: &WorkspaceRef) -> Vec<Place> {
    let mut places = vec![Place {
        machine: deps.machine.clone(),
        workspace: workspace.clone(),
        name: None,
    }];
    for remote in deps.machines.machines().machines {
        if let Some((machine, workspace)) = deps.machines.remote(&remote.name) {
            places.push(Place {
                machine,
                workspace,
                name: Some(remote.name),
            });
        }
    }
    places
}

/// 本机与连上的远端机器的账本里还在跑的后台命令；`tail` 为真时带输出文件的末尾。
async fn running_shells(
    deps: &SessionDeps,
    workspace: &WorkspaceRef,
    session: &SessionId,
    tail: bool,
) -> Vec<BackgroundEntry> {
    // 各台机器同时问（一台慢不拖住别的台；结果仍按本机在前、远端按名字的顺序）。
    let places = places(deps, workspace);
    let lists =
        futures_util::future::join_all(places.iter().map(|place| running_on(place, session, tail)))
            .await;
    lists.into_iter().flatten().collect()
}

/// 一台机器的账本里还在跑的后台命令。
async fn running_on(place: &Place, session: &SessionId, tail: bool) -> Vec<BackgroundEntry> {
    let mut entries = Vec::new();
    let Ok(state) = place
        .machine
        .shell()
        .shell_state(&place.workspace, session)
        .await
    else {
        return entries;
    };
    for task in state.background {
        let key = BackgroundTask {
            session: session.clone(),
            task: task.clone(),
        };
        let Ok(Some(info)) = place.machine.background().background_command(key).await else {
            continue;
        };
        if info.status != BackgroundStatus::Running {
            continue;
        }
        let output = match &state.outputs {
            Some(dir) if tail => output_tail(place, &output_path(dir, task.as_str())).await,
            _ => None,
        };
        entries.push(BackgroundEntry {
            id: task.to_string(),
            kind: BackgroundKind::Shell,
            description: info.command,
            status: info.status,
            output,
            machine: place.name.clone(),
        });
    }
    entries
}

/// 与执行环境的输出文件同名（`<outputs>/<任务>.output`）；Windows 也接受 `/`。
fn output_path(
    dir: &rust_cli_kernel_api::env::EnvPath,
    task: &str,
) -> rust_cli_kernel_api::env::EnvPath {
    let base = dir.as_str().trim_end_matches(['/', '\\']);
    rust_cli_kernel_api::env::EnvPath::new(format!("{base}/{task}.output"))
}

/// 一个后台命令的详情（Shell 详情，docs/specs/rust-cli/tui-panels.md §3.3）：与 `/tasks` 同一份账本与输出
/// 文件；不认识时为 None。按本机、再按连上的远端机器依次问（任务 ID 跨机器不重复，remote-exec.md §20.7.2、§21.6）。
/// 在调用方的任务里跑，不占用 actor。
pub(crate) async fn shell_detail(
    deps: &SessionDeps,
    workspace: &WorkspaceRef,
    session: &SessionId,
    task: &str,
) -> Option<ShellDetail> {
    let key = BackgroundTask {
        session: session.clone(),
        task: rust_cli_kernel_api::ids::ShellTaskId::new(task),
    };
    let mut found = None;
    for place in places(deps, workspace) {
        let info = place
            .machine
            .background()
            .background_command(key.clone())
            .await
            .ok()
            .flatten();
        if let Some(info) = info {
            found = Some((place, info));
            break;
        }
    }
    let (place, info) = found?;
    let elapsed_ms = place
        .machine
        .background()
        .background_elapsed(key)
        .await
        .ok()
        .flatten();
    let outputs = place
        .machine
        .shell()
        .shell_state(&place.workspace, session)
        .await
        .ok()
        .and_then(|state| state.outputs);
    let output = match outputs {
        Some(dir) => output_preview(&place, &output_path(&dir, task)).await,
        None => ShellOutput::Unavailable,
    };
    Some(ShellDetail {
        id: task.to_owned(),
        command: info.command,
        status: info.status,
        elapsed_ms,
        output,
    })
}

/// 输出文件的末尾（最多 [`SHELL_DETAIL_LINES`] 行）与总行数；还没有文件时是空的，太大时不预览。
async fn output_preview(place: &Place, path: &rust_cli_kernel_api::env::EnvPath) -> ShellOutput {
    let request = ReadText {
        path: path.clone(),
        start_line: 0,
        max_lines: None,
        max_bytes: Some(OUTPUT_MAX_BYTES),
    };
    match place
        .machine
        .files()
        .read_text(&place.workspace, request)
        .await
    {
        Ok(TextRead::Text(slice)) => {
            let lines: Vec<&str> = slice.content.lines().collect();
            let tail = &lines[lines.len().saturating_sub(SHELL_DETAIL_LINES)..];
            ShellOutput::Tail {
                lines: tail.iter().map(|line| (*line).to_owned()).collect(),
                total: slice.total_lines,
            }
        }
        Ok(TextRead::TooLarge { size }) => ShellOutput::TooLarge { bytes: size },
        Err(rust_cli_kernel_api::env::EnvError::NotFound { .. }) => ShellOutput::Tail {
            lines: Vec::new(),
            total: 0,
        },
        Ok(TextRead::Directory) | Err(_) => ShellOutput::Unavailable,
    }
}

async fn output_tail(place: &Place, path: &rust_cli_kernel_api::env::EnvPath) -> Option<String> {
    let request = ReadText {
        path: path.clone(),
        start_line: 0,
        max_lines: None,
        max_bytes: Some(OUTPUT_MAX_BYTES),
    };
    let TextRead::Text(slice) = place
        .machine
        .files()
        .read_text(&place.workspace, request)
        .await
        .ok()?
    else {
        return None;
    };
    let lines: Vec<&str> = slice.content.lines().collect();
    Some(lines[lines.len().saturating_sub(OUTPUT_TAIL_LINES)..].join("\n"))
}

impl Actor {
    /// `Command::StopTask`：用户停止一个后台任务，走 agents 的 TaskStop 路径。
    pub(crate) async fn stop_task(&self, task: String) -> Ack {
        let operation = OperationContext {
            environment: self.meta.workspace.environment.clone(),
            generation: self.deps.generation.clone(),
            owner_epoch: OwnerEpoch::LOCAL,
            operation: self.deps.ids.operation(),
            origin: Origin::User {
                session: Some(self.meta.id.clone()),
            },
        };
        match self
            .deps
            .agents
            .stop_task(&self.meta.id, &task, operation)
            .await
        {
            Ok(message) => Ack::Accepted {
                result: json!({ "message": message }),
            },
            Err(reason) => Ack::Rejected { reason },
        }
    }
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
