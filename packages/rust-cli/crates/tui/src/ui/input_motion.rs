//! 输入框里淡入的东西（tui-kit.md §4.2「`!` bash 模式」「粘贴」）：`!` 从 brand 淡到 code（Puffin `shell_view::mark` 的
//! `shell_at`）、粘贴的帽从板上淡入（Puffin `pastes::cap` 的 `born`）、帽的预览一起淡入（Puffin `peek_at`）。
//!
//! 什么时候开始淡：画的时候第一次见到它的那一刻（与槽的淡入同一个办法，`pane::faded`）。runner 在输入之后马上画，所以与
//! Puffin 在输入的那个 tick 记下的是同一刻。数据只有草稿与画的时刻两个来源，不另开计时器；淡入期间 `App::animating` 为真，
//! 按动画节拍重画到落定。
use std::collections::HashMap;
use std::time::Instant;

use rust_cli_tui_kit::app::widgets::fade::TICKS;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;

use super::app::App;
use super::composer::InputMode;

/// 各样东西第一次画出来的时刻与这一帧的时刻。
#[derive(Debug, Default)]
pub struct Motion {
    /// 这一帧画的时刻（`note`）；没画过时为 None，年龄当作早已落定。
    now: Option<Instant>,
    /// 进 bash 模式之后第一次画的时刻；离开时清掉。
    shell: Option<Instant>,
    /// 每个粘贴的帽（占位文字）第一次画的时刻；帽不在草稿里时去掉。
    caps: HashMap<String, Instant>,
    /// 开着的预览是哪一个帽的、从什么时候开着。
    peek: Option<(String, Instant)>,
}

impl Motion {
    /// 从 `at` 到这一帧走过的 tick 数（kit 的 `fade` 按 tick 算）。
    fn age(&self, at: Option<Instant>) -> i64 {
        match (self.now, at) {
            (Some(now), Some(at)) => {
                let ms = now.saturating_duration_since(at).as_millis();
                i64::try_from(ms / u128::from(TICK_MS)).unwrap_or(i64::MAX)
            }
            _ => i64::MAX / 4,
        }
    }

    pub fn shell_age(&self) -> i64 {
        self.age(self.shell)
    }

    pub fn cap_age(&self, placeholder: &str) -> i64 {
        self.age(self.caps.get(placeholder).copied())
    }

    pub fn peek_age(&self) -> i64 {
        self.age(self.peek.as_ref().map(|(_, at)| *at))
    }

    /// 还有东西在淡入（按动画节拍重画）。
    pub fn moving(&self) -> bool {
        let fading = |at: &Instant| {
            at.elapsed().as_millis() < u128::from(TICK_MS) * u128::try_from(TICKS).unwrap_or(0)
        };
        self.shell.as_ref().is_some_and(fading)
            || self.caps.values().any(fading)
            || self.peek.as_ref().is_some_and(|(_, at)| fading(at))
    }
}

/// 画输入框之前：记下这一帧的时刻，与第一次见到的 `!`、帽、预览（pane 在排输入框之前调）；状态栏的时钟同时记下
/// （status_motion.rs）。
pub fn note(app: &mut App, now: Instant, now_ms: u64) {
    super::status_motion::note(app, now, now_ms);
    let bash = app.composer.mode == InputMode::Bash;
    let text = app.composer.area.text().to_owned();
    let present: Vec<String> = app
        .composer
        .paste_placeholders()
        .filter(|p| text.contains(*p))
        .map(str::to_owned)
        .collect();
    let peeking = super::input_box::peek_open(app)
        .then(|| app.composer.paste_under_cursor().map(|(p, _)| p.to_owned()))
        .flatten();
    let m = &mut app.bottom_motion;
    m.now = Some(now);
    if bash {
        m.shell.get_or_insert(now);
    } else {
        m.shell = None;
    }
    m.caps.retain(|p, _| present.contains(p));
    for p in present {
        m.caps.entry(p).or_insert(now);
    }
    m.peek = match (peeking, m.peek.take()) {
        (Some(p), Some((was, at))) if was == p => Some((was, at)),
        (Some(p), _) => Some((p, now)),
        (None, _) => None,
    };
}
