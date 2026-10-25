//! SSH 的连接器（remote-exec.md §19.2、§19.9）：`ssh -T -o BatchMode=yes <目标> '<执行器>' executor --stdio`，ssh 的
//! stdin / stdout 就是帧的通道。连接被丢掉时结束 ssh 进程（远端的代理读到结束自己退出，常驻执行器不受影响）。
//!
//! 远端执行 F96（§23.4）：SSH 一直重连，所以这里分清再连也一样的失败——ssh 认证失败、主机密钥不对（ssh 以 255 退出，
//! stderr 有这些话）、远端的执行器程序不在了或不能执行（远端命令以 127 / 126 退出）——把它们标成
//! `LinkError::Rejected`，客户端不再重连。只有知道 ssh 的这一层能认出它们。

use async_trait::async_trait;
use bytes::Bytes;
use rust_cli_exec_transport::stdio::{ChildCommand, StdioConnector};
use rust_cli_exec_transport::{Connector, FrameSink, FrameSource, Link, LinkError, Provisioned};

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
        let link = self.inner.connect().await.map_err(classify)?;
        Ok(Box::new(Classified(link)))
    }

    fn describe(&self) -> String {
        self.target.describe()
    }
}

/// ssh 的这几句说明再连也一样（认证与主机密钥；ssh 自己的退出码是 255）。
const REJECTIONS: &[&str] = &[
    "Permission denied (",
    "Too many authentication failures",
    "Host key verification failed",
    "REMOTE HOST IDENTIFICATION HAS CHANGED",
    "No more authentication methods available",
];

/// 子进程以 `code` 退出（stdio 传输写成 `executor exited (exit status: N)`；Windows 上是 `exit code: N`）。
fn exited_with(message: &str, code: u8) -> bool {
    message.contains(&format!("(exit status: {code})"))
        || message.contains(&format!("(exit code: {code})"))
}

/// 这次失败再连也一样：ssh 认证失败或主机密钥不对；远端的执行器程序不在了（127）或不能执行（126）。
pub(crate) fn rejected(message: &str) -> bool {
    (exited_with(message, 255) && REJECTIONS.iter().any(|known| message.contains(known)))
        || exited_with(message, 126)
        || exited_with(message, 127)
}

fn classify(error: LinkError) -> LinkError {
    match error {
        LinkError::Io(message) if rejected(&message) => LinkError::Rejected(message),
        other => other,
    }
}

/// ssh 的连接：接收方向的错误经 [`classify`]（ssh 退出时错误在这里，§18.6 的收尾顺序）。
struct Classified(Box<dyn Link>);

impl Link for Classified {
    fn split(self: Box<Self>) -> (Box<dyn FrameSink>, Box<dyn FrameSource>) {
        let (sink, source) = self.0.split();
        (sink, Box::new(ClassifiedSource(source)))
    }

    fn peer(&self) -> String {
        self.0.peer()
    }
}

struct ClassifiedSource(Box<dyn FrameSource>);

#[async_trait]
impl FrameSource for ClassifiedSource {
    async fn recv(&mut self) -> Result<Option<Bytes>, LinkError> {
        self.0.recv().await.map_err(classify)
    }
}
