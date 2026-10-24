//! SSH 的连接器（remote-exec.md §19.2、§19.9）：`ssh -T -o BatchMode=yes <目标> '<执行器>' executor --stdio`，ssh 的
//! stdin / stdout 就是帧的通道。连接被丢掉时结束 ssh 进程（远端的代理读到结束自己退出，常驻执行器不受影响）。

use async_trait::async_trait;
use rust_cli_exec_transport::stdio::{ChildCommand, StdioConnector};
use rust_cli_exec_transport::{Connector, Link, LinkError, Provisioned};

use crate::target::{SshTarget, quote};

pub struct SshConnector {
    target: SshTarget,
    inner: StdioConnector,
}

impl SshConnector {
    /// `provisioned`：部署给的启动命令（远端的路径与参数）。
    pub fn new(target: SshTarget, provisioned: &Provisioned) -> Result<Self, LinkError> {
        let remote = std::iter::once(quote(&provisioned.program))
            .chain(provisioned.args.iter().map(|arg| quote(arg)))
            .collect::<Vec<_>>()
            .join(" ");
        let args = target.args(&remote).map_err(LinkError::Connect)?;
        let inner = StdioConnector::new(ChildCommand {
            program: target.program.clone(),
            args,
            env: target.env.clone(),
            cwd: None,
            kill_on_drop: true,
        });
        Ok(Self { target, inner })
    }
}

#[async_trait]
impl Connector for SshConnector {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        self.inner.connect().await
    }

    fn describe(&self) -> String {
        self.target.describe()
    }
}
