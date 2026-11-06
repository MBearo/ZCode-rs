//! 工作区信任的询问要显示的内容与回答（docs/specs/rust-cli/workspace-trust.md §2.4、§2.5）：两套终端界面共用的类型；判定是否
//! 要问、写信任记录都在入口，画法在各自的界面 crate。

/// 项目级配置文件里会自动生效的授权（`permission.allowedTools`，workspace-trust.md §2.4）。规则已排好、
/// 写成规则的形式并去重；`count` 是原始条数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grants {
    pub count: usize,
    pub rules: Vec<String>,
    /// 写了这些规则的文件，相对工作区根。
    pub sources: Vec<String>,
}

/// 对话框要显示的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustPrompt {
    /// 工作区根的绝对路径。
    pub path: String,
    /// 只经父目录信任、项目有授权时的后备询问（§2.5）：取消项是“不带这些权限继续”。
    pub backstop: bool,
    pub grants: Option<Grants>,
}

/// 用户的回答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustAnswer {
    /// 选了 `Yes, I trust this folder`。
    Trusted,
    /// 后备询问里选了“不带这些权限继续”或按了 esc：继续，但去掉项目级的 allow 规则。
    WithoutGrants,
    /// 退出进程，不打印文字：`No, exit` 与 ctrl+c / ctrl+d 两次是 1，esc 是 0。
    Exit(u8),
}
