//! 远端调用失败时给模型的说明（远端执行 F96，docs/specs/rust-cli/remote-exec.md §23.6）：连接层的错误与执行器重启写成一句
//! “有没有执行、能不能重试、下一步做什么”，文字在 `prompts/remote/`。消息名换成工具能懂的说法（`prompts::remote::action`）。
//!
//! 不改的：能力位缺失（`Unsupported`，`stat_many`、`mcp.declare` 的回退靠它）、执行器自己的错误（env-local 原样的
//! `not found` 等，与本机相同）、协议错误。

use rust_cli_exec_client::ClientError;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::op::Op;
use rust_cli_kernel_api::env::EnvError;
use rust_cli_prompts::texts;

fn io(message: String) -> EnvError {
    EnvError::Io { message }
}

/// 连接为什么结束了（说明里括号中的原因）。
fn reason(error: &ClientError) -> String {
    match error {
        ClientError::Closed => "disconnected by the user".to_owned(),
        ClientError::NotSent(inner) => reason(inner),
        ClientError::Refused { message, .. } => {
            format!("the executor refused the connection: {message}")
        }
        ClientError::Disconnected(message)
        | ClientError::Rejected(message)
        | ClientError::Connect(message) => message.trim().to_owned(),
        other => other.to_string(),
    }
}

fn render(template: &str, machine: &str, op: Op, reason: &str) -> String {
    texts::render(
        template,
        &[
            ("machine", machine),
            ("action", rust_cli_prompts::remote::action(op.as_str())),
            ("reason", reason),
        ],
    )
}

/// 执行器换了实例、这次调用的结果不明（§18.5）：命令的流是“命令被停掉了”，别的是“可能已经执行了”。
fn restarted(machine: &str, op: Op) -> EnvError {
    let template = if op == Op::WatchCommand {
        texts::remote_restarted_command()
    } else {
        texts::remote_restarted()
    };
    io(render(template, machine, op, ""))
}

/// 连接层的错误 → `EnvError`。
pub(crate) fn client(machine: &str, op: Op, error: ClientError) -> EnvError {
    match error {
        ClientError::Unsupported { what } => EnvError::Unsupported { what },
        ClientError::Interrupted { .. } => restarted(machine, op),
        ClientError::NotSent(cause) => io(render(
            texts::remote_ended_unsent(),
            machine,
            op,
            &reason(&cause),
        )),
        ended @ (ClientError::Closed
        | ClientError::Disconnected(_)
        | ClientError::Refused { .. }
        | ClientError::Rejected(_)) => {
            // 发出过：命令可能还在那台机器上跑；有副作用的调用结果不明；只读的没有改动。
            let template = if op == Op::WatchCommand {
                texts::remote_ended_command()
            } else if op.is_mutation() {
                texts::remote_ended_unknown()
            } else {
                texts::remote_ended_read()
            };
            io(render(template, machine, op, &reason(&ended)))
        }
        other => io(other.to_string()),
    }
}

/// 执行器回的错误：重启前没有终态（`interrupted`）的写成结果不明；别的照旧（同本机）。
pub(crate) fn wire(machine: &str, op: Op, error: WireError) -> EnvError {
    match error {
        WireError::Interrupted { .. } => restarted(machine, op),
        other => crate::from_wire::error(other),
    }
}
