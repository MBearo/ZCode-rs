//! 原始结果的两种解释（docs/specs/rust-cli/hooks.md §4.1、§4.3）。
//!
//! REPL 路径：command 与 http 的结果按退出码与 JSON 输出解释。“外部”路径：每个 hook 的结果单独给出。

use rust_cli_base::proto::http;
use serde_json::Value;

use crate::consts::BLOCKING_EXIT;
use crate::output::{self, Parsed};
use crate::result::{Executed, HookResult, Outcome, Raw, Summary};

fn no_stderr(stderr: &str) -> &str {
    if stderr.is_empty() {
        "No stderr output"
    } else {
        stderr
    }
}

/// 退出码 2 的阻塞原因（`[<command>]: <stderr>`，stderr 不去空白）。
fn exit_two(command: &str, stderr: &str) -> HookResult {
    HookResult::blocking(
        format!("[{command}]: {}", no_stderr(stderr)),
        command.to_owned(),
    )
}

/// 校验错误后附上非零退出码的 stderr。
fn with_stderr(message: &str, exit: i32, stderr: &str) -> String {
    let stderr = stderr.trim();
    if exit != 0 && !stderr.is_empty() {
        format!("{message}\n\nHook exited {exit} with stderr:\n{stderr}")
    } else {
        message.to_owned()
    }
}

fn command_result(command: &str, event: &str, stdout: &str, stderr: &str, exit: i32) -> HookResult {
    match output::parse(stdout) {
        Parsed::Invalid(_) | Parsed::Plain if exit == BLOCKING_EXIT => exit_two(command, stderr),
        Parsed::Invalid(message) => HookResult::non_blocking(format!(
            "JSON validation failed: {}",
            with_stderr(&message, exit, stderr)
        )),
        Parsed::Async if exit == BLOCKING_EXIT => exit_two(command, stderr),
        Parsed::Async if exit != 0 => HookResult::non_blocking(format!(
            "Announced async, then failed with status code {exit}: {}",
            no_stderr(stderr.trim())
        )),
        Parsed::Async => HookResult::default(),
        Parsed::Json(json) => match output::process(&json, command, event) {
            Ok(mut result) => {
                if exit == BLOCKING_EXIT && result.blocking.is_none() {
                    let blocking = exit_two(command, stderr);
                    result.blocking = blocking.blocking;
                    result.outcome = Outcome::Blocking;
                    result.success_content = None;
                }
                result
            }
            Err(error) => HookResult::non_blocking(format!("Failed to run: {error}")),
        },
        Parsed::Plain if exit == 0 => HookResult {
            success_content: Some(stdout.trim().to_owned()),
            ..HookResult::default()
        },
        Parsed::Plain => HookResult::non_blocking(format!(
            "Failed with non-blocking status code: {}",
            no_stderr(stderr.trim())
        )),
    }
}

fn http_result(url: &str, event: &str, status: u16, body: &str) -> HookResult {
    if !http::is_success(status) {
        return HookResult::non_blocking(format!("HTTP {status} from {url}"));
    }
    match output::parse_http(body) {
        Parsed::Invalid(message) => {
            HookResult::non_blocking(format!("JSON validation failed: {message}"))
        }
        Parsed::Async | Parsed::Plain => HookResult::default(),
        Parsed::Json(json) => output::process(&json, url, event)
            .unwrap_or_else(|error| HookResult::non_blocking(format!("Failed to run: {error}"))),
    }
}

/// REPL 路径：一个 hook 的结果。
pub(crate) fn repl(executed: Executed, command: &str, event: &str) -> HookResult {
    match executed {
        Executed::Result(result) => *result,
        Executed::Raw(Raw::Command {
            stdout,
            stderr,
            exit,
        }) => command_result(command, event, &stdout, &stderr, exit),
        Executed::Raw(Raw::Http { status, body }) => http_result(command, event, status, &body),
        Executed::Raw(Raw::Cancelled | Raw::TimedOut) => HookResult::cancelled(),
        Executed::Raw(Raw::Backgrounded) => HookResult::default(),
        Executed::Raw(Raw::Failed(message)) => HookResult::non_blocking(message),
    }
}

fn blocked_by_json(json: &serde_json::Map<String, Value>) -> bool {
    json.get("decision").and_then(Value::as_str) == Some("block")
}

/// “外部”路径：一个 hook 的结果（HookOutsideReplResult）。
pub(crate) fn outside(executed: Executed, command: &str) -> Summary {
    let summary = |succeeded: bool, output: String, blocked: bool| Summary {
        command: command.to_owned(),
        succeeded,
        output,
        blocked,
        cancelled: false,
    };
    match executed {
        Executed::Raw(Raw::Command {
            stdout,
            stderr,
            exit,
        }) => {
            let json = match output::parse(&stdout) {
                Parsed::Invalid(message) if exit != BLOCKING_EXIT => {
                    return summary(false, with_stderr(&message, exit, &stderr), false);
                }
                Parsed::Json(json) => Some(json),
                _ => None,
            };
            let json_blocked = json.as_ref().is_some_and(blocked_by_json);
            let output = if json_blocked {
                json.as_ref()
                    .and_then(|json| json.get("reason"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or(stderr)
            } else if exit == 0 {
                stdout
            } else {
                stderr
            };
            summary(exit == 0, output, exit == BLOCKING_EXIT || json_blocked)
        }
        Executed::Raw(Raw::Http { status, body }) => {
            if !http::is_success(status) {
                return summary(false, format!("HTTP {status} from {command}"), false);
            }
            match output::parse_http(&body) {
                Parsed::Invalid(message) => summary(false, message, false),
                Parsed::Json(json) => summary(true, body, blocked_by_json(&json)),
                Parsed::Async | Parsed::Plain => summary(true, body, false),
            }
        }
        Executed::Raw(Raw::Cancelled | Raw::TimedOut) => Summary {
            cancelled: true,
            ..summary(false, "Hook cancelled".to_owned(), false)
        },
        Executed::Raw(Raw::Backgrounded) => summary(true, String::new(), false),
        Executed::Raw(Raw::Failed(message)) => summary(false, message, false),
        Executed::Result(result) => summary(
            result.outcome == Outcome::Success,
            result.error.unwrap_or_default(),
            result.blocking.is_some(),
        ),
    }
}

#[cfg(test)]
#[path = "interpret_tests.rs"]
mod tests;
