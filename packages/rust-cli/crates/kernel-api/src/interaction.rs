//! 交互的回答形状（[`crate::orchestration::Interactions`] 的返回值）。从 `orchestration` 拆出，
//! 原路径经 `orchestration` 重新导出。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::InteractionId;
use crate::orchestration::InteractionRequest;
use crate::permission::PermissionRule;

/// 待处理的交互多久没回答就通知（6000 ms）：TUI 的终端通知（检查的间隔与“多久
/// 没按键”的门槛）与权限询问的 Notification hook 用同一个值。F58：以前 tui 与 session
/// 各写一份。
pub const NOTIFY_AFTER: Duration = Duration::from_secs(6);

/// 权限询问的回答（[`Answer::Permission`]）。
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

/// 提问（AskUserQuestion）的回答（[`Answer::Question`]）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum QuestionAnswer {
    /// 问题文字 → 选中的标签（多选用 `, ` 连接；`Other` 是用户输入的文字），即工具结果里的
    /// `answers`。其余字段是工作线 F72 新增的（serde 缺省为空、为空时不写出：旧的回答照读，只有 `answers` 的回答
    /// JSON 不变；docs/specs/rust-cli/tools-more.md §1.1）。
    Answered {
        answers: std::collections::BTreeMap<String, String>,
        /// 问题文字 → 这道题的预览与备注（工具结果里的 `annotations`）。
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        annotations: std::collections::BTreeMap<String, QuestionAnnotation>,
        /// 用户没有选、直接写的回复（`response`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<String>,
        /// 用户要求再多问几个（`followUp`）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        follow_up: bool,
        /// 空闲到期自动提交：超时的毫秒数（`afkTimeoutMs`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        afk_timeout_ms: Option<u64>,
    },
    /// 用户不回答（esc）。
    Declined,
}

/// 一道题的注记：选中选项的预览（原文）与用户的备注（工作线 F72）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnnotation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// 计划审批（ExitPlanMode）的回答（[`Answer::PlanApproval`]）。
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

/// 一个询问的回答：三种询问各一种（工作线 F134，审查 K4）。以前是 `serde_json::Value`：前端 `to_value`、工具
/// `from_value(..).ok()`，形状不对时一律当拒绝。JSON 的形状不变（`untagged`：就是里面那种回答的 JSON，三种的
/// `decision` 不重名）；App 协议的回答在 protocol-zcode 的边界换成它。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Answer {
    Permission(PermissionAnswer),
    Question(QuestionAnswer),
    PlanApproval(PlanApprovalAnswer),
}

impl Answer {
    /// 没有人能回答时按询问的种类给的回答：权限询问是 `permission`（`-p` 是 `Unavailable`，`-p` 里的后台子代理是
    /// `NoPrompts`），提问是不回答，计划审批是继续计划（工具按这几种与“没有回答”同样处理）。
    pub fn nobody(request: &InteractionRequest, permission: PermissionAnswer) -> Self {
        match request {
            InteractionRequest::Permission { .. } => Self::Permission(permission),
            InteractionRequest::Question { .. } => Self::Question(QuestionAnswer::Declined),
            InteractionRequest::PlanApproval { .. } => {
                Self::PlanApproval(PlanApprovalAnswer::KeepPlanning { feedback: None })
            }
        }
    }

    /// 回答的决定（日志的 `decision`，同 JSON 里的名字）。
    pub fn decision(&self) -> &'static str {
        match self {
            Self::Permission(PermissionAnswer::Allow { .. }) => "allow",
            Self::Permission(PermissionAnswer::Deny { .. }) => "deny",
            Self::Permission(PermissionAnswer::Unavailable) => "unavailable",
            Self::Permission(PermissionAnswer::NoPrompts) => "no_prompts",
            Self::Question(QuestionAnswer::Answered { .. }) => "answered",
            Self::Question(QuestionAnswer::Declined) => "declined",
            Self::PlanApproval(PlanApprovalAnswer::Approve { .. }) => "approve",
            Self::PlanApproval(PlanApprovalAnswer::KeepPlanning { .. }) => "keep_planning",
        }
    }

    /// 用户选“不再询问”时一并加入的规则（权限询问的 `Allow { remember }`）；其余回答没有。
    pub fn remembered(&self) -> &[PermissionRule] {
        match self {
            Self::Permission(PermissionAnswer::Allow { remember }) => remember,
            Self::Permission(
                PermissionAnswer::Deny { .. }
                | PermissionAnswer::Unavailable
                | PermissionAnswer::NoPrompts,
            )
            | Self::Question(_)
            | Self::PlanApproval(_) => &[],
        }
    }
}

impl From<PermissionAnswer> for Answer {
    fn from(answer: PermissionAnswer) -> Self {
        Self::Permission(answer)
    }
}

impl From<QuestionAnswer> for Answer {
    fn from(answer: QuestionAnswer) -> Self {
        Self::Question(answer)
    }
}

impl From<PlanApprovalAnswer> for Answer {
    fn from(answer: PlanApprovalAnswer) -> Self {
        Self::PlanApproval(answer)
    }
}

/// 工具按询问的种类取回答：回答是别的种类时原样退回（工具按没有回答处理）。
impl TryFrom<Answer> for PermissionAnswer {
    type Error = Answer;

    fn try_from(answer: Answer) -> Result<Self, Answer> {
        match answer {
            Answer::Permission(answer) => Ok(answer),
            other @ (Answer::Question(_) | Answer::PlanApproval(_)) => Err(other),
        }
    }
}

impl TryFrom<Answer> for QuestionAnswer {
    type Error = Answer;

    fn try_from(answer: Answer) -> Result<Self, Answer> {
        match answer {
            Answer::Question(answer) => Ok(answer),
            other @ (Answer::Permission(_) | Answer::PlanApproval(_)) => Err(other),
        }
    }
}

impl TryFrom<Answer> for PlanApprovalAnswer {
    type Error = Answer;

    fn try_from(answer: Answer) -> Result<Self, Answer> {
        match answer {
            Answer::PlanApproval(answer) => Ok(answer),
            other @ (Answer::Permission(_) | Answer::Question(_)) => Err(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionAnswer {
    pub interaction: InteractionId,
    pub answer: Answer,
}

/// 父会话已关闭（`/clear`、`/resume`）之后，旧会话里后台子代理的询问（docs/specs/rust-cli/agents.md §4.1.2，
/// 工作线 F72 新增）：不属于任何打开着的会话，前端经 [`crate::session_api::Kernel::detached_asks`] 订阅。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetachedAsk {
    pub interaction: InteractionId,
    /// 发起询问的子代理所属的会话（已关闭）。
    pub session: crate::ids::SessionId,
    /// 那个会话关闭时的标题（没有标题时为空）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub request: crate::orchestration::InteractionRequest,
}

/// 旧会话询问的变化（[`crate::session_api::Kernel::detached_asks`] 的一项）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DetachedEvent {
    /// 新的询问（订阅时先给出还没回答的）。装箱：询问比撤回大得多。
    Asked { ask: Box<DetachedAsk> },
    /// 回答了，或询问被撤回（子代理被停止）。
    Resolved { interaction: InteractionId },
}

#[cfg(test)]
#[path = "interaction_tests.rs"]
mod tests;
