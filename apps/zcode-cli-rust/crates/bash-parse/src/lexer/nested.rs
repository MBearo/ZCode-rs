//! `skipDQ` / `extractBalanced`：跳过嵌套的双引号与 `$(...)`。
//!
//! JS 中二者互相递归（`"$("$(...` 可嵌套上千层）。这里改用显式栈，逐步执行与 JS 完全相同的分支，
//! 避免耗尽线程栈；同时按 JS 调用帧计数，模拟 Node 的栈溢出（见 `MAX_SCAN_FRAMES`）。

use super::{
    BACKSLASH, BACKTICK, DOLLAR, DQUOTE, LBRACE, LPAREN, Lexer, RBRACE, RPAREN, SQUOTE, is_meta,
    is_special,
};

/// JS 中 `extractBalanced`/`skipDQ` 的递归帧上限。实测 Node 24 冷启动时约 2560 层 `"$(`
/// （≈5100 帧）抛出 RangeError（被 `analyzeBashCommand` 捕获为 parse error），JIT 预热后则能跑完
/// 10k 字符内的全部 3300 层——Node 在此区间的结论取决于 JIT 状态，无法精确复刻。这里取偏保守的
/// 4000 帧：更深的嵌套一律按 parse error（不安全）处理，保证不会在 Node 可能判为不安全时判为安全。
const MAX_SCAN_FRAMES: usize = 4000;

#[derive(Clone, Copy)]
enum Frame {
    /// `extractBalanced` 的慢速路径循环。
    Balanced { depth: u32, case_depth: u32 },
    /// `skipDQ` 主循环。
    DoubleQuoted,
    /// `skipDQ` 内部 `${ ... }` 的配对循环（JS 中是内联循环，不占调用帧）。
    DqBrace { depth: u32 },
}

#[derive(Default)]
struct Frames {
    stack: Vec<Frame>,
    calls: usize,
}

impl Frames {
    fn push(&mut self, frame: Frame) {
        if !matches!(frame, Frame::DqBrace { .. }) {
            self.calls += 1;
        }
        self.stack.push(frame);
    }

    fn pop(&mut self) {
        if let Some(frame) = self.stack.pop()
            && !matches!(frame, Frame::DqBrace { .. })
        {
            self.calls -= 1;
        }
    }

    fn replace_top(&mut self, frame: Frame) {
        if let Some(top) = self.stack.last_mut() {
            *top = frame;
        }
    }
}

impl Lexer<'_> {
    pub(super) fn skip_dq(&mut self) {
        let mut frames = Frames::default();
        frames.push(Frame::DoubleQuoted);
        self.run_frames(frames);
    }

    /// `extractBalanced`：`pos` 已越过开头的 `(`，结束时越过匹配的 `)`（未闭合则停在串尾）。
    pub(super) fn extract_balanced(&mut self) {
        let mut frames = Frames::default();
        self.push_balanced(&mut frames);
        self.run_frames(frames);
    }

    /// `extractBalanced` 的快速路径；需要处理嵌套结构时压入慢速路径帧。
    fn push_balanced(&mut self, frames: &mut Frames) {
        let start = self.pos;
        while self.pos < self.src_end {
            let c = self.c(self.pos);
            if c == RPAREN {
                self.pos += 1;
                return;
            }
            if matches!(c, LPAREN | BACKSLASH | SQUOTE | DQUOTE | BACKTICK)
                || self.case_keyword_at(start)
            {
                break;
            }
            self.pos += 1;
        }
        frames.push(Frame::Balanced {
            depth: 1,
            case_depth: 0,
        });
    }

    /// 快速路径里识别单词边界上的 `case`（之后要在慢速路径里跟踪 case..esac 中的 `)`）。
    fn case_keyword_at(&self, start: usize) -> bool {
        let pos = self.pos;
        let len = self.src_end;
        self.c(pos) == u32::from(b'c')
            && (pos == start || is_special(self.c(pos - 1)))
            && pos + 3 < len
            && self.c(pos + 1) == u32::from(b'a')
            && self.c(pos + 2) == u32::from(b's')
            && self.c(pos + 3) == u32::from(b'e')
            && (pos + 4 >= len || is_meta(self.c(pos + 4)))
    }

    /// 反引号内容：跳到匹配的反引号之后。`guarded` 区分 extractBalanced（转义后检查边界）与
    /// skipDQ（不检查，可能越过 `src_end` 一位，JS 亦然）。
    fn skip_backtick(&mut self, guarded: bool) {
        let len = self.src_end;
        self.pos += 1;
        while self.pos < len && self.c(self.pos) != BACKTICK {
            if self.c(self.pos) == BACKSLASH {
                self.pos += 1;
            }
            if !guarded || self.pos < len {
                self.pos += 1;
            }
        }
        if self.pos < len {
            self.pos += 1;
        }
    }

    fn run_frames(&mut self, mut frames: Frames) {
        let len = self.src_end;
        while let Some(&frame) = frames.stack.last() {
            if frames.calls > MAX_SCAN_FRAMES {
                self.overflow = true;
                self.pos = len;
                return;
            }
            match frame {
                Frame::Balanced { depth, case_depth } => {
                    self.step_balanced(&mut frames, depth, case_depth)
                }
                Frame::DoubleQuoted => self.step_double_quoted(&mut frames),
                Frame::DqBrace { depth } => self.step_dq_brace(&mut frames, depth),
            }
        }
    }

    fn step_balanced(&mut self, frames: &mut Frames, mut depth: u32, mut case_depth: u32) {
        let len = self.src_end;
        if self.pos >= len || depth == 0 {
            frames.pop();
            return;
        }
        match self.c(self.pos) {
            LPAREN => {
                depth += 1;
                self.pos += 1;
            }
            RPAREN => {
                if case_depth == 0 {
                    depth -= 1;
                }
                self.pos += 1;
            }
            BACKSLASH => {
                self.pos += 1;
                if self.pos < len {
                    self.pos += 1;
                }
            }
            SQUOTE => {
                self.pos += 1;
                self.skip_sq();
            }
            DQUOTE => {
                self.pos += 1;
                frames.replace_top(Frame::Balanced { depth, case_depth });
                frames.push(Frame::DoubleQuoted);
                return;
            }
            BACKTICK => self.skip_backtick(true),
            _ => {
                let word_start = self.pos;
                while self.pos < len && !is_special(self.c(self.pos)) {
                    self.pos += 1;
                }
                if self.pos == word_start {
                    self.pos += 1;
                } else if self.pos - word_start == 4 {
                    match self.slice(word_start, self.pos) {
                        b"case" => case_depth += 1,
                        b"esac" if case_depth > 0 => case_depth -= 1,
                        _ => {}
                    }
                }
            }
        }
        frames.replace_top(Frame::Balanced { depth, case_depth });
    }

    fn step_double_quoted(&mut self, frames: &mut Frames) {
        let len = self.src_end;
        if self.pos >= len {
            frames.pop();
            return;
        }
        let ch = self.c(self.pos);
        let next = if self.pos + 1 < len {
            self.c(self.pos + 1)
        } else {
            0
        };
        if ch == DQUOTE {
            self.pos += 1;
            frames.pop();
        } else if ch == BACKSLASH {
            self.pos += 2;
        } else if ch == DOLLAR && next == LPAREN {
            self.pos += 2;
            self.push_balanced(frames);
        } else if ch == DOLLAR && next == LBRACE {
            self.pos += 2;
            frames.push(Frame::DqBrace { depth: 1 });
        } else if ch == BACKTICK {
            self.skip_backtick(false);
        } else {
            self.pos += 1;
        }
    }

    fn step_dq_brace(&mut self, frames: &mut Frames, mut depth: u32) {
        let len = self.src_end;
        if self.pos >= len || depth == 0 {
            frames.pop();
            return;
        }
        let c = self.c(self.pos);
        if c == RBRACE {
            depth -= 1;
            if depth == 0 {
                self.pos += 1;
                frames.pop();
                return;
            }
        } else if c == LBRACE && self.pos > 0 && self.c(self.pos - 1) == DOLLAR {
            depth += 1;
        } else if c == BACKSLASH {
            self.pos += 1;
        } else if c == SQUOTE {
            self.pos += 1;
            self.skip_sq();
            frames.replace_top(Frame::DqBrace { depth });
            return;
        } else if c == DQUOTE {
            self.pos += 1;
            frames.replace_top(Frame::DqBrace { depth });
            frames.push(Frame::DoubleQuoted);
            return;
        }
        self.pos += 1;
        frames.replace_top(Frame::DqBrace { depth });
    }
}
