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
    assert_eq!(Op::ALL.len(), 30);
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
fn ports_group_the_methods_and_only_the_two_streams_are_streams() {
    let streams: Vec<Op> = Op::ALL.into_iter().filter(|op| op.is_stream()).collect();
    assert_eq!(streams, [Op::WatchCommand, Op::ShellNotices]);
    for op in Op::ALL {
        let port = op.as_str().split('.').next().unwrap();
        assert_eq!(port, op.capability().as_str(), "{op:?}");
        assert!(Capability::PORTS.contains(&op.capability()));
    }
    let mutations = Op::ALL.into_iter().filter(|op| op.is_mutation()).count();
    assert_eq!(mutations, 11);
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
