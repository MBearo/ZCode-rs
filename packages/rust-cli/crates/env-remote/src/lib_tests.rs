//! 客户端侧的换算：错误一一对应、字节字段从二进制段取、越界是 `io` 错误。端口的行为由 executor-host 的合同测试覆盖
//! （同一套测试对本机与 loopback 远端各跑一遍）。

use rust_cli_exec_client::ClientError;
use rust_cli_exec_wire::BodyBuilder;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::dto::core as w;
use rust_cli_exec_wire::dto::files as wf;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_kernel_api::env::{CommandEvent, EnvError, Outcome};
use rust_cli_kernel_api::env_files::TextRead;
use rust_cli_kernel_api::env_pdf::PdfRender;
use rust_cli_kernel_api::ids::OperationId;

use crate::from_wire::{command_event, error, pdf_render, text_read};

#[test]
fn wire_errors_map_one_to_one_and_protocol_errors_become_io() {
    let cases = [
        (
            WireError::NotFound {
                path: "/a".to_owned(),
            },
            EnvError::NotFound {
                path: "/a".to_owned(),
            },
        ),
        (
            WireError::PreconditionFailed {
                path: "/a".to_owned(),
            },
            EnvError::PreconditionFailed {
                path: "/a".to_owned(),
            },
        ),
        (
            WireError::PermissionDenied {
                message: "m".to_owned(),
            },
            EnvError::PermissionDenied {
                message: "m".to_owned(),
            },
        ),
        (
            WireError::Stale {
                message: "m".to_owned(),
            },
            EnvError::Stale {
                message: "m".to_owned(),
            },
        ),
        (
            WireError::OperationConflict {
                operation: "op".to_owned(),
            },
            EnvError::OperationConflict {
                operation: OperationId::new("op"),
            },
        ),
        (
            WireError::Io {
                message: "m".to_owned(),
            },
            EnvError::Io {
                message: "m".to_owned(),
            },
        ),
        (
            WireError::Unsupported {
                what: "x".to_owned(),
            },
            EnvError::Unsupported {
                what: "x".to_owned(),
            },
        ),
    ];
    for (wire, env) in cases {
        assert_eq!(error(wire), env);
    }
    assert!(matches!(
        error(WireError::Interrupted {
            message: "m".to_owned()
        }),
        EnvError::Io { .. }
    ));
}

#[test]
fn byte_fields_come_from_the_body() {
    let mut body = BodyBuilder::new();
    let text = body.push("第一行\n".as_bytes());
    let first = body.push(b"\xff\xd8page1");
    let second = body.push(b"\xff\xd8page2");
    let data = body.push(b"out");
    let body = body.finish();
    let slice = wf::TextSlice {
        content: text,
        line_count: 1,
        total_lines: 1,
        total_bytes: 10,
        read_bytes: 10,
        mtime_ms: 1,
        version: "v".to_owned(),
    };
    let TextRead::Text(read) = text_read(wf::TextRead::Text { slice }, &body).unwrap() else {
        panic!("not text");
    };
    assert_eq!(read.content, "第一行\n");
    let rendered = pdf_render(
        wf::PdfRender::Rendered {
            pages: vec![first, second],
        },
        &body,
    )
    .unwrap();
    assert_eq!(
        rendered,
        PdfRender::Rendered {
            pages: vec![b"\xff\xd8page1".to_vec(), b"\xff\xd8page2".to_vec()]
        }
    );
    let event = wc::CommandEvent::Output {
        stream: wc::OutputStream::Stdout,
        data,
        cursor: 3,
    };
    assert!(matches!(
        command_event(event, &body).unwrap(),
        CommandEvent::Output { cursor: 3, .. }
    ));
    let outside = wc::CommandEvent::Output {
        stream: wc::OutputStream::Stdout,
        data: rust_cli_exec_wire::Blob { at: 1000, len: 1 },
        cursor: 4,
    };
    assert!(matches!(
        command_event(outside, &body),
        Err(EnvError::Io { .. })
    ));
    let finished = wc::CommandEvent::Finished {
        outcome: w::Outcome::Cancelled,
        exit_code: Some(137),
        timed_out: false,
        cwd_reset: None,
        output_bytes: 9,
        output_limit: Some("limit".to_owned()),
    };
    assert_eq!(
        command_event(finished, &body).unwrap(),
        CommandEvent::Finished {
            outcome: Outcome::Cancelled,
            exit_code: Some(137),
            timed_out: false,
            cwd_reset: None,
            output_bytes: 9,
            output_limit: Some("limit".to_owned()),
        }
    );
}

/// 远端执行 F96（remote-exec.md §23.6）：连接层的失败与执行器重启写成“有没有执行、能不能重试、下一步做什么”；`op` 写成
/// 工具能懂的说法；能力位缺失、执行器自己的错误照旧。
#[test]
fn remote_failures_say_whether_the_call_ran_and_what_to_do() {
    use rust_cli_exec_wire::op::Op;

    use crate::failure;
    let io = |error: EnvError| match error {
        EnvError::Io { message } => message,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        io(failure::client(
            "dev-a",
            Op::WriteFile,
            ClientError::NotSent(Box::new(ClientError::Closed))
        )),
        "Machine \"dev-a\" disconnected before this call was sent (disconnected by the user), so it did not run. Retry once the machine is connected again, or run it on the local machine instead."
    );
    assert_eq!(
        io(failure::client("dev-a", Op::ReadFile, ClientError::Closed)),
        "Machine \"dev-a\" disconnected before the result came back (disconnected by the user). Nothing was changed; retry once the machine is connected again, or run it on the local machine instead."
    );
    assert_eq!(
        io(failure::client(
            "dev-a",
            Op::WriteFile,
            ClientError::Rejected("Permission denied (publickey).\n".to_owned())
        )),
        "Machine \"dev-a\" disconnected before the file write finished (Permission denied (publickey).), so the outcome is unknown: it may already have run. Once the machine is connected again, check the state on that machine before deciding whether to do it again."
    );
    assert_eq!(
        io(failure::client(
            "gpu-1",
            Op::WatchCommand,
            ClientError::Closed
        )),
        "Machine \"gpu-1\" disconnected while the command was running (disconnected by the user). The command may still be running there, but its result is not available here. Once the machine is connected again, check the state on that machine before running the command again."
    );
    assert_eq!(
        io(failure::client(
            "dev-a",
            Op::StartCommand,
            ClientError::Interrupted {
                op: "commands.start_command".to_owned()
            }
        )),
        "The executor on machine \"dev-a\" restarted before the command finished, so the outcome is unknown: it may already have run. Check the state on that machine before deciding whether to do it again."
    );
    assert_eq!(
        io(failure::client(
            "dev-a",
            Op::WatchCommand,
            ClientError::Interrupted {
                op: "commands.watch_command".to_owned()
            }
        )),
        "The executor on machine \"dev-a\" restarted while the command was running, which stopped the command; part of it may have run. Check the state on that machine before running it again."
    );
    assert_eq!(
        io(failure::wire(
            "dev-a",
            Op::PutFile,
            WireError::Interrupted {
                message: "files.put_file did not finish before the executor restarted".to_owned()
            }
        )),
        "The executor on machine \"dev-a\" restarted before the file write finished, so the outcome is unknown: it may already have run. Check the state on that machine before deciding whether to do it again."
    );
    // 执行器拒绝重连：原因写明。
    assert!(
        io(failure::client(
            "dev-a",
            Op::Stat,
            ClientError::Refused {
                code: rust_cli_exec_wire::handshake::RefuseCode::Protocol,
                message: "protocol 2".to_owned()
            }
        ))
        .contains("(the executor refused the connection: protocol 2)")
    );
    // 不改的：能力位缺失（回退的信号）、执行器自己的错误。
    assert_eq!(
        failure::client(
            "dev-a",
            Op::StatMany,
            ClientError::Unsupported {
                what: "files.stat_many".to_owned()
            }
        ),
        EnvError::Unsupported {
            what: "files.stat_many".to_owned()
        }
    );
    assert_eq!(
        failure::wire(
            "dev-a",
            Op::ReadFile,
            WireError::NotFound {
                path: "/a".to_owned()
            }
        ),
        EnvError::NotFound {
            path: "/a".to_owned()
        }
    );
    // 内部的方法名不出现在给模型的话里。
    let text = io(failure::client("dev-a", Op::PutFile, ClientError::Closed));
    assert!(
        !text.contains("files.") && !text.contains("put_file"),
        "{text}"
    );
}
