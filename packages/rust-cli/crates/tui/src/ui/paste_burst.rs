// 复制自 Codex（https://github.com/openai/codex）`codex-rs/tui/src/bottom_pane/paste_burst.rs`（c3d3b142d），
// Apache License 2.0，Copyright 2025 OpenAI。改动（修复线 F65）：注释改写成中文并缩短；空闲超时只取 Codex 的 Windows
// 值 60 ms（本工程只在 Windows 上启用）；去掉本工程用不到的方法；加 `deadline`（事件循环的计时）。
//! 没有括号粘贴的终端上的粘贴判定（docs/specs/rust-cli/tui-input.md §9.2）：Windows 上粘贴的文字是一串很快的
//! 字符键、enter 与 tab。纯状态机，不改输入框：调用方把文字键喂进来，按返回的决定扣住第一个字符、缓存成一次粘贴，
//! 或照常键入。时间都由调用方传入（测试拨时钟）。
//!
//! 调用方式：
//! - ASCII 文字键调 [`PasteBurst::on_plain_char`]，非 ASCII（输入法）调 [`PasteBurst::on_plain_char_no_hold`]；
//!   要缓存时调 [`PasteBurst::append_char_to_buffer`]；
//! - enter / tab 在 burst 中调 [`PasteBurst::append_control_char_if_active`]；
//! - 计时到了调 [`PasteBurst::flush_if_due`]：`Typed` 照常键入，`Paste` 当作一次粘贴；
//! - 别的键之前调 [`PasteBurst::flush_before_modified_input`] 交出缓存，之后调
//!   [`PasteBurst::clear_window_after_non_char`]，下一个字符不再算进上一段。
use std::time::{Duration, Instant};

/// 连续多少个字符起认成 burst。
const PASTE_BURST_MIN_CHARS: u16 = 3;
/// burst 之后这么久里的 enter 仍是换行。
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);
/// 两个字符的间隔不超过它才算连续；也是扣住第一个字符的时长。
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);
/// 缓存中最后一个字符之后这么久没有新字符，缓存作为一次粘贴交出（Windows 上粘贴的按键较慢）。
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(60);

#[derive(Debug, Clone, Default)]
pub struct PasteBurst {
    last_plain_char_time: Option<Instant>,
    consecutive_plain_char_burst: u16,
    burst_window_until: Option<Instant>,
    buffer: String,
    active: bool,
    /// 扣住的第一个快速字符（防闪烁：认成 burst 之前不画）。
    pending_first_char: Option<(char, Instant)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CharDecision {
    /// 开始缓存，并把已经键入的 `retro_chars` 个字符收回来。
    BeginBuffer { retro_chars: u16 },
    /// 缓存中：这个字符进缓存。
    BufferAppend,
    /// 先扣住这个字符，看后面是不是 burst。
    RetainFirstChar,
    /// 用扣住的字符开始缓存（不用收回）。
    BeginBufferFromPending,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RetroGrab {
    pub start_byte: usize,
    pub grabbed: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FlushResult {
    Paste(String),
    Typed(char),
    None,
}

impl PasteBurst {
    /// 一个 ASCII 文字键。
    pub fn on_plain_char(&mut self, ch: char, now: Instant) -> CharDecision {
        self.note_plain_char(now);
        if self.active {
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return CharDecision::BufferAppend;
        }
        // 扣着第一个字符时又来了一个快的：开始缓存（第一个没画过，不用收回）。
        if let Some((held, held_at)) = self.pending_first_char
            && now.duration_since(held_at) <= PASTE_BURST_CHAR_INTERVAL
        {
            self.active = true;
            self.pending_first_char = None;
            self.buffer.push(held);
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return CharDecision::BeginBufferFromPending;
        }
        if self.consecutive_plain_char_burst >= PASTE_BURST_MIN_CHARS {
            return CharDecision::BeginBuffer {
                retro_chars: self.consecutive_plain_char_burst.saturating_sub(1),
            };
        }
        self.pending_first_char = Some((ch, now));
        CharDecision::RetainFirstChar
    }

    /// 一个非 ASCII 文字键（输入法）：不扣住，只返回 `BufferAppend` 或 `BeginBuffer`；None 时照常键入。
    pub fn on_plain_char_no_hold(&mut self, now: Instant) -> Option<CharDecision> {
        self.note_plain_char(now);
        if self.active {
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return Some(CharDecision::BufferAppend);
        }
        if self.consecutive_plain_char_burst >= PASTE_BURST_MIN_CHARS {
            return Some(CharDecision::BeginBuffer {
                retro_chars: self.consecutive_plain_char_burst.saturating_sub(1),
            });
        }
        None
    }

    fn note_plain_char(&mut self, now: Instant) {
        match self.last_plain_char_time {
            Some(prev) if now.duration_since(prev) <= PASTE_BURST_CHAR_INTERVAL => {
                self.consecutive_plain_char_burst =
                    self.consecutive_plain_char_burst.saturating_add(1);
            }
            _ => self.consecutive_plain_char_burst = 1,
        }
        self.last_plain_char_time = Some(now);
    }

    /// 计时到了：缓存作为一次粘贴交出，或扣住的字符照常键入。比较用 `>`（越过阈值至少 1 ms）。
    pub fn flush_if_due(&mut self, now: Instant) -> FlushResult {
        let timeout = if self.is_active_internal() {
            PASTE_BURST_ACTIVE_IDLE_TIMEOUT
        } else {
            PASTE_BURST_CHAR_INTERVAL
        };
        let timed_out = self
            .last_plain_char_time
            .is_some_and(|t| now.duration_since(t) > timeout);
        if timed_out && self.is_active_internal() {
            self.active = false;
            FlushResult::Paste(std::mem::take(&mut self.buffer))
        } else if timed_out {
            match self.pending_first_char.take() {
                Some((ch, _)) => FlushResult::Typed(ch),
                None => FlushResult::None,
            }
        } else {
            FlushResult::None
        }
    }

    /// 下一次该调 [`Self::flush_if_due`] 的时刻（越过阈值 1 ms）；没有扣住的字符与缓存时为 None。
    pub fn deadline(&self) -> Option<Instant> {
        if !self.is_active() {
            return None;
        }
        let timeout = if self.is_active_internal() {
            PASTE_BURST_ACTIVE_IDLE_TIMEOUT
        } else {
            PASTE_BURST_CHAR_INTERVAL
        };
        self.last_plain_char_time
            .map(|t| t + timeout + Duration::from_millis(1))
    }

    /// burst 中的 enter（`\n`）或 tab：进缓存，不当作快捷键；扣住的字符先进缓存。返回 false 表示不在 burst 中。
    pub fn append_control_char_if_active(&mut self, ch: char, now: Instant) -> bool {
        if !self.is_active() {
            return false;
        }
        if let Some((held, _)) = self.pending_first_char.take() {
            self.buffer.push(held);
        }
        self.active = true;
        self.buffer.push(ch);
        self.last_plain_char_time = Some(now);
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
        true
    }

    /// enter 是不是换行（在 burst 中或刚结束的窗口里）。
    pub fn newline_should_insert_instead_of_submit(&self, now: Instant) -> bool {
        let in_burst_window = self.burst_window_until.is_some_and(|until| now <= until);
        self.is_active() || in_burst_window
    }

    /// 字符立即键入的路径上 enter 是不是换行：另加“上一个字符键在 8 ms 内”。
    pub fn direct_insert_newline_should_insert(&self, now: Instant) -> bool {
        self.newline_should_insert_instead_of_submit(now)
            || self
                .last_plain_char_time
                .is_some_and(|t| now.duration_since(t) <= PASTE_BURST_CHAR_INTERVAL)
    }

    /// 延长 enter 当作换行的窗口。
    pub fn extend_window(&mut self, now: Instant) {
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    fn begin_with_retro_grabbed(&mut self, grabbed: &str, now: Instant) {
        self.buffer.push_str(grabbed);
        self.active = true;
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    pub fn append_char_to_buffer(&mut self, ch: char, now: Instant) {
        self.buffer.push(ch);
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    /// 已经在缓存时把这个字符放进去；返回 false 表示没在缓存。
    pub fn try_append_char_if_active(&mut self, ch: char, now: Instant) -> bool {
        if self.active || !self.buffer.is_empty() {
            self.append_char_to_buffer(ch, now);
            true
        } else {
            false
        }
    }

    /// 要不要把光标前已经键入的 `retro_chars` 个字符收回来、开始缓存：收回的那段含空白或至少 16 个字符才算像粘贴
    /// （短的词不算，输入法的连续上屏也不算）。返回 Some 时调用方从输入框删掉 `start_byte..光标`。
    pub fn decide_begin_buffer(
        &mut self,
        now: Instant,
        before: &str,
        retro_chars: usize,
    ) -> Option<RetroGrab> {
        let start_byte = retro_start_index(before, retro_chars);
        let grabbed = before[start_byte..].to_owned();
        /// 收回的一段至少这么多个字符才像粘贴（不含空白时）。
        const PASTEY_CHARS: usize = 16;
        let looks_pastey =
            grabbed.chars().any(char::is_whitespace) || grabbed.chars().count() >= PASTEY_CHARS;
        if !looks_pastey {
            return None;
        }
        self.begin_with_retro_grabbed(&grabbed, now);
        Some(RetroGrab {
            start_byte,
            grabbed,
        })
    }

    /// 别的输入之前：立即交出缓存（含扣住的字符）。
    pub fn flush_before_modified_input(&mut self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        self.active = false;
        let mut out = std::mem::take(&mut self.buffer);
        if let Some((ch, _)) = self.pending_first_char.take() {
            out.push(ch);
        }
        Some(out)
    }

    /// 只清掉计时窗口与扣住的字符（缓存要先交出）。
    pub fn clear_window_after_non_char(&mut self) {
        self.consecutive_plain_char_burst = 0;
        self.last_plain_char_time = None;
        self.burst_window_until = None;
        self.active = false;
        self.pending_first_char = None;
    }

    /// 在 burst 的任何中间状态（缓存中、缓存不空、扣着第一个字符）。
    pub fn is_active(&self) -> bool {
        self.is_active_internal() || self.pending_first_char.is_some()
    }

    fn is_active_internal(&self) -> bool {
        self.active || !self.buffer.is_empty()
    }

    /// 收回失败时放弃这次缓存。
    pub fn clear_after_explicit_paste(&mut self) {
        self.last_plain_char_time = None;
        self.consecutive_plain_char_burst = 0;
        self.burst_window_until = None;
        self.active = false;
        self.buffer.clear();
        self.pending_first_char = None;
    }
}

/// `before` 末尾 `retro_chars` 个字符的起点（字节）。
pub fn retro_start_index(before: &str, retro_chars: usize) -> usize {
    if retro_chars == 0 {
        return before.len();
    }
    before
        .char_indices()
        .rev()
        .nth(retro_chars.saturating_sub(1))
        .map_or(0, |(idx, _)| idx)
}

#[cfg(test)]
#[path = "paste_burst_tests.rs"]
mod tests;
