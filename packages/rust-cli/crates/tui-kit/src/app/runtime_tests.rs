//! 运行时的测试。移植自 packages/tui/tui/test/src/zcode/tui/app/RuntimeSuite.scala（feat/tui a2bf878b34）。
//!
//! 输入经 [`FakeTerminal`] 注入：运行时取走输入流之前推的字节缓冲着。每个测试把运行时跑到结束，超过 5 秒（虚拟时间）
//! 算卡住，失败而不是挂起。测试在 `on_write` 里推进（看到某一帧就推下一步），因为每个看得见的步骤都以一次写结束。
//!
//! 时间是 tokio 的暂停时钟（`start_paused`）：空闲时直接跳到下一个计时器，测试不等真实的时间。所以「永远不触发」的
//! 帧间隔与轮询间隔（[`IDLE`]）比 5 秒的上限长：它们没触发，测试就证明了重画来自别的路径。
//!
//! 没有移植的 Scala 测试：「a rejected JS promise is delivered as the JS error's own message」——JS 的 Promise 与
//! `Runtime.describe` 在 Rust 里没有对应：`Cmd::Task` 的 future 自己把错误变成消息。
mod handshake;
mod lifecycle;
mod sources;

use std::time::Duration;

use rust_cli_base::TaskScope;

use super::cmd::Cmd;
use super::element::text;
use super::key::Key;
use super::runtime::{RuntimeConfig, run};
use super::screen::{App, Screen, Viewport};
use super::sub::Sub;
use crate::render::ansi;
use crate::render::renderer::Graphic;
use crate::testing::fake_terminal::FakeTerminal;

/// 测试里永远不触发的帧间隔、轮询间隔（毫秒）：长于 [`LIMIT`]。
const IDLE: u64 = 10_000_000;

/// 跑一个运行时最多多久（虚拟时间）。
const LIMIT: Duration = Duration::from_secs(5);

/// 退出键 ^Q 的字节。
const QUIT: &[u8] = &[0x11];

fn config(frame_ms: u64, width_poll_ms: u64) -> RuntimeConfig {
    RuntimeConfig {
        frame_ms,
        width_poll_ms,
        ..RuntimeConfig::default()
    }
}

/// 把运行时跑到结束；超过 [`LIMIT`] 时失败（不挂起）。
async fn run_to_end<A>(
    app: A,
    term: &FakeTerminal,
    config: RuntimeConfig,
) -> anyhow::Result<A::State>
where
    A: App,
    A::Msg: Clone + PartialEq,
{
    let scope = TaskScope::root("runtime-test", |_| {});
    tokio::time::timeout(
        LIMIT,
        run(app, Box::new(term.clone()), config, scope.spawner()),
    )
    .await
    .expect("the runtime did not finish (wedged)")
}

/// 每个键折进 `+1`；视图显示 `count: N`。
struct Counter;

impl App for Counter {
    type State = usize;
    type Msg = usize;

    fn init(&self) -> (usize, Cmd<usize>) {
        (0, Cmd::None)
    }

    fn update(&self, msg: usize, state: &mut usize) -> Cmd<usize> {
        *state += msg;
        Cmd::None
    }

    fn subscriptions(&self, _: &usize) -> Vec<Sub<usize>> {
        vec![Sub::on_key_press(|k| {
            matches!(k, Key::Char(_)).then_some(1)
        })]
    }

    fn view(&self, state: &usize, _: Viewport) -> Screen {
        Screen::new(text(format!("count: {state}")))
    }
}

#[tokio::test(start_paused = true)]
async fn runtime_renders_processes_a_key_and_quits_cleanly_on_the_quit_key() {
    let term = FakeTerminal::new();
    // 打 'a'（处理成 +1），再 Ctrl-Q（0x11）退出。
    term.push(b"a");
    term.push(QUIT);
    let seen = run_to_end(Counter, &term, config(5, 50)).await.unwrap();
    assert!(term.raw_entered(), "raw mode was not entered");
    assert!(term.closed(), "terminal was not closed on teardown");
    assert!(term.writes().contains("count:"), "no frame was rendered");
    assert_eq!(seen, 1, "the typed key was not folded before quit");
}

/// 'x' 让 update 请求退出。
struct QuitOnX;

impl App for QuitOnX {
    type State = ();
    type Msg = ();

    fn init(&self) -> ((), Cmd<()>) {
        ((), Cmd::None)
    }

    fn update(&self, _: (), _: &mut ()) -> Cmd<()> {
        Cmd::Quit
    }

    fn subscriptions(&self, _: &()) -> Vec<Sub<()>> {
        vec![Sub::on_key_press(|k| (*k == Key::Char('x')).then_some(()))]
    }

    fn view(&self, _: &(), _: Viewport) -> Screen {
        Screen::new(text("running"))
    }
}

#[tokio::test(start_paused = true)]
async fn runtime_exits_when_an_app_command_requests_quit() {
    let term = FakeTerminal::new();
    term.push(b"x");
    run_to_end(QuitOnX, &term, config(5, 50)).await.unwrap();
    assert!(term.closed(), "terminal was not closed on teardown");
}

/// 第二帧画出来就退出。
fn quit_on_second_paint(term: &FakeTerminal) {
    term.on_write(|t, _| {
        if t.paints() >= 2 {
            t.push(QUIT);
        }
    });
}

#[tokio::test(start_paused = true)]
async fn a_keystroke_repaints_immediately_without_waiting_for_a_frame_tick() {
    let term = FakeTerminal::new();
    // 帧节拍实际上关掉了（IDLE），启动帧之后唯一可能的帧就是按键自己立即画的。
    quit_on_second_paint(&term);
    term.push(b"a");
    let seen = run_to_end(Counter, &term, config(IDLE, IDLE))
        .await
        .unwrap();
    assert_eq!(seen, 1, "the key was not folded");
    assert_eq!(
        term.paints(),
        2,
        "the keystroke did not repaint immediately (no second frame)"
    );
}

#[tokio::test(start_paused = true)]
async fn a_burst_of_buffered_keys_coalesces_into_a_single_repaint() {
    let term = FakeTerminal::new();
    quit_on_second_paint(&term);
    // 三个字一起缓冲着：循环第一次收集之前都已就绪，在同一批里折叠。
    term.push(b"a");
    term.push(b"b");
    term.push(b"c");
    let seen = run_to_end(Counter, &term, config(IDLE, IDLE))
        .await
        .unwrap();
    assert_eq!(seen, 3, "all three keys should fold before the paint");
    assert_eq!(term.paints(), 2, "expected startup + one coalesced frame");
}

// ---- 备用屏（docs/puffin-tui.md §4.4） ----

/// 显示计数；`exit_text` 道别；`graphics` 带一张图。
struct ScreenApp {
    exit: String,
    pic: Option<Graphic>,
}

impl ScreenApp {
    fn new() -> ScreenApp {
        ScreenApp {
            exit: String::new(),
            pic: None,
        }
    }
}

impl App for ScreenApp {
    type State = usize;
    type Msg = usize;

    fn init(&self) -> (usize, Cmd<usize>) {
        (0, Cmd::None)
    }

    fn update(&self, msg: usize, state: &mut usize) -> Cmd<usize> {
        *state += msg;
        Cmd::None
    }

    fn subscriptions(&self, _: &usize) -> Vec<Sub<usize>> {
        vec![Sub::on_key_press(|k| {
            matches!(k, Key::Char(_)).then_some(1)
        })]
    }

    fn view(&self, state: &usize, _: Viewport) -> Screen {
        Screen {
            graphics: self.pic.iter().cloned().collect(),
            ..Screen::new(text(format!("count: {state}")))
        }
    }

    fn exit_text(&self, _: &usize) -> String {
        self.exit.clone()
    }
}

/// 一进入备用屏就退出。
fn quit_on_alt_enter(term: &FakeTerminal) {
    term.on_write(|t, s| {
        if s.contains(ansi::ALT_SCREEN_ENTER) {
            t.push(QUIT);
        }
    });
}

#[tokio::test(start_paused = true)]
async fn the_first_frame_enters_the_alt_buffer_and_quitting_leaves_it() {
    let term = FakeTerminal::new();
    quit_on_alt_enter(&term);
    run_to_end(ScreenApp::new(), &term, config(5, IDLE))
        .await
        .unwrap();
    let log = term.writes();
    let painted = log.find("count: 0").unwrap();
    assert!(
        log.find(ansi::ALT_SCREEN_ENTER).unwrap() < painted,
        "the first frame is painted in the alt buffer"
    );
    assert!(
        log.rfind(ansi::ALT_SCREEN_EXIT).unwrap() > painted,
        "teardown leaves the alt buffer"
    );
    assert!(term.closed());
}

#[tokio::test(start_paused = true)]
async fn the_exit_text_is_written_after_leaving_the_alt_buffer_before_the_terminal_closes() {
    let term = FakeTerminal::new();
    quit_on_alt_enter(&term);
    let app = ScreenApp {
        exit: "Session: x\r\n".into(),
        pic: None,
    };
    run_to_end(app, &term, config(5, IDLE)).await.unwrap();
    let log = term.writes();
    assert!(
        log.find("Session: x").unwrap() > log.rfind(ansi::ALT_SCREEN_EXIT).unwrap(),
        "{log:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_frames_graphics_reach_the_terminal_at_their_cell() {
    let term = FakeTerminal::new();
    term.on_write(|t, s| {
        if s.contains("<pic>") {
            t.push(QUIT);
        }
    });
    let pic = Graphic {
        id: 1,
        row: 2,
        col: 3,
        rows: 1,
        cols: 2,
        draw: "<pic>".into(),
        erase: String::new(),
    };
    let app = ScreenApp {
        exit: String::new(),
        pic: Some(pic),
    };
    run_to_end(app, &term, config(5, IDLE)).await.unwrap();
    assert!(term.writes().contains(&format!("{}3;4H<pic>", ansi::CSI)));
}

#[tokio::test(start_paused = true)]
async fn a_resize_with_no_key_or_msg_events_repaints_at_the_new_size() {
    let term = FakeTerminal::new();
    let repainted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = repainted.clone();
    let mut entered = false;
    term.on_write(move |t, s| {
        if !entered && s.contains(ansi::ALT_SCREEN_ENTER) {
            entered = true;
            // 之后没有按键也没有消息。
            t.set_size(30, 8);
        } else if entered
            && !flag.load(std::sync::atomic::Ordering::SeqCst)
            && s.contains(ansi::CLEAR_SCREEN)
        {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            t.push(QUIT);
        }
    });
    run_to_end(ScreenApp::new(), &term, config(5, 5))
        .await
        .unwrap();
    assert!(
        repainted.load(std::sync::atomic::Ordering::SeqCst),
        "a resize with no input did not repaint at the new size"
    );
}

#[tokio::test(start_paused = true)]
async fn kitty_pushes_the_keyboard_enhancement_after_entering_the_alt_buffer_and_pops_it_before_leaving()
 {
    // kitty 按屏幕缓冲记键盘模式，主屏的 push 带不过来：没有备用屏的 push，Shift+Enter 会成为裸的 CR。
    let term = FakeTerminal::kitty();
    quit_on_alt_enter(&term);
    run_to_end(ScreenApp::new(), &term, config(5, IDLE))
        .await
        .unwrap();
    let log = term.writes();
    assert!(log.contains(&format!(
        "{}{}",
        ansi::ALT_SCREEN_ENTER,
        ansi::PUSH_KEYBOARD_ENHANCEMENT
    )));
    assert!(log.contains(&format!(
        "{}{}",
        ansi::POP_KEYBOARD_ENHANCEMENT,
        ansi::ALT_SCREEN_EXIT
    )));
}

#[tokio::test(start_paused = true)]
async fn non_kitty_has_no_keyboard_push_or_pop_around_the_alt_buffer() {
    let term = FakeTerminal::new();
    quit_on_alt_enter(&term);
    run_to_end(ScreenApp::new(), &term, config(5, IDLE))
        .await
        .unwrap();
    let log = term.writes();
    assert!(
        !log.contains(ansi::PUSH_KEYBOARD_ENHANCEMENT)
            && !log.contains(ansi::POP_KEYBOARD_ENHANCEMENT)
    );
    assert!(
        log.contains(&format!(
            "{}{}{}",
            ansi::ALT_SCREEN_ENTER,
            ansi::WRAP_OFF,
            ansi::RESET
        )),
        "entry carries only wrap-off + reset"
    );
}
