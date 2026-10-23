//! 工具专属检查的结果与输入。

use rust_cli_kernel_api::permission::{PermissionMode, PermissionRule};

use crate::paths::PathContext;
use crate::rules::Rules;
use crate::texts::Reason;

/// 询问的性质：决定 bypassPermissions 与整个工具的 allow 规则能不能盖过它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskKind {
    /// 命中带 content 的 ask 规则：bypass 也要询问。
    Rule,
    /// 安全路径（`.git/`、shell 配置等）：bypass 也要询问（1g），自动模式不经分类器。
    Safety,
    /// 其他理由。
    Other,
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
}
