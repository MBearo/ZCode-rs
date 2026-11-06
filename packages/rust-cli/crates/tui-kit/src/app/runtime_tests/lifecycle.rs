//! 生命周期、按键的路由与 resize。移植自 RuntimeSuite.scala 的 lifecycle 一节。
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rust_cli_base::Panicked;

use super::{Counter, IDLE, QUIT, config, run_to_end};
use crate::app::cmd::Cmd;
use crate::app::element::text;
use crate::app::key::Key;
use crate::app::runtime::RuntimeConfig;
use crate::app::screen::{App, Screen, Viewport};
use crate::app::sub::Sub;
use crate::testing::fake_terminal::FakeTerminal;

fn position(log: &[String], step: &str) -> Option<usize> {
    log.iter().position(|s| s == step)
}

#[tokio::test(start_paused = true)]
async fn mouse_mode_is_enabled_after_raw_mode_and_disabled_before_close() {
    let term = FakeTerminal::new();
    // 启动帧之后立即退出。
    term.push(QUIT);
    let config = RuntimeConfig {
        enable_mouse: true,
        ..config(5, IDLE)
    };
    run_to_end(Counter, &term, config).await.unwrap();
    let order = term.lifecycle();
    let enter = position(&order, "enter_raw").unwrap();
    let enable = position(&order, "enable_mouse");
    let disable = position(&order, "disable_mouse");
    let close = position(&order, "close").unwrap();
    assert!(
        enable.is_some_and(|e| e > enter),
        "enable_mouse must follow enter_raw: {order:?}"
    );
    assert!(
        disable.is_some_and(|d| d < close),
        "disable_mouse must precede close: {order:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn mouse_mode_is_never_enabled_and_so_never_disabled_under_the_default_config() {
    let term = FakeTerminal::new();
    term.push(QUIT);
    run_to_end(Counter, &term, config(5, IDLE)).await.unwrap();
    let order = term.lifecycle();
    assert!(
        !order.contains(&"enable_mouse".to_string()),
        "mouse was enabled without the config"
    );
    // 收尾总会关鼠标，但它由标志保护：从没开过时什么也不记。
    assert!(
        !order.contains(&"disable_mouse".to_string()),
        "mouse was disabled though never enabled"
    );
}

/// 每个键都 panic。
struct Throws;

impl App for Throws {
    type State = ();
    type Msg = ();

    fn init(&self) -> ((), Cmd<()>) {
        ((), Cmd::None)
    }

    fn update(&self, _: (), _: &mut ()) -> Cmd<()> {
        panic!("boom")
    }

    fn subscriptions(&self, _: &()) -> Vec<Sub<()>> {
        vec![Sub::on_key_press(|_| Some(()))]
    }

    fn view(&self, _: &(), _: Viewport) -> Screen {
        Screen::new(text("x"))
    }
}

#[tokio::test(start_paused = true)]
async fn the_terminal_is_restored_even_when_the_app_panics() {
    let term = FakeTerminal::new();
    term.push(b"a");
    let error = run_to_end(Throws, &term, config(5, IDLE))
        .await
        .expect_err("the panic must surface as an error");
    let panicked = error
        .downcast_ref::<Panicked>()
        .expect("the error is the panic");
    assert_eq!(panicked.message, "boom");
    assert!(term.closed(), "terminal was not closed after the failure");
}

/// 记下交给它的每个键（经按键处理器，与 Scala 一样在处理器里记）；视图显示多少个。
struct KeyRecorder {
    seen: Arc<Mutex<Vec<Key>>>,
}

impl KeyRecorder {
    fn new() -> (KeyRecorder, Arc<Mutex<Vec<Key>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (KeyRecorder { seen: seen.clone() }, seen)
    }
}

impl App for KeyRecorder {
    type State = usize;
    type Msg = Key;

    fn init(&self) -> (usize, Cmd<Key>) {
        (0, Cmd::None)
    }

    fn update(&self, _: Key, state: &mut usize) -> Cmd<Key> {
        *state += 1;
        Cmd::None
    }

    fn subscriptions(&self, _: &usize) -> Vec<Sub<Key>> {
        let seen = self.seen.clone();
        vec![Sub::on_key_press(move |k| {
            seen.lock().unwrap().push(k.clone());
            Some(k.clone())
        })]
    }

    fn view(&self, state: &usize, _: Viewport) -> Screen {
        Screen::new(text(format!("keys: {state}")))
    }
}

#[tokio::test(start_paused = true)]
async fn sgr_wheel_bytes_reach_the_apps_key_subscription_as_a_wheel_up() {
    let (app, seen) = KeyRecorder::new();
    let term = FakeTerminal::new();
    // ESC [ < 64 ; 12 ; 5 M → WheelUp(12, 5)
    term.push_str("\x1b[<64;12;5M");
    term.push(QUIT);
    run_to_end(app, &term, config(5, IDLE)).await.unwrap();
    let seen = seen.lock().unwrap();
    assert!(
        seen.contains(&Key::WheelUp { col: 12, row: 5 }),
        "wheel not delivered; saw {seen:?}"
    );
}

/// 键一到达就在下一次写时退出。
fn quit_once_seen(term: &FakeTerminal, seen: &Arc<Mutex<Vec<Key>>>) {
    let seen = seen.clone();
    term.on_write(move |t, _| {
        if !seen.lock().unwrap().is_empty() {
            t.push(QUIT);
        }
    });
}

fn escape_config() -> RuntimeConfig {
    RuntimeConfig {
        escape_ms: 20,
        ..config(5, IDLE)
    }
}

#[tokio::test(start_paused = true)]
async fn a_lone_esc_becomes_esc_once_no_byte_follows_within_the_escape_timeout() {
    let (app, seen) = KeyRecorder::new();
    let term = FakeTerminal::new();
    quit_once_seen(&term, &seen);
    term.push(&[0x1b]);
    run_to_end(app, &term, escape_config()).await.unwrap();
    assert_eq!(*seen.lock().unwrap(), [Key::Esc]);
}

#[tokio::test(start_paused = true)]
async fn esc_and_a_letter_arriving_together_are_alt_letter_never_esc() {
    let (app, seen) = KeyRecorder::new();
    let term = FakeTerminal::new();
    quit_once_seen(&term, &seen);
    term.push(&[0x1b]);
    term.push(b"b");
    run_to_end(app, &term, escape_config()).await.unwrap();
    assert_eq!(*seen.lock().unwrap(), [Key::alt(Key::Char('b'))]);
}

/// 'a' 的消息是按键处理器拿到的视口宽度。
struct WidthOnA;

impl App for WidthOnA {
    type State = Vec<usize>;
    type Msg = usize;

    fn init(&self) -> (Vec<usize>, Cmd<usize>) {
        (Vec::new(), Cmd::None)
    }

    fn update(&self, msg: usize, state: &mut Vec<usize>) -> Cmd<usize> {
        state.push(msg);
        Cmd::None
    }

    fn subscriptions(&self, _: &Vec<usize>) -> Vec<Sub<usize>> {
        vec![Sub::on_key(|k, vp| {
            (*k == Key::Char('a')).then_some(vp.width)
        })]
    }

    fn view(&self, state: &Vec<usize>, _: Viewport) -> Screen {
        Screen::new(text(format!("n: {}", state.len())))
    }
}

#[tokio::test(start_paused = true)]
async fn a_key_handler_can_read_the_viewport_the_next_frame_is_drawn_at() {
    let term = FakeTerminal::new();
    term.set_size(57, 10);
    term.push(b"a");
    term.push(QUIT);
    let widths = run_to_end(WidthOnA, &term, config(5, IDLE)).await.unwrap();
    assert_eq!(widths, [57]);
}

/// 记下最后一次 `view` 的行数。
struct RowsSeen {
    rows: Arc<AtomicUsize>,
}

impl App for RowsSeen {
    type State = ();
    type Msg = ();

    fn init(&self) -> ((), Cmd<()>) {
        ((), Cmd::None)
    }

    fn update(&self, _: (), _: &mut ()) -> Cmd<()> {
        Cmd::None
    }

    fn subscriptions(&self, _: &()) -> Vec<Sub<()>> {
        vec![Sub::on_key_press(|_| None)]
    }

    fn view(&self, _: &(), viewport: Viewport) -> Screen {
        self.rows.store(viewport.rows, Ordering::SeqCst);
        Screen::new(text("x"))
    }
}

#[tokio::test(start_paused = true)]
async fn a_push_style_resize_repaints_at_the_new_dims_with_no_key_or_msg() {
    let term = FakeTerminal::new();
    let rows = Arc::new(AtomicUsize::new(0));
    let stage = Arc::new(AtomicUsize::new(0));
    let (seen_rows, at) = (rows.clone(), stage.clone());
    term.on_write(move |t, _| {
        if at.load(Ordering::SeqCst) == 0 {
            // 启动帧上模拟 OS 的 resize 并发信号——之后没有按键也没有消息，只有信号能让它重画。
            at.store(1, Ordering::SeqCst);
            t.set_size(30, 8);
            t.signal_resize();
        } else if at.load(Ordering::SeqCst) == 1 && seen_rows.load(Ordering::SeqCst) == 8 {
            // 新尺寸的帧到了：退出。
            at.store(2, Ordering::SeqCst);
            t.push(QUIT);
        }
    });
    // 轮询与帧节拍实际上都关掉了：重画只能来自信号。
    let app = RowsSeen { rows: rows.clone() };
    run_to_end(app, &term, config(IDLE, IDLE)).await.unwrap();
    assert_eq!(
        stage.load(Ordering::SeqCst),
        2,
        "the resize did not drive an immediate repaint"
    );
    assert_eq!(
        rows.load(Ordering::SeqCst),
        8,
        "the repaint did not use the new terminal rows"
    );
}
