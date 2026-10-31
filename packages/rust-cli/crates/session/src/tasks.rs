//! 后台任务的查看与停止（docs/specs/rust-cli/tui-display.md §3.1，工作线 L28）。
//!
//! 后台命令的事实在执行环境的账本里，后台子代理在 agents 里；这里只汇总给前端看，停止交给 agents 的
//! 同一条 TaskStop 路径，会话 actor 不另记任务表。远端执行 R4（docs/specs/rust-cli/remote-exec.md §21.6）：连上的远端
//! 机器上的后台命令也列出来（各台机器的执行器有自己的账本），条目带机器名；Shell 详情按任务 ID 依次问各台机器。
//!
//! F96（remote-exec.md §23.3）：重连中的机器不问（它的调用要等重连，SSH 又一直重连）；用它上一份的条目，标
//! `reconnecting`。“上一份”由快照的唯一 owner——会话（[`LastSeen`]，`Session` 持有）——记着，不靠超时。

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use rust_cli_kernel_api::env::{OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::env_files::{ReadText, TextRead};
use rust_cli_kernel_api::env_shell::{self, BackgroundStatus, BackgroundTask};
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

/// 每台远端机器最后一次问到的后台命令（F96）：机器名 → 条目。锁只在取放时拿着，不跨 await。
#[derive(Default)]
pub(crate) struct LastSeen(Mutex<BTreeMap<String, Vec<BackgroundEntry>>>);

impl LastSeen {
    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<BackgroundEntry>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 重连中的这台机器上一份的条目，每条标 `reconnecting`；没问到过时是空的。
    fn stale(&self, machine: &str) -> Vec<BackgroundEntry> {
        self.map()
            .get(machine)
            .into_iter()
            .flatten()
            .cloned()
            .map(|entry| BackgroundEntry {
                reconnecting: true,
                ..entry
            })
            .collect()
    }
}

/// 会话里还在跑的后台命令（带输出末尾）与后台子代理。在调用方的任务里跑，不占用 actor。
pub(crate) async fn background_entries(
    deps: &SessionDeps,
    last: &LastSeen,
    workspace: &WorkspaceRef,
    session: &SessionId,
) -> Vec<BackgroundEntry> {
    let mut entries = running_shells(deps, last, workspace, session, true).await;
    entries.extend(deps.agents.background_entries(session).await);
    entries
}

/// 子代理列表与后台任务的同一份快照（修复线 F34，docs/specs/rust-cli/tui-panels.md §0）：先取在跑的后台命令（不读
/// 输出末尾：界面不用，而这份快照在 agents 每次变化后都重取），最后取 agents 的那一半（条目与后台子代理同一次读取）。
/// 后台任务的顺序同 [`background_entries`]：命令在前、子代理在后。在调用方的任务里跑，不占用 actor。
pub(crate) async fn snapshot(
    deps: &SessionDeps,
    last: &LastSeen,
    workspace: &WorkspaceRef,
    session: &SessionId,
) -> TasksSnapshot {
    let mut background = running_shells(deps, last, workspace, session, false).await;
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

/// 本机与连上的远端机器的账本里还在跑的后台命令；`tail` 为真时带输出文件的末尾。重连中的机器不问，用 `last` 里它
/// 上一份的条目（F96）；问到的远端机器记成新的上一份，离开注册表的机器的上一份丢掉。
async fn running_shells(
    deps: &SessionDeps,
    last: &LastSeen,
    workspace: &WorkspaceRef,
    session: &SessionId,
    tail: bool,
) -> Vec<BackgroundEntry> {
    let places = places(deps, workspace);
    // 每台机器在不在重连只看一次：下面“问哪些”与“结果归哪台”都按这一次的答案，不会因为中途状态变了而错位。
    let reconnecting: Vec<bool> = places
        .iter()
        .map(|place| {
            place
                .name
                .as_deref()
                .is_some_and(|name| deps.machines.reconnecting(name))
        })
        .collect();
    // 各台机器同时问（一台慢不拖住别的台；结果仍按本机在前、远端按名字的顺序）。
    let asked = futures_util::future::join_all(
        places
            .iter()
            .zip(&reconnecting)
            .filter(|(_, reconnecting)| !**reconnecting)
            .map(|(place, _)| running_on(place, session, tail)),
    )
    .await;
    let mut asked = asked.into_iter();
    let mut entries = Vec::new();
    for (place, reconnecting) in places.iter().zip(reconnecting) {
        let found = if reconnecting {
            place
                .name
                .as_deref()
                .map(|name| last.stale(name))
                .unwrap_or_default()
        } else {
            let found = asked.next().unwrap_or_default();
            if let Some(name) = &place.name {
                last.map().insert(name.clone(), found.clone());
            }
            found
        };
        entries.extend(found);
    }
    last.map()
        .retain(|name, _| places.iter().any(|place| place.name.as_ref() == Some(name)));
    entries
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
        // 修复线 F117：种类与名称照执行环境报的。Monitor 是 `Monitor`、名称是它的 description（空时是命令原文，旧执行器
        // 不报种类时按 Bash）；以前一律记成 `Shell`、名称是命令原文，footer 把 monitor 数成 shell。
        let (kind, description) = match info.kind {
            env_shell::BackgroundKind::Monitor if !info.description.is_empty() => {
                (BackgroundKind::Monitor, info.description)
            }
            env_shell::BackgroundKind::Monitor => (BackgroundKind::Monitor, info.command),
            env_shell::BackgroundKind::Bash => (BackgroundKind::Shell, info.command),
        };
        entries.push(BackgroundEntry {
            id: task.to_string(),
            kind,
            description,
            status: info.status,
            output,
            machine: place.name.clone(),
            reconnecting: false,
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
            // 缺陷修复（Docker 试用 L11，remote-exec.md §25.4.6）：`total_lines` 是 `\n` 的个数加一（Read 的读法），框里的行
            // 用 `lines()` 切；输出以换行结尾时（命令的输出一般都是）总数比切出的行多一。不是两次读之间多了一行的竞态：
            // 本机也一样。
            let total = if slice.content.ends_with('\n') {
                slice.total_lines.saturating_sub(1)
            } else {
                slice.total_lines
            };
            ShellOutput::Tail {
                lines: tail.iter().map(|line| (*line).to_owned()).collect(),
                total,
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
