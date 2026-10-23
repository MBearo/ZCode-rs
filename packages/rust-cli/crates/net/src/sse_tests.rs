// 前两个测试复制自 apps/zcode-cli-rust/crates/model/src/sse.rs（feat/rs 79b6691c55）。
use super::*;

fn data(events: Vec<SseEvent>) -> Vec<String> {
    events.into_iter().map(|event| event.data).collect()
}

#[test]
fn unicode_multiline_crlf_and_large_multi_event_chunk() {
    let mut decoder = SseDecoder::new();
    let mut events = vec![];
    for byte in "data: 你好\r\ndata: world\r\n\r\n".as_bytes() {
        events.extend(decoder.push(&[*byte]).unwrap());
    }
    assert_eq!(data(events), vec!["你好\nworld"]);
    let frame = "data: {}\n\n";
    let events = decoder.push(frame.repeat(40_000).as_bytes()).unwrap();
    assert_eq!(events.len(), 40_000);
}

/// 行与事件不限长：一次写 300 KB 文件的工具调用参数就在一条行里。
#[test]
fn long_lines_and_events_decode_across_packets() {
    let mut decoder = SseDecoder::new();
    let line = format!("data: {}\n\n", "a".repeat(1024 * 1024));
    let mut events = vec![];
    for packet in line.as_bytes().chunks(16 * 1024) {
        events.extend(decoder.push(packet).unwrap());
    }
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data.len(), 1024 * 1024);
    let mut decoder = SseDecoder::new();
    for _ in 0..5 {
        let line = format!("data: {}\n", "x".repeat(60_000));
        assert!(decoder.push(line.as_bytes()).unwrap().is_empty());
    }
    let events = decoder.push(b"\n").unwrap();
    assert_eq!(events[0].data.len(), 5 * 60_000 + 4);
    assert_eq!(SseDecoder::new().push(b"data: \xff\n\n"), Err(SseError));
}

#[test]
fn event_names_comments_and_ignored_fields() {
    let stream = concat!(
        ": keep-alive comment\n",
        "event: message_start\n",
        "id: 7\n",
        "retry: 1000\n",
        "data: {\"type\":\"message_start\"}\n",
        "\n",
        "event: ping\n",
        "\n",
        "data:no-space\n",
        "\n",
    );
    let events = SseDecoder::new().push(stream.as_bytes()).unwrap();
    assert_eq!(
        events,
        vec![
            SseEvent {
                event: Some("message_start".to_owned()),
                data: "{\"type\":\"message_start\"}".to_owned(),
            },
            // `event: ping` 没有 data 行，不分发；它的名字也不会带到下一个事件上。
            SseEvent {
                event: None,
                data: "no-space".to_owned(),
            },
        ]
    );
}

/// 任意切分（包括切在 `\r` 与 `\n` 之间、切在多字节字符中间）都得到同样的事件。
#[test]
fn every_split_point_gives_the_same_events() {
    let stream = "event: a\r\ndata: 你好\r\n\r\nevent: b\ndata: x\ndata: y\n\n".as_bytes();
    let expected = SseDecoder::new().push(stream).unwrap();
    assert_eq!(expected.len(), 2);
    for split in 0..=stream.len() {
        let mut decoder = SseDecoder::new();
        let mut events = decoder.push(&stream[..split]).unwrap();
        events.extend(decoder.push(&stream[split..]).unwrap());
        assert_eq!(events, expected, "split at {split}");
    }
}

#[test]
fn an_unfinished_event_is_not_dispatched() {
    let mut decoder = SseDecoder::new();
    assert!(decoder.push(b"data: partial\n").unwrap().is_empty());
    assert!(decoder.push(b"data: more").unwrap().is_empty());
}
