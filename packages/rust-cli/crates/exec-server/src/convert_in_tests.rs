//! 线上的四个可选字段拼回命令的种类（F128，审查 K10）：合法的四种组合各自对应一种，别的组合是 `bad_request`。

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::command as wc;
use rust_cli_exec_wire::envelope::WireError;
use rust_cli_kernel_api::env::CommandKind;
use serde_json::{Value, json};

use super::start_command;

fn wire(extra: Value) -> wc::StartCommand {
    let mut base = json!({
        "command": "ls", "cwd": "/w", "task": "b1", "shell": "bash", "cwd_scope": {"kind": "pinned"}
    });
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(base).unwrap()
}

fn background() -> Value {
    json!({"description": "d", "kind": "monitor"})
}

#[test]
fn the_four_shapes_map_to_the_four_kinds() {
    let body = Body::empty();
    let kind = |extra| start_command(wire(extra), &body).unwrap().kind;
    assert_eq!(
        kind(json!({})),
        CommandKind::Foreground { on_timeout: None }
    );
    assert!(matches!(
        kind(json!({"on_timeout": {"command": background()}})),
        CommandKind::Foreground {
            on_timeout: Some(_)
        }
    ));
    assert!(matches!(
        kind(json!({"background": background()})),
        CommandKind::Background { .. }
    ));
    assert!(matches!(
        kind(json!({"background": background(), "socket": {"url": "ws://h/x"}})),
        CommandKind::Socket { .. }
    ));
}

#[test]
fn a_hook_reads_its_stdin_from_the_body() {
    let body = Body::new(bytes::Bytes::from_static(b"{}\n"));
    let start =
        start_command(wire(json!({"hook": {"stdin": {"at": 0, "len": 3}}})), &body).unwrap();
    let CommandKind::Hook { hook } = start.kind else {
        panic!("{:?}", start.kind);
    };
    assert_eq!(hook.stdin, "{}\n");
}

#[test]
fn other_combinations_are_bad_requests() {
    let body = Body::new(bytes::Bytes::from_static(b"{}\n"));
    let hook = json!({"stdin": {"at": 0, "len": 3}});
    for extra in [
        json!({"background": background(), "hook": hook}),
        json!({"background": background(), "on_timeout": {"command": background()}}),
        json!({"socket": {"url": "ws://h/x"}}),
        json!({"hook": hook, "on_timeout": {"command": background()}}),
    ] {
        assert!(
            matches!(
                start_command(wire(extra.clone()), &body),
                Err(WireError::BadRequest { .. })
            ),
            "{extra}"
        );
    }
}
