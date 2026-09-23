//! 重定向 token（`readRedirection` / `redirectToken`）。
//!
//! 刻意保留的 unbash 怪癖（均已用 Node oracle 验证）：
//! - 目标缺失（行尾或 EOF）时不会重新读单词，`content` 沿用上一次扫描的 `word_text`：
//!   `echo foo >` 的目标是 `foo`。EOF 时 `redirect_target_pos` 也是陈旧值。
//! - heredoc token 不设置 `target_pos/target_end`，沿用该槽位上一个重定向的目标范围；目标单词的
//!   结构由那段陈旧范围重新扫描得到（`cat >"o u t" >a <<EOF` 的 heredoc 目标值是 `o u t`）。
//! - `<(`/`>(` 紧跟在 `<`/`>` 后面时产生 Word token；带 fd 前缀时（`2<(ls)`）整段成为一个单词，
//!   重新扫描只看到 `2`，因而既不是动态单词也不是重定向。

use super::{AMP, GT, LPAREN, LT, Lexer, NL, PIPE, Tk};

impl Lexer<'_> {
    /// `readRedirection`：`pos` 指向 `<` 或 `>`。
    pub(super) fn read_redirection(&mut self, slot: usize, start: usize) {
        let len = self.src_end;
        let ch = self.c(self.pos);
        self.pos += 1;
        let next = if self.pos < len { self.c(self.pos) } else { 0 };
        let op: &'static [u8] = if ch == LT {
            match next {
                LT => {
                    self.pos += 1;
                    let third = if self.pos < len { self.c(self.pos) } else { 0 };
                    if third == LT {
                        self.pos += 1;
                        self.skip_spaces_and_tabs();
                        self.redirect_target_pos = self.pos;
                        if self.pos < len && self.c(self.pos) != NL {
                            self.read_word_text();
                        }
                        self.redirect_token(slot, b"<<<", start);
                        return;
                    }
                    let strip = third == u32::from(b'-');
                    if strip {
                        self.pos += 1;
                    }
                    self.skip_spaces_and_tabs();
                    self.read_heredoc_delimiter();
                    self.push_pending_heredoc(strip);
                    self.set_token(
                        slot,
                        Tk::Redirect,
                        if strip { b"<<-" } else { b"<<" },
                        start,
                        self.pos,
                    );
                    self.slots[slot].content = Some(self.here_delim.clone());
                    return;
                }
                LPAREN => {
                    self.read_process_substitution(slot, start);
                    return;
                }
                GT => {
                    self.pos += 1;
                    b"<>"
                }
                AMP => {
                    self.pos += 1;
                    b"<&"
                }
                _ => b"<",
            }
        } else {
            match next {
                LPAREN => {
                    self.read_process_substitution(slot, start);
                    return;
                }
                GT => {
                    self.pos += 1;
                    b">>"
                }
                AMP => {
                    self.pos += 1;
                    b">&"
                }
                PIPE => {
                    self.pos += 1;
                    b">|"
                }
                _ => b">",
            }
        };
        self.skip_spaces_and_tabs();
        if self.pos < len {
            let nc = self.c(self.pos);
            if (nc == LT || nc == GT) && self.pos + 1 < len && self.c(self.pos + 1) == LPAREN {
                let target_start = self.pos;
                self.pos += 2;
                self.extract_balanced();
                let target = self.slice(target_start, self.pos).to_vec();
                self.set_token(slot, Tk::Redirect, op, start, self.pos);
                let token = &mut self.slots[slot];
                token.content = Some(target);
                token.target_pos = target_start;
                token.target_end = self.pos;
                return;
            }
            self.redirect_target_pos = self.pos;
            if nc != NL {
                self.read_word_text();
            }
        }
        self.redirect_token(slot, op, start);
    }

    pub(super) fn redirect_token(&mut self, slot: usize, op: &[u8], start: usize) {
        self.set_token(slot, Tk::Redirect, op, start, self.pos);
        let content = self.word_text.clone();
        let token = &mut self.slots[slot];
        token.content = Some(content);
        token.target_pos = self.redirect_target_pos;
        token.target_end = self.pos;
    }

    /// `readProcessSubstitution`：`pos` 指向 `(`，产生一个覆盖 `start..` 的 Word token。
    fn read_process_substitution(&mut self, slot: usize, start: usize) {
        self.pos += 1;
        self.extract_balanced();
        let text = self.slice(start, self.pos).to_vec();
        self.set_token(slot, Tk::Word, &text, start, self.pos);
    }
}
