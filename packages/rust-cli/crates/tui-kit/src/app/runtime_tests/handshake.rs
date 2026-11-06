//! 启动握手（docs/puffin-tui.md §6 规则 3：从不扣住首帧）。移植自 RuntimeSuite.scala 的 startup handshake 一节。
//!
//! 采纳的类宽度是进程级的：这些测试拿全 crate 共用的写锁（[`Adopted::exclusive`]），结束时还原缺省。
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::{Counter, IDLE, QUIT, config, run_to_end};
use crate::app::cmd::Cmd;
use crate::app::element::{Wrap, memo, text, wrap_text};
use crate::app::handshake::{TerminalProbe, WidthProbe};
use crate::app::layout::Layout;
use crate::app::runtime::RuntimeConfig;
use crate::app::screen::{App, Screen, Viewport};
use crate::app::sub::Sub;
use crate::render::ansi;
use crate::render::width;
use crate::testing::fake_terminal::FakeTerminal;
use crate::testing::width_lock::Adopted;

/// ✅→1、😀→1、①→1、一→2：一个可信的、用旧 wcwidth 的终端的探测回答。
const PROBE_REPLIES: &str = "\x1b[1;2R\x1b[1;2R\x1b[1;2R\x1b[1;3R";

/// 缺省宽度的回答（✅→2、😀→2、①→1、一→2）、背景色，然后 DA1。
const DEFAULT_WIDTH_REPLIES: &str =
    "\x1b[1;3R\x1b[1;3R\x1b[1;2R\x1b[1;3R\x1b]11;rgb:1717/1717/1717\x07\x1b[?62;c";

fn probe_config(
    handshake: Box<dyn crate::app::handshake::Handshake>,
    frame_ms: u64,
    timeout: u64,
) -> RuntimeConfig {
    RuntimeConfig {
        handshake: Some(handshake),
        handshake_timeout_ms: timeout,
        ..config(frame_ms, IDLE)
    }
}

#[derive(Default)]
struct Stages {
    stage: usize,
    first: String,
    settle: String,
}

#[tokio::test(start_paused = true)]
async fn the_first_frame_paints_before_the_probe_is_answered_and_the_answer_repaints_the_screen() {
    let _widths = Adopted::exclusive();
    let term = FakeTerminal::new();
    let stages = Arc::new(Mutex::new(Stages::default()));
    let seen = stages.clone();
    term.on_write(move |t, s| {
        let mut st = seen.lock().unwrap();
        match st.stage {
            0 if s.contains("count: 0") => {
                // 终端还没回答，超时实际上是无限的：到了这一帧就证明画没有等。
                st.stage = 1;
                st.first = s.to_string();
                // 与终端回答赛跑的按键要作为输入到达应用。
                t.push(b"g");
                t.push_str(PROBE_REPLIES);
            }
            1 if s.contains("count: 1") => {
                st.stage = 2;
                st.settle = s.to_string();
                t.push(QUIT);
            }
            _ => {}
        }
    });
    let config = probe_config(Box::new(WidthProbe::new()), 5, IDLE);
    let count = run_to_end(Counter, &term, config).await.unwrap();
    let st = stages.lock().unwrap();
    assert_eq!(st.stage, 2, "expected first frame → settle repaint → quit");
    let log = term.writes();
    let probe = log.find("6n").expect("the probe went out");
    assert!(
        probe < log.find("count: 0").unwrap(),
        "the probe must go out before the first frame"
    );
    // 首帧在备用屏里、关着自动换行：终端画得比假定宽的字形被裁掉，而不是挤动行。
    assert!(
        st.first
            .contains(&format!("{}{}", ansi::ALT_SCREEN_ENTER, ansi::WRAP_OFF)),
        "the provisional frame must run with autowrap off"
    );
    // 落定按采纳的宽度清屏、整屏重画。
    assert!(
        st.settle.contains(ansi::CLEAR_SCREEN),
        "the settle must repaint the screen from scratch"
    );
    assert_eq!(count, 1, "the keystroke racing the reply was lost");
    assert_eq!(
        width::display_width('\u{2705}'),
        1,
        "the probe's widths were not adopted"
    );
}

/// 显示探测报告了什么，像入口那样经一个源送到：探测的 `on_settled` 设这个值。
#[derive(Clone, Default)]
struct Probed {
    listener: Arc<Mutex<Option<UnboundedSender<String>>>>,
}

impl Probed {
    fn report(&self, v: &str) {
        if let Some(tx) = self.listener.lock().unwrap().as_ref() {
            let _ = tx.send(v.to_string());
        }
    }

    fn probe(&self) -> Box<TerminalProbe> {
        let me = self.clone();
        Box::new(TerminalProbe::new(move |r| {
            me.report(if r.bg.is_some() {
                "ANSWERED"
            } else {
                "TIMEDOUT"
            })
        }))
    }
}

impl App for Probed {
    type State = String;
    type Msg = String;

    fn init(&self) -> (String, Cmd<String>) {
        ("none".into(), Cmd::None)
    }

    fn update(&self, msg: String, state: &mut String) -> Cmd<String> {
        *state = msg;
        Cmd::None
    }

    fn subscriptions(&self, _: &String) -> Vec<Sub<String>> {
        let listener = self.listener.clone();
        vec![Sub::source(
            "probe",
            move || -> BoxStream<'static, String> {
                let (tx, mut rx) = unbounded_channel();
                *listener.lock().unwrap() = Some(tx);
                stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed()
            },
        )]
    }

    fn view(&self, state: &String, _: Viewport) -> Screen {
        Screen::new(text(format!("probe: {state}")))
    }
}

/// 画过的帧，不含探测请求（它也包在同步输出里）。
fn frames_painted(term: &FakeTerminal) -> Vec<String> {
    term.frames()
        .into_iter()
        .filter(|f| f.contains(ansi::SYNC_BEGIN) && !f.contains("6n"))
        .collect()
}

/// 首帧之后推 `replies`，看到 `done` 就退出。
fn answer_then_quit(term: &FakeTerminal, replies: &'static str, done: &'static str) {
    let mut answered = false;
    term.on_write(move |t, s| {
        if s.contains("probe: none") && !answered {
            answered = true;
            t.push_str(replies);
        } else if s.contains(done) {
            t.push(QUIT);
        }
    });
}

#[tokio::test(start_paused = true)]
async fn the_frame_after_the_settle_already_shows_what_the_settle_delivered() {
    // 修复前：握手完成时立即按旧状态整屏重画，探测结果的消息排在后面，下一批才生效，屏幕先重画一遍旧的首帧，再画
    // 新的。现在先处理结果再画。
    let _widths = Adopted::exclusive();
    let app = Probed::default();
    let term = FakeTerminal::new();
    let replies = "\x1b[1;2R\x1b[1;2R\x1b[1;2R\x1b[1;3R\x1b]11;rgb:1717/1717/1717\x07\x1b[?62;c";
    answer_then_quit(&term, replies, "ANSWERED");
    let config = probe_config(app.probe(), IDLE, IDLE);
    run_to_end(app, &term, config).await.unwrap();
    let paints = frames_painted(&term);
    assert_eq!(
        paints.len(),
        2,
        "the first frame, then one frame with the answer: {paints:?}"
    );
    assert!(paints[1].contains("ANSWERED"), "{:?}", paints[1]);
    assert!(
        paints[1].contains(ansi::CLEAR_SCREEN),
        "the widths changed, so the screen is repainted from scratch"
    );
}

#[tokio::test(start_paused = true)]
async fn when_the_widths_do_not_change_the_settle_paints_only_the_cells_that_changed() {
    let _widths = Adopted::exclusive();
    let app = Probed::default();
    let term = FakeTerminal::new();
    answer_then_quit(&term, DEFAULT_WIDTH_REPLIES, "ANSWERED");
    let config = probe_config(app.probe(), IDLE, IDLE);
    run_to_end(app, &term, config).await.unwrap();
    let paints = frames_painted(&term);
    assert_eq!(paints.len(), 2, "{paints:?}");
    assert!(paints[1].contains("ANSWERED"), "{:?}", paints[1]);
    assert!(
        !paints[1].contains(ansi::CLEAR_SCREEN),
        "nothing the widths decide changed: no full repaint"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unanswered_probe_times_out_keeps_the_defaults_and_the_app_still_gets_its_report() {
    let _widths = Adopted::exclusive();
    let app = Probed::default();
    let term = FakeTerminal::new();
    term.on_write(|t, s| {
        if s.contains("TIMEDOUT") {
            t.push(QUIT);
        }
    });
    let config = probe_config(app.probe(), IDLE, 30);
    run_to_end(app, &term, config).await.unwrap();
    let paints = frames_painted(&term);
    assert_eq!(paints.len(), 2, "the first frame, then the report");
    assert!(
        !paints[1].contains(ansi::CLEAR_SCREEN),
        "default widths kept: no full repaint"
    );
    assert_eq!(
        width::display_width('\u{2705}'),
        2,
        "a timed-out probe must keep the defaults"
    );
}

#[test]
fn layout_memos_filled_before_the_settle_are_relaid_at_the_adopted_widths() {
    // 四个 ✅ 在 5 列里折行：缺省（宽）的 emoji 宽度下一行两个，终端报告它们窄之后一行四个。只按宽度做键的记忆会一直
    // 重放两行的布局。
    let _widths = Adopted::exclusive();
    let node = || wrap_text("", "", "\u{2705}\u{2705}\u{2705}\u{2705}", Wrap::Char);
    let memoized = memo(node());
    let plain = |lines: Vec<crate::render::StyledLine>| -> Vec<String> {
        lines.iter().map(crate::render::StyledLine::plain).collect()
    };
    let before = plain(Layout::lay(&memoized, 5));
    width::adopt(1, 2, 1);
    let fresh = plain(Layout::lay(&node(), 5));
    assert_ne!(
        fresh, before,
        "the fixture must lay differently at the two widths"
    );
    assert_eq!(
        plain(Layout::lay(&memoized, 5)),
        fresh,
        "the memo replayed lines laid at the old widths"
    );
}
