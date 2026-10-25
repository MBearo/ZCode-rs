//! 代理只认第一帧（远端执行 F96，remote-exec.md §23.5）：是 hello 时在 JSON 头里加上这条 ssh 会话的 `session_env`（白名单
//! 里的名字，这条会话没有时是 null），头里别的字段原样保留；别的帧原样。

use bytes::Bytes;
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::admin::{AdminAction, AdminRequest};
use rust_cli_exec_wire::frame::{Decoded, Frame, Kind, decode};
use rust_cli_exec_wire::handshake::Hello;
use serde_json::json;

use super::{session_env, with_session_env};

fn hello_frame() -> Bytes {
    let header = json!({
        "protocol": 1, "client_version": "0.1.0", "client_instance": "c1", "epoch": 2,
        "capabilities": ["files"], "trace": {"id": "t1"}
    });
    Frame::encode(Kind::Hello, &header, &Body::empty()).unwrap()
}

fn header(payload: Bytes) -> serde_json::Value {
    let Ok(Decoded::Frame(frame)) = decode(payload) else {
        panic!("not a frame");
    };
    frame.header().unwrap()
}

#[test]
fn the_session_environment_is_taken_from_the_whitelist() {
    let present =
        session_env(|name| (name == "SSH_AUTH_SOCK").then(|| "/tmp/ssh-x/agent.7".to_owned()));
    assert_eq!(
        present,
        [(
            "SSH_AUTH_SOCK".to_owned(),
            Some("/tmp/ssh-x/agent.7".to_owned())
        )]
        .into()
    );
    let absent = session_env(|_| None);
    assert_eq!(absent, [("SSH_AUTH_SOCK".to_owned(), None)].into());
}

#[test]
fn a_hello_gets_the_session_environment_and_keeps_its_fields() {
    let session =
        session_env(|name| (name == "SSH_AUTH_SOCK").then(|| "/tmp/ssh-x/agent.7".to_owned()));
    let rewritten = with_session_env(hello_frame(), &session);
    let value = header(rewritten.clone());
    assert_eq!(
        value["session_env"],
        json!({"SSH_AUTH_SOCK": "/tmp/ssh-x/agent.7"})
    );
    // 不认识的字段（新客户端的）原样留着。
    assert_eq!(value["trace"], json!({"id": "t1"}));
    let Ok(Decoded::Frame(frame)) = decode(rewritten) else {
        panic!("not a frame");
    };
    let hello: Hello = frame.header().unwrap();
    assert_eq!((hello.client_instance.as_str(), hello.epoch), ("c1", 2));
    // 这条会话没有：写 null（执行器去掉它，不留拉起它的那条会话的）。
    let absent = header(with_session_env(hello_frame(), &session_env(|_| None)));
    assert_eq!(absent["session_env"], json!({"SSH_AUTH_SOCK": null}));
}

#[test]
fn other_first_frames_pass_unchanged() {
    let admin = Frame::encode(
        Kind::Admin,
        &AdminRequest {
            action: AdminAction::Status,
        },
        &Body::empty(),
    )
    .unwrap();
    let session = session_env(|_| Some("/tmp/a".to_owned()));
    assert_eq!(with_session_env(admin.clone(), &session), admin);
    let garbage = Bytes::from_static(b"\x01not json");
    assert_eq!(with_session_env(garbage.clone(), &session), garbage);
}
