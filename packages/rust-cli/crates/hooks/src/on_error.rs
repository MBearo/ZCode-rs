//! Node 配置的 `onError: "deny"`（hooks.md §4.1 末段）。
//!
//! 只在 PreToolUse、只对写了它的 hook 生效：hook 出错（超时、退出码不是 0 也不是 2、输出解析或校验失败、无法启动）时，
//! 把这一个结果换成与退出码 2 同形的阻塞，交给同一条拦截路径；调用方取消不算出错。
//!
//! 修复线 F68（审计 C1）：以前不解析这个字段，守卫 hook 出错时工具照常执行（fail-open），而 Node 与 App 会拦下。

use crate::output::{self, Parsed};
use crate::result::{Executed, HookResult, Outcome, Raw};

/// stderr 进理由时最多留的字符数（多的截掉，加 `...`）。
const PREVIEW_CHARS: usize = 4000;

/// 出错时的说明；没有出错（成功、阻塞、取消、后台）时为 None。`interpreted` 是这个结果按通常规则的解释。
fn failure(executed: &Executed, interpreted: &HookResult, timeout_ms: u64) -> Option<String> {
    match executed {
        Executed::Raw(Raw::TimedOut) => Some(format!("Hook timed out after {timeout_ms}ms")),
        Executed::Raw(Raw::Failed(message)) => Some(message.clone()),
        Executed::Raw(Raw::Command {
            stdout,
            stderr,
            exit,
        }) if *exit != 0 && *exit != 2 && matches!(output::parse(stdout), Parsed::Plain) => {
            let stderr = stderr.trim();
            Some(if stderr.is_empty() {
                "Hook process exited with status failed".to_owned()
            } else if stderr.chars().count() > PREVIEW_CHARS {
                format!(
                    "{}...",
                    stderr.chars().take(PREVIEW_CHARS).collect::<String>()
                )
            } else {
                stderr.to_owned()
            })
        }
        _ if interpreted.outcome == Outcome::NonBlockingError => interpreted.error.clone(),
        _ => None,
    }
}

/// 写了 `onError: "deny"` 的 PreToolUse hook：出错时换成拦截。
pub(crate) fn deny(
    executed: &Executed,
    interpreted: HookResult,
    command: &str,
    timeout_ms: u64,
) -> HookResult {
    match failure(executed, &interpreted, timeout_ms) {
        Some(detail) => HookResult::blocking(
            format!(
                "[{command}]: PreToolUse hook failed and its onError is \"deny\", so the tool call is blocked: {detail}"
            ),
            command.to_owned(),
        ),
        None => interpreted,
    }
}
