//! 交互的回答形状（[`crate::orchestration::Interactions`] 的返回值）。从 `orchestration` 拆出，
//! 原路径经 `orchestration` 重新导出，接口不变。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::InteractionId;

/// 待处理的交互多久没回答就通知（6000 ms）：TUI 的终端通知（检查的间隔与“多久
/// 没按键”的门槛）与权限询问的 Notification hook 用同一个值。F58：以前 tui 与 session
/// 各写一份。
pub const NOTIFY_AFTER: Duration = Duration::from_secs(6);

/// 权限询问的回答（[`InteractionAnswer::answer`] 的形状）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PermissionAnswer {
    /// 允许；`remember` 是用户选择一并加入的规则。
    Allow {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remember: Vec<crate::permission::PermissionRule>,
    },
    /// 拒绝；`feedback` 是用户附带的说明，会转给模型。
    Deny {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    /// 没有人能回答（例如 `-p` 没有交互）：按拒绝处理，模型收到的是询问本身的文本
    /// （print 模式的行为，docs/specs/rust-cli/shell-permissions.md §4）。
    Unavailable,
    /// 所在的上下文不能询问（`-p` 里的后台子代理，没有询问的入口）：PermissionRequest hooks 没有结论时按拒绝处理，
    /// 模型收到 `Permission to use <tool> has been denied.` 加变通说明（docs/specs/rust-cli/agents.md §4.1.1，
    /// 工作线 F69 新增）。
    NoPrompts,
}

/// 提问（AskUserQuestion）的回答（[`InteractionAnswer::answer`] 的形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum QuestionAnswer {
    /// 问题文字 → 选中的标签（多选用 `, ` 连接；`Other` 是用户输入的文字），即工具结果里的
    /// `answers`。
    Answered {
        answers: std::collections::BTreeMap<String, String>,
    },
    /// 用户不回答（esc）。
    Declined,
}

/// 计划审批（ExitPlanMode）的回答（[`InteractionAnswer::answer`] 的形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PlanApprovalAnswer {
    /// 同意。修复线 F20 起批准只关掉 plan 开关，权限模式回到进入 plan 前的值；`mode` 只作记录（TUI 填当前的
    /// 权限模式），ExitPlanMode 不用它改模式。（原来是 “Yes, auto-accept edits” 为 acceptEdits，
    /// “Yes, manually approve edits” 为 default。）
    Approve {
        mode: crate::permission::PermissionMode,
    },
    /// 继续计划；`feedback` 是用户的修改意见，会转给模型。
    KeepPlanning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswer {
    pub interaction: InteractionId,
    pub answer: Value,
}
