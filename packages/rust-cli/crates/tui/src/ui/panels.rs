//! L30 面板（docs/specs/rust-cli/tui-panels.md）要 runner 去问内核的动作。
use rust_cli_kernel_api::ids::AgentId;

#[derive(Debug, Clone, PartialEq)]
pub enum Panel {
    /// 查看子代理的只读视图；None 回到主会话（§1.4）。
    ViewAgent(Option<AgentId>),
    /// Shell 详情已打开：读这个后台命令（§3.3）。
    ReadShell(String),
}
