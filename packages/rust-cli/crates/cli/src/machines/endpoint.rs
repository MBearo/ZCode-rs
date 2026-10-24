//! 一台机器的端点（R3，remote-exec.md §20.3.3）：SSH 用 R2 的 ssh-connect；只给测试的 loopback 直接拉起本进程的二进制。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use rust_cli_exec_transport::stdio::{ChildCommand, StdioConnector};
use rust_cli_exec_transport::{Connector, LinkError, Provisioned, Provisioner, Ready};
use rust_cli_machines::{Endpoint, Machine, MachineEntry};

use super::{Connect, Start};

#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程事实的位置：loopback 拉起的执行器就是本进程的二进制（README §4）"
)]
fn this_executable() -> Result<PathBuf> {
    std::env::current_exe().context("finding this executable for a loopback machine")
}

/// 只给测试的 loopback：部署就是“本进程的二进制以执行器模式跑”；连接器在那台机器的工作目录拉起它。
struct Loopback {
    ready: Ready,
    env: Vec<(String, String)>,
    cwd: PathBuf,
    name: String,
}

impl Endpoint for Loopback {
    fn describe(&self) -> String {
        format!("loopback machine {}", self.name)
    }

    fn provisioner(&self) -> &dyn Provisioner {
        &self.ready
    }

    fn connector(&self, provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError> {
        Ok(Arc::new(StdioConnector::new(ChildCommand {
            program: PathBuf::from(&provisioned.program),
            args: provisioned.args.clone(),
            env: self.env.clone(),
            cwd: Some(self.cwd.clone()),
            // 同 R1 的 loopback：父进程不强杀执行器（它读到 stdin 结束自己退出，§18.6）。
            kill_on_drop: false,
        })))
    }
}

/// 一台机器的状态机。
pub(super) fn machine(start: &Start, entry: &MachineEntry) -> Result<Machine> {
    let connect = &start.connect;
    let endpoint: Arc<dyn Endpoint> = match connect {
        Connect::Ssh { config, artifact } => crate::ssh_executor::endpoint(
            (&start.env, &start.executor),
            &entry.ssh,
            (config.as_deref(), artifact.as_deref()),
        ),
        Connect::Loopback => {
            let dir = entry
                .working_directory
                .as_ref()
                .ok_or_else(|| anyhow!("a loopback machine needs a working directory"))?;
            Arc::new(Loopback {
                ready: Ready(Provisioned {
                    program: this_executable()?.to_string_lossy().into_owned(),
                    args: crate::executor::child_args(&[], true),
                }),
                env: start.env.clone(),
                cwd: PathBuf::from(dir),
                name: entry.name.clone(),
            })
        }
    };
    Ok(Machine::new(endpoint, super::options(connect)))
}
