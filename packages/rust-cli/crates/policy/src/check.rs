//! 工具专属检查的结果与输入。

use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule};

use crate::paths::PathContext;
use crate::rules::Rules;
use crate::texts::Reason;

/// 询问的性质：决定 bypassPermissions 与整个工具的 allow 规则能不能盖过它（shell-permissions.md §4.1 的表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskKind {
    /// 命中带 content 的 ask 规则：bypass 也要询问。
    Rule,
    /// 安全检查（敏感文件、后台 `&`）：allow 规则盖不过；yolo 放行，guarded 与其他模式询问，hook 放行之后仍询问；
    /// 自动模式不经分类器。工作线 F76：guarded 改回询问（用户决定 2026-10-23，F68 曾按 bypass 放行）。后台 `&` 的这一问
    /// 在 guarded 下不产生（工作线 F80，同 Node，Bash 检查按模式决定）。
    ///
    /// 修复线 F68（审计 P1）：以前按“安全检查一律 bypass 也问”处理，bypass 下写 `.bashrc`、`.git/…` 仍询问，`-p` 里
    /// 等于拒绝；现在只有 [`AskKind::Immune`] 一类 bypass 也问。
    Safety,
    /// bypass 也要询问、allow 规则盖不过的安全检查（危险删除、guarded 的危险命令）；自动模式不经分类器。
    Immune,
    /// 其他理由。
    Other,
}

impl AskKind {
    /// 安全检查（两种都算）：allow 规则盖不过，自动模式不经分类器。
    pub fn is_safety(&self) -> bool {
        matches!(self, Self::Safety | Self::Immune)
    }
}

/// 一次工具专属检查的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Allow,
    Deny {
        message: String,
    },
    Ask {
        message: String,
        kind: AskKind,
        suggestions: Vec<PermissionRule>,
    },
    /// 没有意见：交给通用判决链，最后转成询问（`reason` 决定询问的文本）。
    Passthrough {
        reason: Option<Reason>,
        suggestions: Vec<PermissionRule>,
    },
}

impl Check {
    pub fn ask(message: impl Into<String>) -> Self {
        Self::Ask {
            message: message.into(),
            kind: AskKind::Other,
            suggestions: Vec::new(),
        }
    }

    pub fn deny(message: impl Into<String>) -> Self {
        Self::Deny {
            message: message.into(),
        }
    }

    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// 检查需要的输入：模式、规则、路径事实，以及规则的根（工作区根）。
pub struct Inputs<'a> {
    pub mode: PermissionMode,
    pub rules: &'a Rules<'a>,
    pub paths: &'a PathContext,
    pub root: &'a str,
    /// 这次调用是 guarded 下命中危险命令规则的 Bash 命令（`guarded::applies`，修复线 F20）。
    pub dangerous: bool,
    /// hook 放行之后的检查（`PermissionGate::check_rules`）：[`AskKind::Safety`] 的询问也留下，不按 bypass 放行。
    pub hook_check: bool,
    /// 完全访问：会话的模式是 bypassPermissions（yolo）且 plan 关着（plan 开着时判决用的模式是 plan）。除拒绝外的
    /// 询问一律放行（工作线 F76，用户决定 2026-10-23“如果是 yolo 啥也不问”，shell-permissions.md §4.6.2）。
    pub full_access: bool,
}

impl Inputs<'_> {
    /// 同样的输入，路径事实换成 `paths`（cd 之后按别的当前目录检查，shell-permissions.md §4.5.7）。
    pub(crate) fn with_paths<'b>(&'b self, paths: &'b crate::paths::PathContext) -> Inputs<'b> {
        Inputs {
            mode: self.mode,
            rules: self.rules,
            paths,
            root: self.root,
            dangerous: self.dangerous,
            hook_check: self.hook_check,
            full_access: self.full_access,
        }
    }
}
