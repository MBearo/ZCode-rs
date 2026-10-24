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

use crate::from_wire::{client_error, command_event, error, pdf_render, text_read};

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
    assert_eq!(
        client_error(ClientError::Unsupported {
            what: "files.stat".to_owned()
        }),
        EnvError::Unsupported {
            what: "files.stat".to_owned()
        }
    );
    assert!(matches!(
        client_error(ClientError::Closed),
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
