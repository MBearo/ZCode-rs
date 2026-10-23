//! 终端按键的入口（docs/specs/rust-cli/tui-input.md §9，修复线 F65）：松开事件与 alt 码（`Releases`）、Windows 上
//! 没有括号粘贴时的 paste burst（`PasteBurst`），之后交给 [`App::dispatch`] 或 [`App::on_paste`]。缓存的文字作为一次
//! 粘贴，走与括号粘贴相同的路径。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{Action, App};
use super::composer::{InputMode, Popup};
use super::keyboard::{Releases, Typing};
use super::paste_burst::{CharDecision, FlushResult, PasteBurst, retro_start_index};
use crate::os::Os;

impl App {
    /// 入口注入的平台（runner 启动时）：文字输入的判断、松开事件；Windows 上打开 paste burst。
    pub fn set_os(&mut self, os: Os) {
        self.typing = Typing::new(os);
        self.releases = Releases::new(os);
        self.burst = os.is_windows().then(PasteBurst::default);
    }

    /// runner 的按键入口。返回要执行的动作：先交出缓存的粘贴、再处理这个键时有两个。
    pub fn on_terminal_key(&mut self, key: KeyEvent, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(key) = self.releases.admit(key) else {
            return actions;
        };
        if self.burst.is_none() {
            actions.extend(self.dispatch(key, now));
            return actions;
        }
        self.flush_due(now, &mut actions);
        self.burst_key(key, now, &mut actions);
        actions
    }

    /// 下一次该调 [`Self::flush_burst`] 的时刻；没有扣住的字符与缓存时为 None。
    pub fn burst_deadline(&self) -> Option<Instant> {
        self.burst.as_ref().and_then(PasteBurst::deadline)
    }

    /// 到了冲刷时刻（runner 的计时）：缓存作为一次粘贴交出，扣住的字符照常键入。
    pub fn flush_burst(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        self.flush_due(now, &mut actions);
        actions
    }

    /// 鼠标、粘贴等别的输入之前：立即交出缓存，下一个字符不再算进这一段。
    pub fn flush_burst_before_input(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        self.flush_now(&mut actions);
        if let Some(burst) = self.burst.as_mut() {
            burst.clear_window_after_non_char();
        }
        actions
    }

    /// 焦点变化（`Event::FocusGained` / `FocusLost`）。
    pub fn focus_changed(&mut self) {
        self.releases.focus_changed();
    }

    fn flush_due(&mut self, now: Instant, actions: &mut Vec<Action>) {
        let Some(burst) = self.burst.as_mut() else {
            return;
        };
        match burst.flush_if_due(now) {
            FlushResult::Paste(text) => actions.extend(self.on_paste(&text)),
            FlushResult::Typed(c) => {
                let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
                actions.extend(self.dispatch(key, now));
            }
            FlushResult::None => {}
        }
    }

    fn flush_now(&mut self, actions: &mut Vec<Action>) {
        if let Some(text) = self
            .burst
            .as_mut()
            .and_then(PasteBurst::flush_before_modified_input)
        {
            actions.extend(self.on_paste(&text));
        }
    }

    fn burst_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<Action>) {
        let (ctrl, alt) = (self.typing.ctrl(&key), self.typing.alt(&key));
        match key.code {
            KeyCode::Enter if !ctrl && !alt => {
                // 输入框第一行以 `/` 开头、斜杠列表打开时 enter 照常执行命令。
                let slash = self.slash_context();
                let Some(burst) = self.burst.as_mut() else {
                    return;
                };
                if !slash && burst.append_control_char_if_active('\n', now) {
                    return;
                }
                // 刚交出的 burst 之后、或上一个字符键 8 ms 内的 enter：换行（非 ASCII 的字符立即键入，见 tui-input.md §9.2）。
                if !slash && burst.direct_insert_newline_should_insert(now) {
                    burst.extend_window(now);
                    actions.extend(self.on_paste("\n"));
                    return;
                }
                self.flush_now(actions);
                actions.extend(self.dispatch(key, now));
            }
            KeyCode::Tab if key.modifiers.is_empty() => {
                if let Some(burst) = self.burst.as_mut()
                    && burst.append_control_char_if_active('\t', now)
                {
                    return;
                }
                self.other_key(key, now, actions);
            }
            KeyCode::Char(c) if self.typing.is_plain_text(&key) => {
                if !self.burst_char(c, now, actions) {
                    self.flush_now(actions);
                    actions.extend(self.dispatch(key, now));
                }
            }
            _ => self.other_key(key, now, actions),
        }
    }

    /// 不进判定的键：先交出缓存，再照常处理，清掉连续计数（enter 不走这里：多行粘贴里的空行不打断 burst）。
    fn other_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<Action>) {
        self.flush_now(actions);
        actions.extend(self.dispatch(key, now));
        if let Some(burst) = self.burst.as_mut() {
            burst.clear_window_after_non_char();
        }
    }

    /// 一个文字键进判定；返回 true 表示进了缓存或被扣住（不照常键入）。
    fn burst_char(&mut self, c: char, now: Instant, actions: &mut Vec<Action>) -> bool {
        let Some(burst) = self.burst.as_mut() else {
            return false;
        };
        let decision = if c.is_ascii() {
            Some(burst.on_plain_char(c, now))
        } else {
            // 非 ASCII（输入法）：不扣住，立即键入；已经在缓存时进缓存。
            if burst.try_append_char_if_active(c, now) {
                return true;
            }
            self.flush_now(actions);
            let Some(burst) = self.burst.as_mut() else {
                return false;
            };
            burst.on_plain_char_no_hold(now)
        };
        let begun = match decision {
            Some(CharDecision::RetainFirstChar) => return true,
            Some(CharDecision::BufferAppend | CharDecision::BeginBufferFromPending) => true,
            Some(CharDecision::BeginBuffer { retro_chars }) => self.retro_grab(retro_chars, now),
            None => false,
        };
        if begun && let Some(burst) = self.burst.as_mut() {
            burst.append_char_to_buffer(c, now);
        }
        begun
    }

    /// 把输入框里光标前已经键入、像粘贴的那一段收回缓存。只收输入框里的，不跨占位文字。
    fn retro_grab(&mut self, retro_chars: u16, now: Instant) -> bool {
        if !self.composer_takes_keys() || self.composer.area.selection().is_some() {
            return false;
        }
        let area = &self.composer.area;
        let cursor = area.cursor();
        let before = &area.text()[..cursor];
        let start = retro_start_index(before, usize::from(retro_chars));
        let chips = super::textarea::chips::chips(area.text());
        if chips.iter().any(|&(from, to)| from < cursor && to > start) {
            return false;
        }
        let Some(burst) = self.burst.as_mut() else {
            return false;
        };
        let Some(grab) = burst.decide_begin_buffer(now, before, usize::from(retro_chars)) else {
            return false;
        };
        self.composer
            .area
            .replace_range(grab.start_byte, cursor, "");
        self.composer.refresh_popup();
        true
    }

    /// 按键会到输入框（与 [`App::dispatch`] 的分派相同的条件）。
    fn composer_takes_keys(&self) -> bool {
        self.overlay.is_none()
            && self.view.pending().is_none()
            && self.view.peer_held().is_none()
            && self.focus.is_none()
            && self.viewing.is_none()
            && self.composer.search.is_none()
    }

    /// enter 要照常执行斜杠命令：输入框第一行以 `/` 开头，或斜杠列表打开。
    fn slash_context(&self) -> bool {
        self.composer_takes_keys()
            && self.composer.mode == InputMode::Prompt
            && (matches!(self.composer.popup, Some(Popup::Slash { .. }))
                || self
                    .composer
                    .area
                    .text()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .starts_with('/'))
    }
}

#[cfg(test)]
#[path = "keys_burst_tests.rs"]
mod tests;
