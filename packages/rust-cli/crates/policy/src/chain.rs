//! 判决链（shell-permissions.md §4.1）。
//!
//! 内层顺序：整个工具的 deny 规则 → 整个工具的 ask 规则 → 工具自身的检查（没有意见时按判决主体的内置检查：
//! shell 命令、文件路径、网址、skill）→ 检查拒绝则拒绝 → 命中 ask 规则或 bypass 也问的安全检查（危险删除）的询问 →
//! 其余安全检查（敏感文件）在 bypassPermissions / guarded 下放行、否则询问 →
//! bypassPermissions 放行 → 整个工具的 allow 规则放行 → 没有意见转成询问。
//! 外层按模式改写询问：dontAsk 拒绝；auto（与开启自动语义的 plan）先走 acceptEdits 快速放行
//! （只在 auto），再交给分类器；没有人能回答时（`avoid_prompts`）询问直接按拒绝处理。

use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::{
    PermissionDecision, PermissionMode, PermissionRule, PermissionSubject, RuleBehavior,
};
use serde_json::Value;
use url::Url;

use crate::check::{AskKind, Check, Inputs};
use crate::classifier::{AutoClassifier, ClassifierRequest, ClassifierVerdict};
use crate::texts;
use crate::{bash, files, skill};
use rust_cli_kernel_api::tool_names;

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
        /// 只批准这一次（guarded 的危险命令，shell-permissions.md §4.6.1）：成为 `PermissionDecision::Ask.once`。
        once: bool,
    },
}

/// 网址主体的规则写在这个工具名下（`WebFetch(domain:<host>)`）。
const WEB_FETCH: &str = tool_names::WEB_FETCH;

/// `domain:` 规则比较的主机名（小写，不带端口与用户信息）：按 WHATWG URL 解析，与 WebFetch 工具取网址、预批准域名同一个
/// 解析器；解析不了时为空。
///
/// 修复（F125）：以前按 `://`、`/`、`@`、`:` 切字符串，`https://example.com\@evil.com/` 的规则按 evil.com 查，工具实际取的
/// 却是 example.com（反斜杠在 http(s) 里是路径分隔符），example.com 的 deny 规则拦不住；IPv6 只剩 `[`。
fn rule_host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .unwrap_or_default()
}

/// WebFetch 的 `domain:` 规则。
fn web_fetch(url: &str, inputs: &Inputs<'_>) -> Check {
    let content = format!("domain:{}", rule_host(url));
    let hit = |behavior| {
        inputs
            .rules
            .contents(WEB_FETCH, behavior)
            .iter()
            .any(|(rule, _)| *rule == content)
    };
    if hit(RuleBehavior::Deny) {
        return Check::deny(format!("WebFetch denied access to {content}."));
    }
    if hit(RuleBehavior::Ask) {
        return Check::Ask {
            message: texts::request_message(WEB_FETCH, None),
            kind: AskKind::Rule,
            suggestions: Vec::new(),
        };
    }
    if hit(RuleBehavior::Allow) {
        return Check::Allow;
    }
    // 预批准域名排在三类规则之后（docs/specs/rust-cli/tools-more.md §4）。
    if crate::web_preapproved::preapproved(url) {
        return Check::Allow;
    }
    Check::Passthrough {
        reason: None,
        suggestions: vec![PermissionRule {
            tool: WEB_FETCH.to_owned(),
            content: Some(content),
        }],
    }
}

/// 工具自己没有意见时的内置检查：按工具声明的判决主体（kernel-api `PermissionSubject`）。没有主体的工具交给通用判决链。
///
/// F125（T3）：以前按工具名分派（`Bash`、带 `command` 的 `Monitor`、`WebFetch`、`Skill`、六个文件工具与各自的路径字段）。
pub fn builtin(subject: Option<&PermissionSubject>, inputs: &Inputs<'_>) -> Check {
    match subject {
        Some(PermissionSubject::Shell { command }) => bash::check(command, inputs),
        Some(PermissionSubject::Path { access, path }) => {
            files::check(*access, path.as_deref(), inputs)
        }
        Some(PermissionSubject::Url { url }) => web_fetch(url, inputs),
        Some(PermissionSubject::Skill { name }) => skill::check(name, inputs),
        None => Check::Passthrough {
            reason: None,
            suggestions: Vec::new(),
        },
    }
}

/// 工具自己的 `check_permissions` 结果换成检查结果；没有意见时用内置检查。
pub fn own(
    subject: Option<&PermissionSubject>,
    decision: PermissionDecision,
    inputs: &Inputs<'_>,
) -> (Check, Option<Value>) {
    match decision {
        PermissionDecision::Passthrough => (builtin(subject, inputs), None),
        PermissionDecision::Allow { updated_input } => match subject {
            // Skill 工具放行只有安全字段的 skill，内容 deny 规则仍然优先（ext.md §2.4）。
            Some(PermissionSubject::Skill { name }) => {
                (skill::deny_first(name, inputs), updated_input)
            }
            Some(
                PermissionSubject::Shell { .. }
                | PermissionSubject::Path { .. }
                | PermissionSubject::Url { .. },
            )
            | None => (Check::Allow, updated_input),
        },
        PermissionDecision::Deny { message } => (Check::deny(message), None),
        PermissionDecision::Ask {
            message,
            suggestions,
            once: _,
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
    // 工作线 F76（用户决定 2026-10-23“如果是 yolo 啥也不问”，shell-permissions.md §4.6.2）：yolo 下只有拒绝留下，
    // ask 规则、危险删除、安全检查与工具自身的询问都放行（F68 起 ask 规则与危险删除 bypass 也问，D21）。
    if inputs.full_access {
        return match check {
            Check::Deny { message } => Inner::Deny { message },
            _ => Inner::Allow { updated_input },
        };
    }
    if inputs.rules.whole_tool(tool, RuleBehavior::Ask).is_some() {
        return Inner::Ask {
            message: texts::request_message(tool, None),
            kind: AskKind::Rule,
            suggestions: Vec::new(),
            // 危险命令又命中整个工具的 ask 规则时，这一问同样只批准这一次（工作线 F137 以前 `ask_once` 只看命令，不看是
            // 哪一条询问；判决逐项不变）。
            once: inputs.dangerous,
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
                once: true,
            };
        }
        Check::Allow => return Inner::Allow { updated_input },
        // 修复线 F68（审计 P1）放行了 bypass 类模式下的敏感文件；工作线 F76（用户决定 2026-10-23）guarded 改回询问
        // （App 对自主模式的说明是“在有风险时询问”），yolo 由上面的 `full_access` 放行。
        Check::Ask {
            message,
            kind: kind @ (AskKind::Rule | AskKind::Safety | AskKind::Immune),
            suggestions,
        } => {
            return Inner::Ask {
                message,
                kind,
                suggestions,
                once: false,
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
        once: false,
    }
}

/// bypass 类模式：其他理由的询问放行。guarded 在这一点上同 bypassPermissions（Node `mode === "yolo" || mode === "guarded"`）；
/// ask 规则、危险删除与安全检查 guarded 照样询问（工作线 F76）。
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
    let (message, kind, suggestions, once) = match result {
        Inner::Allow { updated_input } => return PermissionDecision::Allow { updated_input },
        Inner::Deny { message } => return PermissionDecision::Deny { message },
        Inner::Ask {
            message,
            kind,
            suggestions,
            once,
        } => (message, kind, suggestions, once),
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
        once,
    }
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
