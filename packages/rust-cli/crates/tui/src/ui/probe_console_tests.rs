//! Windows 的前景 / 背景色探测（tui.md §7.4 的 T13，工作线 F71）：拼回复、判断完整与停止，注入输入记录。

use std::collections::VecDeque;
use std::time::Duration;

use super::*;

/// 注入的输入队列：每次 `read` 交出一批；没有批次时 `wait` 当作到了期限。
#[derive(Default)]
struct Fake {
    batches: VecDeque<Vec<Record>>,
    waits: usize,
}

impl Fake {
    fn new(batches: Vec<Vec<Record>>) -> Self {
        Self {
            batches: batches.into(),
            waits: 0,
        }
    }
}

impl ConsoleInput for Fake {
    fn wait(&mut self, _timeout: Duration) -> bool {
        self.waits += 1;
        !self.batches.is_empty()
    }

    fn read(&mut self) -> Vec<Record> {
        self.batches.pop_front().unwrap_or_default()
    }
}

/// 终端的回复按字符变成按下事件（控制台给每个字符一个按下、一个松开）。
fn typed(text: &str) -> Vec<Record> {
    text.encode_utf16()
        .flat_map(|unit| [Record::KeyDown(unit), Record::Other])
        .collect()
}

const FOREGROUND: &str = "\x1b]10;rgb:cdcd/d6d6/f4f4\x1b\\";
const BACKGROUND: &str = "\x1b]11;rgb:ffff/ffff/ffff\x07";

#[test]
fn the_query_asks_for_both_colors_without_a_sentinel() {
    assert_eq!(QUERY, b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\");
}

/// 两段回复都完整就结束，不等期限；回复可以拆在几批里，中间夹着松开、焦点之类的记录。
#[test]
fn both_replies_end_the_probe_before_the_deadline() {
    let mut first = typed(FOREGROUND);
    first.push(Record::Other);
    let (head, tail) = BACKGROUND.split_at(9);
    let mut input = Fake::new(vec![first, typed(head), typed(tail), typed("later")]);
    let reply = collect(&mut input, Duration::from_secs(60));
    assert_eq!(
        parse(&reply),
        Probed {
            foreground: Some((205, 214, 244)),
            background: Some((255, 255, 255)),
        }
    );
    // 第四批（之后的输入）没有读。
    assert_eq!(input.waits, 3);
    assert_eq!(input.batches.len(), 1);
}

/// 不回答的终端（旧的 conhost）：到期限时退回缺省（前景与背景都不知道）。
#[test]
fn a_terminal_that_does_not_answer_keeps_the_defaults() {
    let mut input = Fake::new(Vec::new());
    let reply = collect(&mut input, Duration::from_millis(250));
    assert_eq!(parse(&reply), Probed::default());
}

/// 期限到时只用已经完整的那段：只回了背景，或者前景的回复还没收完。
#[test]
fn only_complete_replies_count() {
    let mut input = Fake::new(vec![typed(BACKGROUND), typed("\x1b]10;rgb:cdcd/d6")]);
    let reply = collect(&mut input, Duration::from_secs(60));
    assert_eq!(
        parse(&reply),
        Probed {
            foreground: None,
            background: Some((255, 255, 255)),
        }
    );
}

/// 探测期间敲的字与非 ASCII 字符（输入法、中文）不进回复，也不打断它。
#[test]
fn typed_keys_around_the_replies_are_dropped() {
    let mut keys = typed("a中");
    keys.push(Record::KeyDown(0));
    keys.extend(typed(BACKGROUND));
    keys.extend(typed("é"));
    keys.extend(typed(FOREGROUND));
    let mut input = Fake::new(vec![keys]);
    let reply = collect(&mut input, Duration::from_secs(60));
    assert!(!reply.contains('中') && !reply.contains('é'), "{reply:?}");
    assert_eq!(
        parse(&reply),
        Probed {
            foreground: Some((205, 214, 244)),
            background: Some((255, 255, 255)),
        }
    );
}

/// 一直有输入（大段粘贴）时收够上限就停，不等期限。
#[test]
fn a_flood_of_input_stops_at_the_limit() {
    let flood: Vec<Vec<Record>> = (0..100).map(|_| typed(&"x".repeat(64))).collect();
    let mut input = Fake::new(flood);
    let reply = collect(&mut input, Duration::from_secs(60));
    assert!(reply.len() >= MAX_REPLY, "{}", reply.len());
    assert!(input.batches.len() > 50, "{}", input.batches.len());
    assert_eq!(parse(&reply), Probed::default());
}
