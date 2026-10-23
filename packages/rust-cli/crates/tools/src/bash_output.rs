//! Bash 结果的文本（shell-permissions.md §1.3）。后台通知的文本在
//! `rust_cli_query::task_notification`。
//!
//! 逐字固定：成功、退出码、超时、超长输出、cwd 重置、空输出、后台。

use rust_cli_kernel_api::ids::{OperationId, ShellTaskId};

use crate::budget::{persisted_message, preview};

/// 成功结果里内联的输出上限（`BASH_MAX_OUTPUT_LENGTH` 默认 30000，按字节读文件头）。
pub const INLINE_LIMIT: usize = 30_000;

/// 任务 ID：`b` 加 8 位 36 进制，从 operationId 派生，重放时不变。
pub fn task_id(operation: &OperationId) -> ShellTaskId {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    // FNV-1a 64 位。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in operation.as_str().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut id = String::from("b");
    for _ in 0..8 {
        id.push(char::from(ALPHABET[(hash % 36) as usize]));
        hash /= 36;
    }
    ShellTaskId::new(id)
}

/// 按最后一个子命令的命令名解释退出码。grep / rg / find / diff / test / `[`
/// 的退出码 1 不算错误。
pub fn is_error(last_command: &str, exit_code: i32) -> bool {
    match last_command {
        "grep" | "rg" | "find" | "diff" | "test" | "[" => exit_code >= 2,
        _ => exit_code != 0,
    }
}

/// 去掉开头的空白行与结尾的空白（`stdout.replace(/^(\s*\n)+/, "").trimEnd()`）。
pub fn strip_output(stdout: &str) -> &str {
    let mut rest = stdout;
    while let Some(newline) = rest.find('\n') {
        if rest[..newline].chars().all(char::is_whitespace) {
            rest = &rest[newline + 1..];
        } else {
            break;
        }
    }
    rest.trim_end()
}

/// 失败结果的文本：`Exit code N` 加合流的输出。超长时由执行管线的
/// 错误文本的格式化保留首尾。
pub fn failure(exit_code: i32, output: &str) -> String {
    [format!("Exit code {exit_code}"), output.to_owned()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 超时的输出：`Command timed out after <时长>` 放在输出前面。时长不带选项地格式化，
/// 即 kernel-api 的 `metering::format_duration`。修复线 F58：以前这里另有一份只写非零单位的格式，
/// 120000 ms 写成 `2m`（应为 `2m 0s`）。
pub fn timed_out(timeout_ms: u64, output: &str) -> String {
    let notice = format!(
        "Command timed out after {}",
        rust_cli_kernel_api::metering::format_duration(timeout_ms)
    );
    if output.is_empty() {
        notice
    } else {
        format!("{notice}\n{output}")
    }
}

/// 输出文件超过上限被结束的输出：说明放在输出前面（有输出时用换行连接）。
pub fn killed_for_size(note: &str, output: &str) -> String {
    if output.is_empty() {
        note.to_owned()
    } else {
        format!("{note}\n{output}")
    }
}

/// 成功结果的文本。`head` 是输出文件的前 [`INLINE_LIMIT`] 字节，`total` 是总字节数；超出时给
/// `<persisted-output>` 预览，位置是执行环境里的输出文件。
pub fn success(head: &str, total: u64, output_file: &str, cwd_reset: Option<&str>) -> String {
    let stdout = strip_output(head);
    let stdout = if total > INLINE_LIMIT as u64 {
        let (shown, more) = preview(stdout);
        let size = usize::try_from(total).unwrap_or(usize::MAX);
        persisted_message(output_file, size, shown, more)
    } else {
        stdout.to_owned()
    };
    let reset = cwd_reset.map(|cwd| format!("Shell cwd was reset to {cwd}"));
    [Some(stdout), reset]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 后台命令里有 cd 时启动说明后面的一行：后台命令不改会话的当前目录（shell-permissions.md §1.2，修复线 F68）。
pub fn background_cwd_hint(cwd: &str) -> String {
    format!(
        "Session cwd remains {cwd}; directory changes made by the backgrounded command do not apply to subsequent commands."
    )
}

/// 后台命令启动后立即返回的文本（非用户手动转后台的分支）。
pub fn backgrounded(task: &ShellTaskId, output_file: &str) -> String {
    format!(
        "Command running in background with ID: {task}. Output is being written to: {output_file}. You will be notified when it completes. To check interim output, use Read on that file path."
    )
}

/// 前台命令到了超时、转到后台时的结果（shell-permissions.md §1.2.1）。超时按秒四舍五入、至少 1 s；`deadline_ms` 是
/// 转后台之后的期限，没有时（交互会话）没有期限一句。转后台之前的输出不放进结果（在输出文件里）。
pub fn moved_to_background(
    timeout_ms: u64,
    task: &ShellTaskId,
    output_file: &str,
    deadline_ms: Option<u64>,
) -> String {
    let seconds = (timeout_ms.saturating_add(500) / 1000).max(1);
    let deadline = deadline_ms
        .map(|ms| {
            format!(
                " If it is still running after {} in the background, it will be stopped and you will be notified.",
                rust_cli_kernel_api::metering::format_duration_trimmed(ms)
            )
        })
        .unwrap_or_default();
    format!(
        "Command did not complete within its {seconds}s timeout and was moved to the background (ID: {task}). Output is being written to: {output_file}. You will be notified when it completes.{deadline} To check interim output, use Read on that file path."
    )
}

#[cfg(test)]
#[path = "bash_output_tests.rs"]
mod tests;
