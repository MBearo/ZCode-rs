//! Windows 的按键入口（tui-input.md §9，修复线 F65）：paste burst 接进界面、AltGr 与 alt 码。平台注入成 Windows，
//! 时间拨动（不等真实时间）。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rust_cli_kernel_api::session_api::Delivery;

use super::super::app::{Action, App, Overlay};
use super::super::composer::InputMode;
use crate::os::{Os, WindowsRelease};

const WINDOWS: Os = Os::Windows(Some(WindowsRelease {
    major: 10,
    build: 22631,
}));

fn app(os: Os) -> App {
    let mut app = crate::ui::test_support::app();
    app.set_os(os);
    app.composer.skills_loaded = true;
    app
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// 按键脚本：每个键间隔 `gap`（Windows 的粘贴是一串很快的按键）；`\n` 是 enter。返回动作与最后一个键的时刻。
fn feed(app: &mut App, text: &str, from: Instant, gap: Duration) -> (Vec<Action>, Instant) {
    let mut actions = Vec::new();
    let mut at = from;
    for (i, c) in text.chars().enumerate() {
        at = from + gap * u32::try_from(i).unwrap();
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        actions.extend(app.on_terminal_key(press(code), at));
    }
    (actions, at)
}

/// 照 runner 的计时：到了冲刷时刻就冲刷，直到没有扣住的字符与缓存。
fn settle(app: &mut App) -> Vec<Action> {
    let mut actions = Vec::new();
    while let Some(at) = app.burst_deadline() {
        actions.extend(app.flush_burst(at));
    }
    actions
}

fn submitted(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Submit { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// T1：Windows 上多行粘贴是一串很快的字符键与 enter。合并成一次粘贴：换行不发送，比五行高时折成帽（Puffin「粘贴」）；之后的
/// enter 发送展开后的全文。
#[test]
fn a_fast_multiline_burst_is_one_paste_that_does_not_submit() {
    let mut app = app(WINDOWS);
    let t0 = Instant::now();
    let (actions, last) = feed(&mut app, "1\n2\n3\n4\n5\n6", t0, ms(1));
    assert!(actions.is_empty(), "{actions:?}");
    // 交出之前什么都不画（扣住、缓存）。
    assert_eq!(app.composer.area.text(), "");
    assert_eq!(app.burst_deadline(), Some(last + ms(61)));
    assert!(settle(&mut app).is_empty());
    assert_eq!(app.composer.area.text(), "[Pasted · 6 lines]");
    let send = app.on_terminal_key(press(KeyCode::Enter), last + ms(300));
    assert_eq!(
        send,
        [Action::Submit {
            text: "1\n2\n3\n4\n5\n6".to_owned(),
            delivery: Delivery::Queue,
        }]
    );
}

/// 平常的打字（间隔大于 8 ms）：每个字符扣住不到 9 ms 后照常键入；enter 发送。
#[test]
fn slow_typing_still_types_and_enter_submits() {
    let mut app = app(WINDOWS);
    let t0 = Instant::now();
    let mut at = t0;
    for c in "hi?".chars() {
        assert!(app.on_terminal_key(press(KeyCode::Char(c)), at).is_empty());
        settle(&mut app);
        at += ms(80);
    }
    assert_eq!(app.composer.area.text(), "hi?");
    assert_eq!(
        submitted(&app.on_terminal_key(press(KeyCode::Enter), at)),
        ["hi?"]
    );
}

/// 逐键才有的行为不发生：行首的 `?` 不打开快捷键面板；`!` 开头时与括号粘贴相同地切到 bash 模式（tui-input.md §2.1）。
#[test]
fn a_burst_does_not_trigger_per_key_shortcuts() {
    let mut app = app(WINDOWS);
    let t0 = Instant::now();
    feed(&mut app, "?why", t0, ms(1));
    settle(&mut app);
    assert!(!matches!(app.overlay, Some(Overlay::Shortcuts)));
    assert_eq!(app.composer.area.text(), "?why");
    let mut bash = self::app(WINDOWS);
    feed(&mut bash, "!ls -la", t0, ms(1));
    settle(&mut bash);
    assert_eq!(bash.composer.mode, InputMode::Bash);
    assert_eq!(bash.composer.area.text(), "ls -la");
    // 逐个键入的 `?` 照旧打开面板。
    let mut typed = self::app(WINDOWS);
    typed.on_terminal_key(press(KeyCode::Char('?')), t0);
    settle(&mut typed);
    assert!(matches!(typed.overlay, Some(Overlay::Shortcuts)));
}

/// 拖进来的图片路径（Windows 的终端把路径当作按键打出来）：与括号粘贴相同，交给 runner 读文件。
#[test]
fn a_dropped_image_path_becomes_an_image_paste() {
    let mut app = app(WINDOWS);
    let (actions, _) = feed(&mut app, "C:\\Users\\dev\\shot.png", Instant::now(), ms(1));
    assert!(actions.is_empty());
    assert_eq!(
        settle(&mut app),
        [Action::PasteImagePath {
            path: "C:\\Users\\dev\\shot.png".to_owned(),
            text: "C:\\Users\\dev\\shot.png".to_owned(),
        }]
    );
}

/// 粘贴的斜杠命令之后很快按 enter：照常执行命令，不当作换行。
#[test]
fn enter_right_after_a_pasted_slash_command_runs_it() {
    let mut app = app(WINDOWS);
    let (_, last) = feed(&mut app, "/clear", Instant::now(), ms(1));
    let flushed = last + ms(61);
    app.flush_burst(flushed);
    assert_eq!(app.composer.area.text(), "/clear");
    let actions = app.on_terminal_key(press(KeyCode::Enter), flushed + ms(5));
    assert!(actions.contains(&Action::Clear), "{actions:?}");
}

/// 中文的多行粘贴：非 ASCII 字符立即键入；第一行短（不像粘贴，收不回）时，紧跟着的 enter 仍是换行，之后整段收回缓存。
#[test]
fn a_chinese_multiline_paste_does_not_submit_its_first_line() {
    let mut app = app(WINDOWS);
    let (actions, _) = feed(&mut app, "第一行\n第二行内容", Instant::now(), ms(1));
    assert!(submitted(&actions).is_empty(), "{actions:?}");
    settle(&mut app);
    assert_eq!(app.composer.area.text(), "第一行\n第二行内容");
    // 输入法逐字上屏（间隔大于 8 ms）：照常键入，enter 发送。
    let mut ime = self::app(WINDOWS);
    let t0 = Instant::now();
    for (i, c) in "你好".chars().enumerate() {
        ime.on_terminal_key(
            press(KeyCode::Char(c)),
            t0 + ms(50) * u32::try_from(i).unwrap(),
        );
    }
    assert_eq!(ime.composer.area.text(), "你好");
    assert_eq!(
        submitted(&ime.on_terminal_key(press(KeyCode::Enter), t0 + ms(200))),
        ["你好"]
    );
}

/// 别的键（这里是 ←）先交出缓存再照常处理。
#[test]
fn another_key_flushes_the_burst_first() {
    let mut app = app(WINDOWS);
    let (_, last) = feed(&mut app, "abc", Instant::now(), ms(1));
    app.on_terminal_key(press(KeyCode::Left), last + ms(2));
    assert_eq!(app.composer.area.text(), "abc");
    assert_eq!(app.composer.area.cursor(), 2);
    assert_eq!(app.burst_deadline(), None);
}

/// T2：AltGr 的字符（Ctrl+Alt）进输入框，也能进 burst；Windows 上它不触发 ctrl 快捷键（空输入框里 Ctrl+Alt+d 是
/// 键入 `d`，不是退出的第一次 ctrl+d）。
#[test]
fn altgr_characters_are_typed_and_join_bursts() {
    let mut app = app(WINDOWS);
    let altgr = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL | KeyModifiers::ALT);
    let t0 = Instant::now();
    assert!(app.on_terminal_key(altgr('d'), t0).is_empty());
    settle(&mut app);
    assert_eq!(app.composer.area.text(), "d");
    assert!(app.notice.is_none());
    app.composer.area.clear();
    for (i, c) in "a{b}@".chars().enumerate() {
        let key = if c.is_ascii_alphabetic() {
            press(KeyCode::Char(c))
        } else {
            altgr(c)
        };
        app.on_terminal_key(key, t0 + ms(100) + ms(1) * u32::try_from(i).unwrap());
    }
    settle(&mut app);
    assert_eq!(app.composer.area.text(), "a{b}@");
    // 其他平台照旧：空输入框里 Ctrl+Alt+d 是 ctrl+d（提示再按一次退出），不插入。
    let mut unix = self::app(Os::Unix);
    unix.on_terminal_key(altgr('d'), t0);
    assert_eq!(unix.composer.area.text(), "");
    assert!(unix.notice.is_some());
}

/// T3：alt+小键盘码的字符只在 Alt 的松开事件里（没有配对的按下）：进输入框；配对的松开不重复。
#[test]
fn alt_codes_are_typed_and_paired_releases_are_not() {
    let mut app = app(WINDOWS);
    let t0 = Instant::now();
    let up =
        |c| KeyEvent::new_with_kind(KeyCode::Char(c), KeyModifiers::NONE, KeyEventKind::Release);
    app.on_terminal_key(press(KeyCode::Char('x')), t0);
    app.on_terminal_key(up('x'), t0 + ms(30));
    settle(&mut app);
    app.on_terminal_key(up('é'), t0 + ms(100));
    settle(&mut app);
    assert_eq!(app.composer.area.text(), "xé");
}

/// 其他平台不变：没有 burst，enter 立即发送；Ctrl+Alt 加字符不是文字；松开事件丢掉。
#[test]
fn other_platforms_keep_the_old_behavior() {
    let mut app = app(Os::Unix);
    let (actions, last) = feed(&mut app, "one\ntwo", Instant::now(), ms(1));
    assert_eq!(submitted(&actions), ["one"]);
    assert_eq!(app.composer.area.text(), "two");
    assert_eq!(app.burst_deadline(), None);
    let altgr = KeyEvent::new(
        KeyCode::Char('@'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    app.on_terminal_key(altgr, last + ms(10));
    let up = KeyEvent::new_with_kind(
        KeyCode::Char('é'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    );
    app.on_terminal_key(up, last + ms(20));
    assert_eq!(app.composer.area.text(), "two");
}

/// 架构整理 U9（tui-input.md §9.1）：Windows 的 AltGr（crossterm 报成 Ctrl+Alt 加字符）在入口归一成普通字符，之后
/// 每个面板都只看普通修饰键。修前产品路径上看不出差别（paste burst 把扣住的 ASCII 字符去掉修饰键再交出），但不经
/// burst 的入口（`on_key`）与 burst 原样放行的键（非 ASCII 字符）带着 Ctrl+Alt 进面板：直接读 `CONTROL` 的地方
/// 把它当 ctrl（Ctrl+Alt+c 关掉打开着的页面），读 `Typing` 的地方当文字。
#[test]
fn ctrl_alt_characters_are_text_in_every_panel_on_windows() {
    let altgr = KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    // 本机统计的页（`/usage all`，usage_local.rs）：ctrl+c 关上它（以前用的独立页面 `Overlay::Page` 已删）。
    let open = |app: &mut App| {
        app.open_usage(
            rust_cli_kernel_api::metering::SessionMetering::default(),
            Err("none".into()),
        );
    };
    let mut app = app(WINDOWS);
    open(&mut app);
    app.on_key(altgr, Instant::now());
    assert!(
        matches!(app.overlay, Some(Overlay::Usage(_))),
        "AltGr+c is a character, not ctrl+c"
    );
    // 经 paste burst 的入口结果相同。
    app.on_terminal_key(altgr, Instant::now());
    settle(&mut app);
    assert!(matches!(app.overlay, Some(Overlay::Usage(_))));
    // 真的 ctrl+c 照旧关闭。
    app.on_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    assert!(app.overlay.is_none());
    // 其他平台：Ctrl+Alt+c 仍是 ctrl 组合，照旧关闭。
    let mut unix = self::app(Os::Unix);
    open(&mut unix);
    unix.on_key(altgr, Instant::now());
    assert!(unix.overlay.is_none());
}
