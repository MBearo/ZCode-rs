//! 帧：编解码、分片与拼回、上限、不认识的类型；1 版的帧（存成十六进制）新代码必须能解开。

use bytes::Bytes;
use serde_json::json;

use crate::envelope::{Ack, EventHeader, RequestHeader, StreamAck, StreamItem};
use crate::frame::{Decoded, Frame, FrameError, Kind, Reassembler, Received, Splitter, decode};
use crate::handshake::{Capabilities, Hello};
use crate::request::Request;
use crate::{Body, BodyBuilder};

fn frame(payload: Bytes) -> Frame {
    match decode(payload).unwrap() {
        Decoded::Frame(frame) => frame,
        other => panic!("{other:?}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Bytes {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
        .collect::<Vec<u8>>()
        .into()
}

#[test]
fn a_frame_round_trips_with_its_body() {
    let mut body = BodyBuilder::new();
    let blob = body.push(b"\x00\x01binary");
    let body = body.finish();
    let header = RequestHeader {
        call: 7,
        request: Request::ReadFile {
            workspace: crate::test_support::workspace(),
            path: "/w/a".to_owned(),
        },
        resume_after: None,
    };
    let payload = Frame::encode(Kind::Request, &header, &body).unwrap();
    assert_eq!(payload[0], 0x10);
    let decoded = frame(payload);
    assert_eq!(decoded.kind, Kind::Request);
    assert_eq!(decoded.header::<RequestHeader>().unwrap(), header);
    assert_eq!(decoded.body.to_vec(&blob).unwrap(), b"\x00\x01binary");
}

#[test]
fn every_kind_has_a_distinct_code_and_control_frames_are_marked() {
    let kinds = [
        Kind::Hello,
        Kind::Welcome,
        Kind::Refuse,
        Kind::Request,
        Kind::Reply,
        Kind::Event,
        Kind::Ack,
        Kind::Close,
        Kind::Ping,
        Kind::Pong,
        Kind::Admin,
        Kind::AdminReply,
    ];
    for kind in kinds {
        assert_eq!(Kind::from_code(kind.code()), Some(kind));
    }
    let control: Vec<Kind> = kinds.into_iter().filter(|kind| kind.is_control()).collect();
    assert_eq!(control, [Kind::Ack, Kind::Close, Kind::Ping, Kind::Pong]);
}

/// 每种帧的类型字节（remote-exec.md §18.2 的表）：线上的值不能变，编码与解码都按它。
#[test]
fn every_kind_keeps_its_wire_code() {
    let table = [
        (Kind::Hello, 0x01),
        (Kind::Welcome, 0x02),
        (Kind::Refuse, 0x03),
        (Kind::Request, 0x10),
        (Kind::Reply, 0x11),
        (Kind::Event, 0x12),
        (Kind::Ack, 0x13),
        (Kind::Close, 0x14),
        (Kind::Ping, 0x20),
        (Kind::Pong, 0x21),
        (Kind::Admin, 0x40),
        (Kind::AdminReply, 0x41),
    ];
    for (kind, code) in table {
        assert_eq!(kind.code(), code, "{kind:?}");
        assert_eq!(Kind::from_code(code), Some(kind));
        let payload = Frame::encode(kind, &json!({}), &Body::empty()).unwrap();
        assert_eq!(payload[0], code, "{kind:?}");
        assert_eq!(frame(payload).kind, kind);
    }
    // 片的类型字节不是帧的类型；不在表里的字节解不出类型。
    for code in [0x00, 0x04, 0x30, 0x42, 0xff] {
        assert_eq!(Kind::from_code(code), None, "{code:#x}");
    }
}

#[test]
fn unknown_kinds_are_reported_and_truncated_frames_fail() {
    assert_eq!(
        decode(Bytes::from_static(&[0x7e, 0, 0])),
        Ok(Decoded::Unknown(0x7e))
    );
    assert_eq!(decode(Bytes::new()), Err(FrameError::Truncated));
    assert_eq!(
        decode(Bytes::from_static(&[0x10, 0, 0, 0, 9, b'{'])),
        Err(FrameError::Truncated)
    );
    assert_eq!(
        decode(Bytes::from_static(&[0x30, 0, 0])),
        Err(FrameError::Truncated)
    );
}

#[test]
fn large_messages_are_split_and_put_back_together() {
    let mut body = BodyBuilder::new();
    let data: Vec<u8> = (0..700_000u32).map(|index| (index % 251) as u8).collect();
    let blob = body.push(&data);
    let payload = Frame::encode(Kind::Reply, &json!({"call": 1}), &body.finish()).unwrap();
    let mut splitter = Splitter::new(256 * 1024);
    let parts = splitter.split(payload.clone());
    assert_eq!(parts.len(), 3);
    assert!(parts.iter().all(|part| part[0] == 0x30));
    let mut reassembler = Reassembler::default();
    assert_eq!(reassembler.push(parts[0].clone()), Ok(Received::Pending));
    // 两片之间插进来的控制帧照常解出。
    let ping = Frame::encode(Kind::Ping, &json!({"nonce": 1}), &Body::empty()).unwrap();
    assert!(matches!(reassembler.push(ping), Ok(Received::Frame(f)) if f.kind == Kind::Ping));
    assert_eq!(reassembler.push(parts[1].clone()), Ok(Received::Pending));
    let Ok(Received::Frame(whole)) = reassembler.push(parts[2].clone()) else {
        panic!("not reassembled");
    };
    assert_eq!(whole.kind, Kind::Reply);
    assert_eq!(whole.body.to_vec(&blob).unwrap(), data);
    // 小消息不拆；下一条大消息换一个消息号。
    let small = Frame::encode(Kind::Ack, &Ack::default(), &Body::empty()).unwrap();
    assert_eq!(splitter.split(small.clone()), vec![small]);
    let next = splitter.split(payload);
    assert_ne!(next[0][1..9], parts[0][1..9]);
}

#[test]
fn a_message_over_the_limit_is_refused_on_both_sides() {
    let mut reassembler = Reassembler::new(10);
    let payload = Frame::encode(Kind::Ack, &json!({"replies": [1, 2, 3]}), &Body::empty()).unwrap();
    let parts = Splitter::new(4).split(payload);
    assert_eq!(reassembler.push(parts[0].clone()), Ok(Received::Pending));
    assert_eq!(reassembler.push(parts[1].clone()), Ok(Received::Pending));
    assert!(matches!(
        reassembler.push(parts[2].clone()),
        Err(FrameError::TooLarge { max: 10, .. })
    ));
}

/// 1 版的帧（由 1 版的编码器生成后存下）：以后的版本必须能解开，结果不变。
const HELLO_V1: &str = "010000006d7b2270726f746f636f6c223a312c22636c69656e745f76657273696f6e223a22302e312e30222c22636c69656e\
745f696e7374616e6365223a22633031222c2265706f6368223a312c226361706162696c6974696573223a5b22636f6d6d61\
6e6473222c2266696c6573225d7d";
const EVENT_V1: &str = "120000007c7b2263616c6c223a352c22736571223a322c226974656d223a7b226b696e64223a22636f6d6d616e64222c2265\
76656e74223a7b2274797065223a226f7574707574222c2273747265616d223a227374646f7574222c2264617461223a7b22\
6174223a302c226c656e223a337d2c22637572736f72223a31307d7d7d686921";

#[test]
fn version_one_frames_still_decode() {
    let hello = frame(unhex(HELLO_V1));
    assert_eq!(hello.kind, Kind::Hello);
    let hello: Hello = hello.header().unwrap();
    assert_eq!(hello.client_instance, "c01");
    assert_eq!(
        hello.capabilities,
        Capabilities::of([
            crate::handshake::Capability::Files,
            crate::handshake::Capability::Commands
        ])
    );
    let event = frame(unhex(EVENT_V1));
    let header: EventHeader = event.header().unwrap();
    assert_eq!((header.call, header.seq), (5, 2));
    let StreamItem::Command {
        event: crate::dto::command::CommandEvent::Output { data, cursor, .. },
    } = header.item
    else {
        panic!("not output");
    };
    assert_eq!(cursor, 10);
    assert_eq!(event.body.to_vec(&data).unwrap(), b"hi!");
    // 现在的编码器写出同样的字节（格式没有无意中变化）。
    let again = Frame::encode(
        Kind::Hello,
        &Hello {
            protocol: 1,
            client_version: "0.1.0".to_owned(),
            client_instance: "c01".to_owned(),
            epoch: 1,
            capabilities: Capabilities::of([
                crate::handshake::Capability::Files,
                crate::handshake::Capability::Commands,
            ]),
            heartbeat_ms: None,
            credentials: None,
            session_env: None,
        },
        &Body::empty(),
    )
    .unwrap();
    assert_eq!(hex(&again), HELLO_V1);
    let _ = StreamAck { call: 1, seq: 1 };
}

/// 管理帧（remote-exec.md §19.6）：往返；不认识的动作与结果解成 `Other`；老的 welcome（没有 `ledger`）照常解开。
#[test]
fn admin_frames_round_trip_and_tolerate_new_names() {
    use crate::admin::{AdminAction, AdminOutcome, AdminReply, AdminRequest, ExecutorStatus};
    let request = AdminRequest {
        action: AdminAction::StopIfIdle,
    };
    let payload = Frame::encode(Kind::Admin, &request, &Body::empty()).unwrap();
    assert_eq!(payload[0], 0x40);
    let decoded = frame(payload);
    assert_eq!(decoded.header::<AdminRequest>().unwrap(), request);
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"action": "stop_if_idle"})
    );
    let reply = AdminReply {
        outcome: AdminOutcome::Busy,
        status: ExecutorStatus {
            protocol: 1,
            version: "0.1.0".to_owned(),
            instance: "i1".to_owned(),
            pid: 42,
            executable: Some("/home/u/zcode-rs".to_owned()),
            uptime_ms: 5,
            clients: 1,
            calls: 0,
            streams: 2,
            commands: 1,
            notices: 0,
            idle: false,
        },
    };
    let decoded = frame(Frame::encode(Kind::AdminReply, &reply, &Body::empty()).unwrap());
    assert_eq!(decoded.kind, Kind::AdminReply);
    assert_eq!(decoded.header::<AdminReply>().unwrap(), reply);
    let later: AdminRequest = serde_json::from_value(json!({"action": "drain"})).unwrap();
    assert_eq!(later.action, AdminAction::Other);
    let mut text = serde_json::to_value(&reply).unwrap();
    text["outcome"] = json!("restarting");
    text["status"]["later_field"] = json!(true);
    let later: AdminReply = serde_json::from_value(text).unwrap();
    assert_eq!(later.outcome, AdminOutcome::Other);
    let older: crate::handshake::Welcome = serde_json::from_value(json!({
        "protocol": 1, "server_version": "0.1.0", "server_instance": "s", "os": "linux", "arch": "x86_64",
        "path_style": "posix", "capabilities": [], "max_message": 1
    }))
    .unwrap();
    assert_eq!(older.ledger, None);
}
