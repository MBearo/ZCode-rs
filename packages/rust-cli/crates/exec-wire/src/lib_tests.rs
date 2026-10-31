//! 消息：每种请求与回复的名字、能力位、往返；兼容（不认识的字段、能力位、消息、拒绝原因）。

use std::collections::BTreeSet;

use serde_json::json;

use crate::envelope::{BadRequest, ReplyHeader, ReplyResult, RequestHeader, WireError};
use crate::frame::{Frame, Kind};
use crate::handshake::{Capabilities, Capability, Hello, PROTOCOL, Refuse, RefuseCode};
use crate::op::Op;
use crate::reply::Reply;
use crate::request::Request;
use crate::test_support::{replies, requests};
use crate::{Body, BodyBuilder};

#[test]
fn every_method_has_one_request_whose_json_op_is_its_name() {
    let requests = requests();
    let ops: BTreeSet<&str> = requests
        .iter()
        .map(|request| request.op().as_str())
        .collect();
    let all: BTreeSet<&str> = Op::ALL.iter().map(|op| op.as_str()).collect();
    assert_eq!(ops, all);
    assert_eq!(Op::ALL.len(), 34);
    for request in requests {
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["op"], request.op().as_str());
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(back, request);
        assert_eq!(Op::from_name(request.op().as_str()), Some(request.op()));
    }
}

#[test]
fn every_unary_method_has_one_reply_with_the_same_name() {
    let replies = replies();
    let ops: BTreeSet<&str> = replies.iter().map(|reply| reply.op().as_str()).collect();
    let unary: BTreeSet<&str> = Op::ALL
        .iter()
        .filter(|op| !op.is_stream())
        .map(|op| op.as_str())
        .collect();
    assert_eq!(ops, unary);
    for reply in replies {
        let value = serde_json::to_value(&reply).unwrap();
        assert_eq!(value["op"], reply.op().as_str());
        let back: Reply = serde_json::from_value(value).unwrap();
        assert_eq!(back, reply);
    }
}

#[test]
fn ports_group_the_methods_and_only_the_three_streams_are_streams() {
    let streams: Vec<Op> = Op::ALL.into_iter().filter(|op| op.is_stream()).collect();
    assert_eq!(streams, [Op::WatchCommand, Op::ShellNotices, Op::McpEvents]);
    for op in Op::ALL {
        let port = op.as_str().split('.').next().unwrap();
        assert_eq!(port, op.capability().as_str(), "{op:?}");
        assert!(
            Capability::PORTS.contains(&op.capability())
                || [Capability::Mcp, Capability::Env].contains(&op.capability())
        );
    }
    let mutations = Op::ALL.into_iter().filter(|op| op.is_mutation()).count();
    assert_eq!(mutations, 15);
}

#[test]
fn the_mcp_pipe_is_its_own_capability() {
    // R5（remote-exec.md §22.4、§22.5）：五种消息都属于 `mcp`；connect、send、close、declare 有副作用，events 是流。
    let mcp: Vec<Op> = Op::ALL
        .into_iter()
        .filter(|op| op.capability() == Capability::Mcp)
        .collect();
    assert_eq!(
        mcp,
        [
            Op::McpConnect,
            Op::McpSend,
            Op::McpEvents,
            Op::McpClose,
            Op::McpDeclare
        ]
    );
    assert!(Op::McpEvents.is_stream() && !Op::McpEvents.is_mutation());
    assert!(
        [Op::McpConnect, Op::McpSend, Op::McpClose, Op::McpDeclare]
            .iter()
            .all(|op| op.is_mutation() && !op.is_stream())
    );
    let both = Capabilities::ports_and_mcp();
    assert!(both.contains(Capability::Mcp) && both.contains(Capability::Files));
    assert_eq!(both.names().count(), 9);
    // 线上的形状：配置是原文（`${VAR}` 没展开），消息是原样的 JSON。
    let connect = requests()
        .into_iter()
        .find(|request| request.op() == Op::McpConnect)
        .unwrap();
    let value = serde_json::to_value(&connect).unwrap();
    assert_eq!(value["args"]["transport"]["type"], "stdio");
    assert_eq!(value["args"]["transport"]["env"]["TOKEN"], "${TOKEN}");
    let event = crate::envelope::StreamItem::Mcp {
        event: crate::dto::mcp::McpEvent::Closed {
            reason: Some("exited".to_owned()),
            session_expired: false,
        },
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({"kind": "mcp", "event": {"type": "closed", "reason": "exited", "session_expired": false}})
    );
}

#[test]
fn capabilities_keep_unknown_names_and_answer_only_known_ones() {
    let parsed: Capabilities =
        serde_json::from_value(json!(["files", "teleport", "commands"])).unwrap();
    assert!(parsed.contains(Capability::Files));
    assert!(parsed.contains(Capability::Commands));
    assert!(!parsed.contains(Capability::Search));
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        json!(["commands", "files", "teleport"])
    );
    assert!(!Capabilities::ports().contains(Capability::Mcp));
    assert_eq!(Capabilities::ports().names().count(), 8);
}

#[test]
fn an_older_hello_without_optional_fields_and_with_unknown_ones_decodes() {
    let hello: Hello = serde_json::from_value(json!({
        "protocol": 1,
        "client_version": "0.1.0",
        "client_instance": "c1",
        "epoch": 1,
        "capabilities": ["files"],
        "trace": {"id": "t1"}
    }))
    .unwrap();
    assert_eq!(hello.protocol, PROTOCOL);
    assert!(hello.credentials.is_none());
    let secret: Hello = serde_json::from_value(json!({
        "protocol": 1, "client_version": "x", "client_instance": "c", "epoch": 2,
        "capabilities": [], "credentials": "token-1"
    }))
    .unwrap();
    assert!(!format!("{secret:?}").contains("token-1"));
    assert_eq!(
        serde_json::to_value(&secret).unwrap()["credentials"],
        "token-1"
    );
}

#[test]
fn an_unknown_refusal_reason_is_still_a_refusal() {
    let refuse: Refuse =
        serde_json::from_value(json!({"code": "quota", "message": "later"})).unwrap();
    assert_eq!(refuse.code, RefuseCode::Other);
}

fn request_frame(header: serde_json::Value) -> Frame {
    let payload = Frame::encode(Kind::Request, &header, &Body::empty()).unwrap();
    match crate::frame::decode(payload).unwrap() {
        crate::frame::Decoded::Frame(frame) => frame,
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unknown_message_is_unsupported_and_keeps_its_call() {
    let frame = request_frame(json!({"call": 9, "request": {"op": "files.teleport", "args": {}}}));
    assert_eq!(
        RequestHeader::parse(&frame),
        Err(BadRequest {
            call: Some(9),
            error: WireError::Unsupported {
                what: "message files.teleport".to_owned()
            }
        })
    );
    let frame = request_frame(json!({"call": 3, "request": {"op": "files.stat", "args": {}}}));
    let error = RequestHeader::parse(&frame).unwrap_err();
    assert_eq!(error.call, Some(3));
    assert!(matches!(error.error, WireError::BadRequest { .. }));
    let frame = request_frame(json!({
        "call": 4,
        "request": {"op": "commands.cancel", "args": {"operation": "op_1"}},
        "resume_after": 2,
        "future": true
    }));
    let header = RequestHeader::parse(&frame).unwrap();
    assert_eq!(header.call, 4);
    assert_eq!(header.resume_after, Some(2));
}

#[test]
fn replies_carry_values_or_wire_errors() {
    let ok = ReplyHeader {
        call: 1,
        result: ReplyResult::Ok(Reply::Stat(None)),
    };
    assert_eq!(
        serde_json::to_value(&ok).unwrap(),
        json!({"call": 1, "result": {"ok": {"op": "files.stat", "value": null}}})
    );
    let err = ReplyHeader {
        call: 2,
        result: ReplyResult::Err(WireError::NotFound {
            path: "/w/a".to_owned(),
        }),
    };
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        json!({"call": 2, "result": {"err": {"kind": "not_found", "path": "/w/a"}}})
    );
    assert_eq!(
        serde_json::to_value(ReplyHeader {
            call: 3,
            result: ReplyResult::Ok(Reply::Cancel)
        })
        .unwrap(),
        json!({"call": 3, "result": {"ok": {"op": "commands.cancel"}}})
    );
}

#[test]
fn blobs_point_into_the_body_and_bad_blobs_are_rejected() {
    let mut body = BodyBuilder::new();
    let first = body.push(b"abc");
    let second = body.push("中文".as_bytes());
    let body = body.finish();
    assert_eq!(body.to_vec(&first).unwrap(), b"abc");
    assert_eq!(body.text(&second).unwrap(), "中文");
    let outside = crate::Blob { at: 4, len: 100 };
    assert!(matches!(
        body.get(&outside),
        Err(WireError::BadRequest { .. })
    ));
    let not_text = BodyBuilder::new();
    let mut not_text = not_text;
    let blob = not_text.push(&[0xff, 0xfe]);
    assert!(not_text.finish().text(&blob).is_err());
}

/// F96（remote-exec.md §23.5）：`env.update` 属于能力位 `env`，不是变更、不是流（幂等，换了实例照常补发）；变量的值可以是
/// null（去掉这个变量）；hello 另有可选的 `session_env`（SSH 的代理加上），老的 hello 没有它。
#[test]
fn the_env_update_is_its_own_capability() {
    let env: Vec<Op> = Op::ALL
        .into_iter()
        .filter(|op| op.capability() == Capability::Env)
        .collect();
    assert_eq!(env, [Op::EnvUpdate]);
    assert!(!Op::EnvUpdate.is_stream() && !Op::EnvUpdate.is_mutation());
    let update = requests()
        .into_iter()
        .find(|request| request.op() == Op::EnvUpdate)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&update).unwrap(),
        json!({"op": "env.update", "args": {
            "mode": "merge",
            "vars": {"DISPLAY": null, "SSH_AUTH_SOCK": "/tmp/ssh-x/agent.1"},
            "injected": ["TOKEN"]
        }})
    );
    let replace: Request = serde_json::from_value(
        json!({"op": "env.update", "args": {"mode": "replace", "vars": {}}}),
    )
    .unwrap();
    assert!(
        matches!(&replace, Request::EnvUpdate { mode: crate::dto::env::EnvMode::Replace, vars, injected } if vars.is_empty() && injected.is_empty())
    );
    let executor = Capabilities::executor();
    assert!(executor.contains(Capability::Env) && executor.contains(Capability::Mcp));
    assert_eq!(executor.names().count(), 10);
    assert!(!Capabilities::ports_and_mcp().contains(Capability::Env));
    let hello = Hello {
        protocol: PROTOCOL,
        client_version: "0.1.0".to_owned(),
        client_instance: "c1".to_owned(),
        epoch: 1,
        capabilities: Capabilities::ports(),
        heartbeat_ms: None,
        credentials: None,
        session_env: Some(std::collections::BTreeMap::from([(
            "SSH_AUTH_SOCK".to_owned(),
            None,
        )])),
    };
    let value = serde_json::to_value(&hello).unwrap();
    assert_eq!(value["session_env"], json!({"SSH_AUTH_SOCK": null}));
    assert_eq!(serde_json::from_value::<Hello>(value).unwrap(), hello);
    let older: Hello = serde_json::from_value(json!({
        "protocol": 1, "client_version": "0.1.0", "client_instance": "c1", "epoch": 1, "capabilities": []
    }))
    .unwrap();
    assert!(older.session_env.is_none());
    assert!(
        serde_json::to_value(&older)
            .unwrap()
            .get("session_env")
            .is_none()
    );
}

/// 修复线 F117（docs/specs/rust-cli/remote-exec.md §18.4）：`background.background_command` 的回复加了可选的 `kind` 与
/// `description`。旧执行器的回复没有它们，解成 Bash 与空；Bash 与空不写出（旧客户端看到的回复不变）；monitor 写出、往返
/// 不变，旧客户端忽略不认识的字段。
#[test]
fn an_older_background_reply_decodes_as_a_bash_command() {
    use crate::dto::command::BackgroundKind;
    use crate::dto::shell::{BackgroundInfo, BackgroundStatus};
    let plain = json!({"command": "sleep 9", "status": "running"});
    let old: ReplyHeader = serde_json::from_value(json!({
        "call": 5,
        "result": {"ok": {"op": "background.background_command", "value": plain}}
    }))
    .unwrap();
    let ReplyResult::Ok(Reply::BackgroundCommand(Some(info))) = &old.result else {
        panic!("{old:?}");
    };
    assert_eq!(
        (info.kind, info.description.as_str()),
        (BackgroundKind::Bash, "")
    );
    assert_eq!(
        serde_json::to_value(&old).unwrap()["result"]["ok"]["value"],
        plain
    );
    let monitor = BackgroundInfo {
        command: "tail -f build.log".to_owned(),
        status: BackgroundStatus::Running,
        kind: BackgroundKind::Monitor,
        description: "build log".to_owned(),
    };
    let value = serde_json::to_value(&monitor).unwrap();
    assert_eq!(
        value,
        json!({
            "command": "tail -f build.log", "status": "running", "kind": "monitor",
            "description": "build log"
        })
    );
    assert_eq!(
        serde_json::from_value::<BackgroundInfo>(value).unwrap(),
        monitor
    );
}
