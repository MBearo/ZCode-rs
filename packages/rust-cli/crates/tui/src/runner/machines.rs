//! 远端机器在 runner 里的部分（docs/specs/rust-cli/remote-exec.md §21.2、§21.3）：`/machines` 的动作经入口注入的机器服务
//! 做（回执在事件循环的 `FuturesUnordered` 里，不另起任务），结果写进 transcript；服务的变化通知到了就重取快照。
use futures_util::FutureExt;
use futures_util::stream::{self, BoxStream, StreamExt};
use ratatui::backend::Backend;

use super::Loop;
use super::session::Done;
use crate::machines::{Changed, MachineProblem};
use crate::ui::i18n_machines as words;
use crate::ui::machines::MachineRequest;

/// 机器服务的回执。
pub(super) enum MachinesDone {
    Added {
        name: String,
        ssh: String,
        result: Result<(), MachineProblem>,
    },
    Connected {
        name: String,
        result: Result<Changed, MachineProblem>,
    },
    Disconnected {
        name: String,
        result: Result<Changed, MachineProblem>,
    },
    Removed {
        name: String,
        result: Result<(), MachineProblem>,
    },
    /// 立即重试（F98，remote-exec.md §25.4.1）。
    Retried {
        name: String,
        result: Result<Changed, MachineProblem>,
    },
}

/// 机器服务的变化通知；没有服务时永远不出项。
pub(super) fn changes(services: &crate::services::Services) -> BoxStream<'static, ()> {
    match &services.machines {
        Some(machines) => machines.changes(),
        None => stream::pending().boxed(),
    }
}

impl<B: Backend> Loop<'_, B> {
    /// 启动时：记下有没有服务，取第一份快照。
    pub(super) fn start_machines(&mut self) {
        self.app.machines_enabled = self.options.services.machines.is_some();
        // MCP 工具行的 `@机器`（R5，§22.8）：按工具名的前缀找，服务器名先规范化。
        self.app.mcp_machines = self
            .options
            .services
            .mcp_machines
            .iter()
            .map(|(server, machine)| {
                (
                    rust_cli_kernel_api::mcp_state::normalize_name(server),
                    machine.clone(),
                )
            })
            .collect();
        self.refresh_machines();
    }

    /// 服务的快照变了（状态、延迟、清单）：重取（状态行与列表都画它）。
    ///
    /// 远端执行 F96（remote-exec.md §23.3）：某台机器的状态种类变了（就绪、连接中、重连中、失败、未连接；不算延迟与
    /// 重连的倒计时）时，后台任务的快照也重取——重连中的机器在快照里是上一份，连回来之后马上换成新问的。
    pub(super) fn refresh_machines(&mut self) {
        if let Some(machines) = &self.options.services.machines {
            let fresh = machines.snapshot();
            let kinds = |rows: &[crate::machines::MachineRow]| -> Vec<(
                String,
                std::mem::Discriminant<crate::machines::MachineStatus>,
            )> {
                rows.iter()
                    .map(|row| (row.name.clone(), std::mem::discriminant(&row.status)))
                    .collect()
            };
            let changed = kinds(&self.app.machines) != kinds(&fresh);
            self.app.machines = fresh;
            self.dirty = true;
            if changed {
                self.refresh_tasks();
            }
        }
    }

    /// 变化通知的流结束（服务不再出项）：以后不再等它。
    pub(super) fn machines_ended(&mut self) {
        self.machine_changes = stream::pending().boxed();
    }

    pub(super) fn machine_request(&mut self, request: MachineRequest) {
        let Some(machines) = self.options.services.machines.clone() else {
            self.app.info(words::unavailable(self.app.lang));
            return;
        };
        let future = match request {
            MachineRequest::Add {
                name,
                ssh,
                working_directory,
            } => {
                let adding = machines.add(name.clone(), ssh.clone(), working_directory);
                async move {
                    let result = adding.await;
                    MachinesDone::Added { name, ssh, result }
                }
                .boxed()
            }
            MachineRequest::Connect(name) => {
                let connecting = machines.connect(name.clone());
                async move {
                    let result = connecting.await;
                    MachinesDone::Connected { name, result }
                }
                .boxed()
            }
            MachineRequest::Disconnect(name) => {
                let leaving = machines.disconnect(name.clone());
                async move {
                    let result = leaving.await;
                    MachinesDone::Disconnected { name, result }
                }
                .boxed()
            }
            MachineRequest::Remove(name) => {
                let removing = machines.remove(name.clone());
                async move {
                    let result = removing.await;
                    MachinesDone::Removed { name, result }
                }
                .boxed()
            }
            MachineRequest::Retry(name) => {
                let retrying = machines.retry(name.clone());
                async move {
                    let result = retrying.await;
                    MachinesDone::Retried { name, result }
                }
                .boxed()
            }
        };
        self.pending.push(future.map(Done::Machines).boxed());
    }

    pub(super) fn on_machines(&mut self, done: MachinesDone) {
        let lang = self.app.lang;
        match done {
            // 写进清单之后马上连（§21.3，暂定）：连上或失败时另起一行。
            MachinesDone::Added {
                name,
                ssh,
                result: Ok(()),
            } => {
                self.app.info(words::added(lang, &name, &ssh));
                self.machine_request(MachineRequest::Connect(name));
            }
            MachinesDone::Connected {
                name,
                result: Ok(Changed::Done),
            } => self.app.info(words::connected(lang, &name)),
            MachinesDone::Connected {
                name,
                result: Ok(Changed::Already),
            } => self.app.info(words::already_connected(lang, &name)),
            MachinesDone::Disconnected {
                name,
                result: Ok(Changed::Done),
            } => self.app.info(words::disconnected(lang, &name)),
            MachinesDone::Disconnected {
                name,
                result: Ok(Changed::Already),
            } => self.app.info(words::not_connected(lang, &name)),
            MachinesDone::Removed {
                name,
                result: Ok(()),
            } => self.app.info(words::removed(lang, &name)),
            MachinesDone::Retried {
                name,
                result: Ok(Changed::Done),
            } => self.app.info(words::retrying(lang, &name)),
            MachinesDone::Retried {
                name,
                result: Ok(Changed::Already),
            } => self.app.info(words::not_waiting(lang, &name)),
            MachinesDone::Added {
                name,
                result: Err(problem),
                ..
            }
            | MachinesDone::Connected {
                name,
                result: Err(problem),
            }
            | MachinesDone::Disconnected {
                name,
                result: Err(problem),
            }
            | MachinesDone::Removed {
                name,
                result: Err(problem),
            }
            | MachinesDone::Retried {
                name,
                result: Err(problem),
            } => self.app.error(words::problem(lang, &name, &problem)),
        }
        // 回执与快照可能一前一后到：再取一次，界面不等下一个变化通知。
        self.refresh_machines();
    }
}
