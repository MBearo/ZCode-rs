//! 判决链（shell-permissions.md §4.1）。
//!
//! 内层顺序：整个工具的 deny 规则 → 整个工具的 ask 规则 → 工具自身的检查（Bash、文件工具、WebFetch
//! 的内置检查在这里）→ 检查拒绝则拒绝 → 命中 ask 规则或 bypass 也问的安全检查（危险删除）的询问 →
//! 其余安全检查（敏感文件）在 bypassPermissions / guarded 下放行、否则询问 →
//! bypassPermissions 放行 → 整个工具的 allow 规则放行 → 没有意见转成询问。
//! 外层按模式改写询问：dontAsk 拒绝；auto（与开启自动语义的 plan）先走 acceptEdits 快速放行
//! （只在 auto），再交给分类器；没有人能回答时（`avoid_prompts`）询问直接按拒绝处理。

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::{
    PermissionDecision, PermissionMode, PermissionRule, RuleBehavior,
};
use serde_json::Value;

use crate::check::{AskKind, Check, Inputs};
use crate::classifier::{AutoClassifier, ClassifierRequest, ClassifierVerdict};
use crate::texts;
use crate::{bash, files, skill};

/// 内层的结论。
#[derive(Debug, Clone, PartialEq)]
pub enum Inner {
    Allow {
        updated_input: Option<Value>,
    },
    Deny {
        message: String,
    },
    Ask {
        message: String,
        kind: AskKind,
        suggestions: Vec<PermissionRule>,
    },
}

/// WebFetch 的 `domain:` 规则。
fn web_fetch(input: &Value, inputs: &Inputs<'_>) -> Check {
    let host = input
        .get("url")
        .and_then(Value::as_str)
        .and_then(|url| url.split_once("://").map(|(_, rest)| rest))
        .map(|rest| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .map(|authority| authority.rsplit('@').next().unwrap_or_default())
        .map(|host| host.split(':').next().unwrap_or_default().to_lowercase())
        .unwrap_or_default();
    let content = format!("domain:{host}");
    let hit = |behavior| {
        inputs
            .rules
            .contents("WebFetch", behavior)
            .iter()
            .any(|(rule, _)| *rule == content)
    };
    if hit(RuleBehavior::Deny) {
        return Check::deny(format!("WebFetch denied access to {content}."));
    }
    if hit(RuleBehavior::Ask) {
        return Check::Ask {
            message: texts::request_message("WebFetch", None),
            kind: AskKind::Rule,
            suggestions: Vec::new(),
        };
    }
    if hit(RuleBehavior::Allow) {
        return Check::Allow;
    }
    // 预批准域名排在三类规则之后（docs/specs/rust-cli/tools-more.md §4）。
    let url = input.get("url").and_then(Value::as_str).unwrap_or_default();
    if crate::web_preapproved::preapproved(url) {
        return Check::Allow;
    }
    Check::Passthrough {
        reason: None,
        suggestions: vec![PermissionRule {
            tool: "WebFetch".to_owned(),
            content: Some(content),
        }],
    }
}

/// 没有自己检查的内置工具，按名字套用对应的内置检查。
pub fn builtin(tool: &str, input: &Value, inputs: &Inputs<'_>) -> Check {
    if tool == "Bash" {
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return bash::check(command, inputs);
    }
    if tool == "WebFetch" {
        return web_fetch(input, inputs);
    }
    if tool == skill::TOOL {
        return skill::check(input, inputs);
    }
    if let Some((access, field)) = files::tool_access(tool) {
        return files::check(access, files::input_path(input, field), inputs);
    }
    Check::Passthrough {
        reason: None,
        suggestions: Vec::new(),
    }
}

/// 工具自己的 `check_permissions` 结果换成检查结果；没有意见时用内置检查。
pub fn own(
    tool: &str,
    decision: PermissionDecision,
    input: &Value,
    inputs: &Inputs<'_>,
) -> (Check, Option<Value>) {
    match decision {
        PermissionDecision::Passthrough => (builtin(tool, input, inputs), None),
        // Skill 工具放行只有安全字段的 skill，内容 deny 规则仍然优先（ext.md §2.4）。
        PermissionDecision::Allow { updated_input } if tool == skill::TOOL => {
            (skill::deny_first(input, inputs), updated_input)
        }
        PermissionDecision::Allow { updated_input } => (Check::Allow, updated_input),
        PermissionDecision::Deny { message } => (Check::deny(message), None),
        PermissionDecision::Ask {
            message,
            suggestions,
        } => (
            Check::Ask {
                message,
                kind: AskKind::Other,
                suggestions,
            },
            None,
        ),
    }
}

/// 内层判决。`check` 是工具自身（含内置）检查的结果。
pub fn inner(tool: &str, check: Check, updated_input: Option<Value>, inputs: &Inputs<'_>) -> Inner {
    if inputs.rules.whole_tool(tool, RuleBehavior::Deny).is_some() {
        return Inner::Deny {
            message: texts::tool_denied(tool),
        };
    }
    if inputs.rules.whole_tool(tool, RuleBehavior::Ask).is_some() {
        return Inner::Ask {
            message: texts::request_message(tool, None),
            kind: AskKind::Rule,
            suggestions: Vec::new(),
        };
    }
    let (message, reason, kind, suggestions) = match check {
        Check::Deny { message } => return Inner::Deny { message },
        // 修复线 F20：guarded 下的危险命令先询问一次（Node `userApprovalRule`，shell-permissions.md §4.6.1）。排在
        // deny 之后、放行之前：allow 规则与只读判定都不盖过它；没有建议规则，只批准这一次。
        _ if inputs.dangerous => {
            return Inner::Ask {
                message: texts::GUARDED_ASK.to_owned(),
                kind: AskKind::Immune,
                suggestions: Vec::new(),
            };
        }
        Check::Allow => return Inner::Allow { updated_input },
        // 修复线 F68（审计 P1）：bypass 类模式下只有 bypass 也问的安全检查留下，敏感文件放行；hook 放行之后的检查
        // 照旧询问。以前安全检查一律 bypass 也问。
        Check::Ask {
            kind: AskKind::Safety,
            ..
        } if bypass_category(inputs.mode) && !inputs.hook_check => {
            return Inner::Allow { updated_input };
        }
        Check::Ask {
            message,
            kind: kind @ (AskKind::Rule | AskKind::Safety | AskKind::Immune),
            suggestions,
        } => {
            return Inner::Ask {
                message,
                kind,
                suggestions,
            };
        }
        Check::Ask {
            message,
            kind,
            suggestions,
        } => (Some(message), None, kind, suggestions),
        Check::Passthrough {
            reason,
            suggestions,
        } => (None, reason, AskKind::Other, suggestions),
    };
    if bypass_category(inputs.mode) || inputs.rules.whole_tool(tool, RuleBehavior::Allow).is_some()
    {
        return Inner::Allow { updated_input };
    }
    Inner::Ask {
        message: message.unwrap_or_else(|| texts::request_message(tool, reason.as_ref())),
        kind,
        suggestions,
    }
}

/// bypass 类模式：guarded 在危险命令以外同 bypassPermissions（Node `mode === "yolo" || mode === "guarded"`）。
fn bypass_category(mode: PermissionMode) -> bool {
    matches!(
        mode,
        PermissionMode::BypassPermissions | PermissionMode::Guarded
    )
}

/// 外层的选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outer {
    /// plan 模式按自动模式的语义走分类器（默认开启）。
    pub auto_in_plan: bool,
    /// 没有人能回答询问。
    pub avoid_prompts: bool,
}

/// 一次调用。
pub struct Call<'a> {
    pub tool: &'a str,
    pub input: &'a Value,
    pub session: &'a SessionId,
    pub mode: PermissionMode,
}

/// 外层：按模式改写内层的询问。`accept_edits` 是同一调用在 acceptEdits 模式下的内层结论
/// （自动模式的快速放行）。
pub async fn outer(
    call: &Call<'_>,
    result: Inner,
    accept_edits: impl FnOnce() -> Inner,
    classifier: &dyn AutoClassifier,
    options: Outer,
) -> PermissionDecision {
    let Call {
        tool,
        input,
        session,
        mode,
    } = *call;
    let (message, kind, suggestions) = match result {
        Inner::Allow { updated_input } => return PermissionDecision::Allow { updated_input },
        Inner::Deny { message } => return PermissionDecision::Deny { message },
        Inner::Ask {
            message,
            kind,
            suggestions,
        } => (message, kind, suggestions),
    };
    if mode == PermissionMode::DontAsk {
        return PermissionDecision::Deny {
            message: texts::dont_ask_rejected(tool),
        };
    }
    let automatic =
        mode == PermissionMode::Auto || (mode == PermissionMode::Plan && options.auto_in_plan);
    if automatic && !kind.is_safety() {
        if mode == PermissionMode::Auto
            && let Inner::Allow { updated_input } = accept_edits()
        {
            return PermissionDecision::Allow { updated_input };
        }
        let request = ClassifierRequest {
            session: session.clone(),
            tool: tool.to_owned(),
            input: input.clone(),
        };
        return match classifier.classify(request).await {
            ClassifierVerdict::Allow => PermissionDecision::Allow {
                updated_input: None,
            },
            ClassifierVerdict::Block { reason } => PermissionDecision::Deny {
                message: texts::classifier_blocked(&reason),
            },
            ClassifierVerdict::Unavailable => PermissionDecision::Deny {
                message: texts::CLASSIFIER_UNAVAILABLE.to_owned(),
            },
        };
    }
    if options.avoid_prompts {
        return PermissionDecision::Deny { message };
    }
    PermissionDecision::Ask {
        message,
        suggestions,
    }
}
