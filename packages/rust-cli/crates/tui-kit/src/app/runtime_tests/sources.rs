//! 订阅的源、`Cmd::Task`、`Cmd::Handoff`、定时器与帧的合并。移植自 RuntimeSuite.scala 的 cats-effect sources 一节。
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::stream::{BoxStream, Stream, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::{IDLE, QUIT, config, run_to_end};
use crate::app::cmd::Cmd;
use crate::app::element::text;
use crate::app::key::Key;
use crate::app::runtime::RuntimeConfig;
use crate::app::screen::{App, Screen, Viewport};
use crate::app::sub::Sub;
use crate::render::ansi;
use crate::testing::fake_terminal::FakeTerminal;

/// 推送式的源（`TuiHost.subscribe` 的形状），记下订阅与退订：流被丢掉就是退订。
#[derive(Clone, Default)]
struct FakeSource(Arc<Mutex<SourceState>>);

#[derive(Default)]
struct SourceState {
    senders: Vec<UnboundedSender<String>>,
    subscribes: usize,
    unsubscribes: usize,
}

impl FakeSource {
    fn open(&self) -> BoxStream<'static, String> {
        let (tx, rx) = unbounded_channel();
        let mut state = self.0.lock().unwrap();
        state.subscribes += 1;
        state.senders.push(tx);
        Watched {
            rx,
            source: self.clone(),
        }
        .boxed()
    }

    fn emit(&self, v: &str) {
        for tx in &self.0.lock().unwrap().senders {
            let _ = tx.send(v.to_string());
        }
    }

    /// 第一个订阅者的发送端：一个退订之后还在调用旧监听者的源。
    fn leak(&self) -> UnboundedSender<String> {
        self.0.lock().unwrap().senders[0].clone()
    }

    fn subscribes(&self) -> usize {
        self.0.lock().unwrap().subscribes
    }

    fn unsubscribes(&self) -> usize {
        self.0.lock().unwrap().unsubscribes
    }
}

struct Watched {
    rx: UnboundedReceiver<String>,
    source: FakeSource,
}

impl Stream for Watched {
    type Item = String;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<String>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        self.source.0.lock().unwrap().unsubscribes += 1;
    }
}

#[derive(Clone, Debug, PartialEq)]
enum SrcMsg {
    Got(String),
    Drop,
}

/// 显示 key 为 "src" 的源送来的值；`Drop` 去掉这个订阅。`shown` 收到每次画的文字：首帧之后的帧是格子差异，测试从这里
/// 读视图的文字，而不是从写出的字节读。
struct SourceApp {
    src: FakeSource,
    shown: Arc<Mutex<String>>,
}

impl SourceApp {
    fn new(src: &FakeSource) -> (SourceApp, Arc<Mutex<String>>) {
        let shown = Arc::new(Mutex::new(String::new()));
        let app = SourceApp {
            src: src.clone(),
            shown: shown.clone(),
        };
        (app, shown)
    }
}

impl App for SourceApp {
    type State = (Vec<String>, bool);
    type Msg = SrcMsg;

    fn init(&self) -> (Self::State, Cmd<SrcMsg>) {
        ((Vec::new(), true), Cmd::None)
    }

    fn update(&self, msg: SrcMsg, state: &mut Self::State) -> Cmd<SrcMsg> {
        match msg {
            SrcMsg::Got(v) => state.0.push(v),
            SrcMsg::Drop => state.1 = false,
        }
        Cmd::None
    }

    fn subscriptions(&self, state: &Self::State) -> Vec<Sub<SrcMsg>> {
        let mut subs = vec![Sub::on_key_press(|k| {
            (*k == Key::Char('d')).then_some(SrcMsg::Drop)
        })];
        if state.1 {
            // 每次推导都是新的闭包：让订阅跨 update 活着的是 key，不是函数的身份。
            let src = self.src.clone();
            subs.push(Sub::source("src", move || {
                src.open().map(SrcMsg::Got).boxed()
            }));
        }
        subs
    }

    fn view(&self, state: &Self::State, _: Viewport) -> Screen {
        let shown = format!(
            "got[{}]{}",
            state.0.join(","),
            if state.1 { "" } else { " off" }
        );
        *self.shown.lock().unwrap() = shown.clone();
        Screen::new(text(shown))
    }
}

#[tokio::test(start_paused = true)]
async fn on_source_values_become_messages_with_one_subscribe_across_updates_and_unsubscribe_on_teardown()
 {
    let src = FakeSource::default();
    let (app, shown) = SourceApp::new(&src);
    let term = FakeTerminal::new();
    let stage = Arc::new(AtomicUsize::new(0));
    let (at, emit, view) = (stage.clone(), src.clone(), shown.clone());
    term.on_write(move |t, _| {
        let shown = view.lock().unwrap().clone();
        match (at.load(Ordering::SeqCst), shown.as_str()) {
            (0, "got[]") => {
                at.store(1, Ordering::SeqCst);
                emit.emit("a");
            }
            (1, "got[a]") => {
                at.store(2, Ordering::SeqCst);
                emit.emit("b");
            }
            (2, "got[a,b]") => {
                at.store(3, Ordering::SeqCst);
                t.push(QUIT);
            }
            _ => {}
        }
    });
    run_to_end(app, &term, config(5, IDLE)).await.unwrap();
    assert_eq!(
        stage.load(Ordering::SeqCst),
        3,
        "the source's values did not reach the view"
    );
    assert_eq!(
        src.subscribes(),
        1,
        "re-deriving subscriptions must not resubscribe a kept key"
    );
    assert_eq!(
        src.unsubscribes(),
        1,
        "teardown must unsubscribe live sources"
    );
}

#[tokio::test(start_paused = true)]
async fn on_source_dropping_the_key_unsubscribes_and_later_values_are_discarded() {
    let src = FakeSource::default();
    let (app, shown) = SourceApp::new(&src);
    let term = FakeTerminal::new();
    let (emit, view) = (src.clone(), shown.clone());
    let mut stage = 0;
    let mut leaked = None;
    term.on_write(move |t, _| {
        let shown = view.lock().unwrap().clone();
        if stage == 0 && shown == "got[]" {
            stage = 1;
            leaked = Some(emit.leak());
            t.push(b"d");
        } else if stage == 1 && shown.ends_with(" off") {
            // 'd' 的那一帧：订阅没了。过时的投递不能折叠。
            stage = 2;
            if let Some(tx) = &leaked {
                let _ = tx.send("late".into());
            }
            t.push(QUIT);
        }
    });
    let (values, _) = run_to_end(app, &term, config(5, IDLE)).await.unwrap();
    assert_eq!(
        src.unsubscribes(),
        1,
        "dropping the key must unsubscribe exactly once"
    );
    assert!(
        values.is_empty(),
        "a value delivered after unsubscribe was folded: {values:?}"
    );
    assert!(!shown.lock().unwrap().contains("late"));
}

#[derive(Clone, Debug, PartialEq)]
enum TaskMsg {
    Done(Result<i32, String>),
}

/// 两个任务：一个成功、一个失败（错误在 future 里变成消息）；都到了就退出。
struct Tasks;

impl App for Tasks {
    type State = Vec<Result<i32, String>>;
    type Msg = TaskMsg;

    fn init(&self) -> (Self::State, Cmd<TaskMsg>) {
        let ok = Cmd::task(async { TaskMsg::Done(Ok(41 + 1)) });
        let failed = Cmd::task(async { TaskMsg::Done(Err("nope".to_string())) });
        (Vec::new(), Cmd::batch([ok, failed]))
    }

    fn update(&self, TaskMsg::Done(r): TaskMsg, state: &mut Self::State) -> Cmd<TaskMsg> {
        state.push(r);
        if state.len() == 2 {
            Cmd::Quit
        } else {
            Cmd::None
        }
    }

    fn subscriptions(&self, _: &Self::State) -> Vec<Sub<TaskMsg>> {
        Vec::new()
    }

    fn view(&self, state: &Self::State, _: Viewport) -> Screen {
        Screen::new(text(format!("done {}", state.len())))
    }
}

#[tokio::test(start_paused = true)]
async fn cmd_task_delivers_a_result_and_a_failure_as_messages() {
    let got = run_to_end(Tasks, &FakeTerminal::new(), config(5, IDLE))
        .await
        .unwrap();
    assert!(
        got.contains(&Ok(42)),
        "the task result was not delivered: {got:?}"
    );
    assert!(
        got.contains(&Err("nope".to_string())),
        "the task failure was not delivered: {got:?}"
    );
}

#[derive(Clone, Debug, PartialEq)]
enum HandoffMsg {
    Go,
    Tick,
    Back(i32),
}

/// 'e' 交出终端给一个「程序」（记下它运行时画过的帧数）；计时器在程序运行时照常到达：状态在变，但不能画。
struct Handoff {
    term: FakeTerminal,
    paints: Arc<Mutex<(usize, usize)>>,
}

impl App for Handoff {
    /// （tick 的次数，程序回来时给的值）
    type State = (usize, Option<i32>);
    type Msg = HandoffMsg;

    fn init(&self) -> (Self::State, Cmd<HandoffMsg>) {
        ((0, None), Cmd::None)
    }

    fn update(&self, msg: HandoffMsg, state: &mut Self::State) -> Cmd<HandoffMsg> {
        match msg {
            HandoffMsg::Go => {
                let (term, paints) = (self.term.clone(), self.paints.clone());
                Cmd::handoff(async move {
                    let before = term.paints();
                    term.record("program");
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    *paints.lock().unwrap() = (before, term.paints());
                    HandoffMsg::Back(7)
                })
            }
            HandoffMsg::Back(n) => {
                state.1 = Some(n);
                Cmd::None
            }
            HandoffMsg::Tick => {
                state.0 += 1;
                Cmd::None
            }
        }
    }

    fn subscriptions(&self, _: &Self::State) -> Vec<Sub<HandoffMsg>> {
        vec![
            Sub::on_key_press(|k| (*k == Key::Char('e')).then_some(HandoffMsg::Go)),
            Sub::every(5, HandoffMsg::Tick),
        ]
    }

    fn view(&self, state: &Self::State, _: Viewport) -> Screen {
        Screen::new(text(if state.1 == Some(7) {
            "back"
        } else {
            "waiting"
        }))
    }
}

#[tokio::test(start_paused = true)]
async fn cmd_handoff_leaves_the_screen_before_the_program_and_comes_back_whole_with_no_paint_between()
 {
    // 编辑器拿着终端时不能画（§4.2「设置」长文字）；它退出后备用屏重新进入、整屏重画。
    let term = FakeTerminal::new();
    let paints = Arc::new(Mutex::new((usize::MAX, 0)));
    let app = Handoff {
        term: term.clone(),
        paints: paints.clone(),
    };
    term.on_write(|t, s| {
        if s.contains("back") {
            t.push(QUIT);
        }
    });
    term.push(b"e");
    let config = RuntimeConfig {
        enable_mouse: true,
        ..config(5, IDLE)
    };
    run_to_end(app, &term, config).await.unwrap();
    let log = term.lifecycle();
    let at = |step: &str| log.iter().position(|s| s == step).unwrap();
    let last_enable = log.iter().rposition(|s| s == "enable_mouse").unwrap();
    assert!(
        at("disable_mouse") < at("suspend") && at("suspend") < at("program"),
        "{log:?}"
    );
    assert!(
        at("program") < at("resume") && last_enable > at("resume"),
        "{log:?}"
    );
    let (before, after) = *paints.lock().unwrap();
    assert_eq!(
        after, before,
        "a frame was painted while the program had the terminal"
    );
    // 交出时离开了备用屏，回来的第一帧重新进入它。
    let w = term.writes();
    assert!(
        w.find(ansi::ALT_SCREEN_EXIT).unwrap() < w.rfind(ansi::ALT_SCREEN_ENTER).unwrap(),
        "the screen did not come back"
    );
}

/// 丢掉时设标志：任务的 future 被取消的证据。
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// init 起一个永远不结束的任务。
struct Forever(Arc<AtomicBool>);

impl App for Forever {
    type State = ();
    type Msg = TaskMsg;

    fn init(&self) -> ((), Cmd<TaskMsg>) {
        let flag = DropFlag(self.0.clone());
        (
            (),
            Cmd::task(async move {
                let _flag = flag;
                std::future::pending::<TaskMsg>().await
            }),
        )
    }

    fn update(&self, _: TaskMsg, _: &mut ()) -> Cmd<TaskMsg> {
        Cmd::None
    }

    fn subscriptions(&self, _: &()) -> Vec<Sub<TaskMsg>> {
        Vec::new()
    }

    fn view(&self, _: &(), _: Viewport) -> Screen {
        Screen::new(text("x"))
    }
}

#[tokio::test(start_paused = true)]
async fn cmd_task_still_running_at_quit_is_cancelled() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let term = FakeTerminal::new();
    term.push(QUIT);
    run_to_end(Forever(cancelled.clone()), &term, config(5, IDLE))
        .await
        .unwrap();
    assert!(
        cancelled.load(Ordering::SeqCst),
        "the in-flight task was not cancelled on quit"
    );
}

/// 每 2 毫秒一次 +1，到 3 退出。
struct Ticks;

impl App for Ticks {
    type State = usize;
    type Msg = usize;

    fn init(&self) -> (usize, Cmd<usize>) {
        (0, Cmd::None)
    }

    fn update(&self, msg: usize, state: &mut usize) -> Cmd<usize> {
        *state += msg;
        if *state >= 3 { Cmd::Quit } else { Cmd::None }
    }

    fn subscriptions(&self, _: &usize) -> Vec<Sub<usize>> {
        vec![Sub::every(2, 1)]
    }

    fn view(&self, state: &usize, _: Viewport) -> Screen {
        Screen::new(text(format!("t {state}")))
    }
}

#[tokio::test(start_paused = true)]
async fn sub_every_ticks_while_subscribed() {
    let term = FakeTerminal::new();
    run_to_end(Ticks, &term, config(5, IDLE)).await.unwrap();
    assert!(term.closed(), "the timer never drove the app to quit");
}

#[tokio::test(start_paused = true)]
async fn frame_coalescing_paints_a_burst_of_source_messages_within_one_tick_once() {
    const N: usize = 50;
    let src = FakeSource::default();
    let (app, shown) = SourceApp::new(&src);
    let term = FakeTerminal::new();
    let emit = src.clone();
    let mut stage = 0;
    term.on_write(move |t, _| match stage {
        0 => {
            stage = 1;
            for i in 0..N {
                emit.emit(&i.to_string());
            }
        }
        1 => {
            stage = 2;
            t.push(QUIT);
        }
        _ => {}
    });
    run_to_end(app, &term, config(5, IDLE)).await.unwrap();
    assert_eq!(
        term.paints(),
        2,
        "expected the first frame plus exactly one frame for the burst"
    );
    let shown = shown.lock().unwrap();
    assert!(
        shown.ends_with(&format!(",{}]", N - 1)),
        "the coalesced frame must show the whole burst: {shown}"
    );
}

#[tokio::test(start_paused = true)]
async fn frame_coalescing_never_repaints_an_idle_screen() {
    let src = FakeSource::default();
    let (app, _) = SourceApp::new(&src);
    let term = FakeTerminal::new();
    // 没有输入地过去约 20 个帧周期（与约 20 次尺寸轮询），然后退出。
    let script = async {
        tokio::time::sleep(Duration::from_millis(40)).await;
        term.push(QUIT);
    };
    let (result, ()) = tokio::join!(run_to_end(app, &term, config(1, 1)), script);
    result.unwrap();
    assert_eq!(term.paints(), 1, "an idle screen must not repaint");
}
