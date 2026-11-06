//! 请求的数据（docs/puffin-tui.md §4.2「请求」「提问」「计划」；docs/specs/rust-cli/tui-kit.md §4.4）：权限请求、提问与计划，
//! 整理成板要画的形状。是纯的 serde 结构，从 Puffin 的投影（host/projection/ask.rs）搬来；旧界面把自己的请求
//! 映射成它们。
//! 形状来自 packages/tui/host/src/requests.ts（feat/tui a2bf878b34），去掉工作流的请求。
use serde::{Deserialize, Serialize};

use crate::app::widgets::tools::Sign;

/// 一个待处理的请求（`SessionView.pending` / `InteractionAsked` 的一项，整理成壳要画的形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    /// `InteractionId`。
    pub id: String,
    /// 问它的工具条目的 id；这个会话里没有那一行（子代理的请求）时为 ""。
    pub item_id: String,
    /// 内核的工具名原样。
    pub tool_name: String,
    /// 工具行用的名字与摘要（与工具条目同一规则）。
    pub name: String,
    pub summary: String,
    /// 子代理的请求：谁在问。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<AskOrigin>,
    /// 新（§11.1）：调用在哪台远端机器上执行；本机为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(flatten)]
    pub body: AskBody,
}

/// 子代理的请求：`who` 是子代理的名字，`parentItemId` 是启动它的工具条目（找不到时为 ""）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AskOrigin {
    pub who: String,
    pub parent_item_id: String,
}

/// 按 `kind` 区分的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AskBody {
    Permission {
        preview: AskPreview,
        options: Vec<AskOption>,
    },
    Question {
        pages: Vec<QuestionPage>,
        /// 内核没有自己结束的提问（§10.2 Snooze），恒为 false。
        snoozable: bool,
        /// 恒为 0：提问的超时是界面的 AFK（§11.4）。
        #[serde(rename = "deadlineAt")]
        deadline_at: u64,
    },
    Plan {
        /// 计划的 Markdown。
        plan: String,
    },
}

/// 权限请求要做的事，按工具整理（docs/puffin-tui.md §4.2「预览」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AskPreview {
    /// Bash：命令与模型给的说明（没有时为 ""）。
    Command { command: String, reason: String },
    /// Edit、MultiEdit、ApplyPatch：改动的行。
    Diff { path: String, lines: Vec<DiffLine> },
    /// Write：新文件的全文。
    File { path: String, text: String },
    /// WebFetch：地址与它想从页面里得到什么。
    Fetch { url: String, prompt: String },
    /// 其他工具：输入的字段，每个一行。
    Fields { fields: Vec<AskField> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub sign: Sign,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskField {
    pub key: String,
    pub value: String,
}

/// 一个回答。`id` 原样交回 [`AskAnswer::Option`]；`rules` 是 allowAlways 要保存的规则原文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskOption {
    pub id: String,
    pub kind: AskOptionKind,
    #[serde(default)]
    pub rules: Vec<String>,
}

/// 内核的权限回答只有允许（可带要记住的规则）与拒绝：没有 `allowSession` 与 `refine`（§10.5）。
/// `fullAccess` 是允许这一次并切到完全访问（KernelHost 先经 §11.3 的确认，再发 `SetMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AskOptionKind {
    AllowOnce,
    AllowAlways,
    AllowSession,
    FullAccess,
    Deny,
    Refine,
}

/// 提问的一页。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QuestionPage {
    pub header: String,
    pub question: String,
    pub multi: bool,
    pub options: Vec<QuestionOption>,
}

/// 提问的一个回答；`label` 去掉了模型写的 `(Recommended)`，`recommended` 记下它。回答的值是选项的原标签。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QuestionOption {
    pub value: String,
    pub label: String,
    pub description: String,
    /// 选中时在行下面显示的等宽文字；没有时为 ""。
    pub preview: String,
    pub recommended: bool,
}
