//! 行内解析的链接部分：`](` 之后的目标与标题，以及 `<…>` 自动链接的判定。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/InlineParser.scala（feat/tui a2bf878b34）的
//! `parseLinkTail` / `parseDest` / `parseTitle` / `skipWs` / `unescape` / `isUriAutolink` / `isEmailAutolink`。
use super::java_char::{is_letter, is_letter_or_digit, is_regex_space as is_ws};
use super::{Parser, is_ascii_punct};

impl Parser {
    pub(super) fn parse_link_tail(&self, from: usize) -> Option<(String, Option<String>, usize)> {
        if from >= self.n || self.s[from] != '(' {
            return None;
        }
        let j = self.skip_ws(from + 1);
        let (dest, after_dest) = self.parse_dest(j)?;
        let j = self.skip_ws(after_dest);
        let (title, after_title) = self.parse_title(j);
        let j = self.skip_ws(after_title);
        (j < self.n && self.s[j] == ')').then_some((dest, title, j + 1))
    }

    fn parse_dest(&self, from: usize) -> Option<(String, usize)> {
        let (s, n) = (&self.s, self.n);
        if from >= n {
            return None;
        }
        if s[from] == '<' {
            let close = self.index_of('>', from + 1)?;
            if s[from + 1..close].contains(&'\n') {
                return None;
            }
            return Some((unescape(&s[from + 1..close]), close + 1));
        }
        let mut j = from;
        let mut depth = 0usize;
        while j < n {
            let c = s[j];
            if c == '\\' && j + 1 < n {
                j += 2;
            } else if is_ws(c) {
                break;
            } else if c == '(' {
                depth += 1;
                j += 1;
            } else if c == ')' {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                j += 1;
            } else {
                j += 1;
            }
        }
        if depth != 0 || j == from {
            None
        } else {
            Some((unescape(&s[from..j]), j))
        }
    }

    fn parse_title(&self, from: usize) -> (Option<String>, usize) {
        let (s, n) = (&self.s, self.n);
        if from >= n {
            return (None, from);
        }
        let q = s[from];
        if q != '"' && q != '\'' && q != '(' {
            return (None, from);
        }
        let close = if q == '(' { ')' } else { q };
        let mut j = from + 1;
        let mut sb = String::new();
        let mut done = false;
        while j < n && !done {
            let c = s[j];
            if c == '\\' && j + 1 < n {
                sb.push(s[j + 1]);
                j += 2;
            } else if c == close {
                done = true;
                j += 1;
            } else {
                sb.push(c);
                j += 1;
            }
        }
        if done { (Some(sb), j) } else { (None, from) }
    }

    fn skip_ws(&self, from: usize) -> usize {
        let mut j = from;
        while j < self.n && is_ws(self.s[j]) {
            j += 1;
        }
        j
    }
}

fn unescape(s: &[char]) -> String {
    let mut sb = String::with_capacity(s.len());
    let mut k = 0;
    while k < s.len() {
        let c = s[k];
        if c == '\\' && k + 1 < s.len() && is_ascii_punct(s[k + 1]) {
            sb.push(s[k + 1]);
            k += 2;
        } else {
            sb.push(c);
            k += 1;
        }
    }
    sb
}

pub(super) fn is_uri_autolink(inner: &[char]) -> bool {
    let Some(colon) = inner.iter().position(|&c| c == ':') else {
        return false;
    };
    colon > 0 && {
        let scheme = &inner[..colon];
        is_letter(scheme[0])
            && scheme
                .iter()
                .all(|&c| is_letter_or_digit(c) || c == '+' || c == '.' || c == '-')
            && !inner.iter().any(|&c| is_ws(c) || c == '<')
    }
}

pub(super) fn is_email_autolink(inner: &[char]) -> bool {
    let Some(at) = inner.iter().position(|&c| c == '@') else {
        return false;
    };
    at > 0
        && at < inner.len() - 1
        && !inner.iter().any(|&c| is_ws(c))
        && inner[at + 1..].contains(&'.')
}
