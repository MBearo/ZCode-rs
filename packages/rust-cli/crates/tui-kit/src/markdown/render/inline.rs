//! 行内节点 → 带样式的段：强调、行内代码（`Whole`）、链接（下划线颜色与 OSC 8 目标）、图片，以及流式时还没显示的词尾。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/render/MarkdownRender.scala（feat/tui a2bf878b34）。
use std::borrow::Cow;

use crate::markdown::ast::Inline;
use crate::render::{Decor, Span, Style, attr, width};

use super::{HIDDEN, Md};

/// 一段同一样式的行内文字（Scala 的 `Run`）。
#[derive(Clone, Debug)]
pub(super) struct Run {
    pub(super) text: Cow<'static, str>,
    pub(super) style: Style,
    pub(super) decor: Decor,
}

impl Run {
    fn new(text: impl Into<Cow<'static, str>>, style: Style) -> Run {
        Run {
            text: text.into(),
            style,
            decor: Decor::PLAIN,
        }
    }

    fn decorated(text: impl Into<Cow<'static, str>>, style: Style, decor: &Decor) -> Run {
        Run {
            text: text.into(),
            style,
            decor: decor.clone(),
        }
    }
}

impl Md {
    /// 段落与标题的段；`mark` 时把代理的字标成 `Written`（表格不标：它整张落下）。
    pub(super) fn spans(&self, runs: Vec<Run>, mark: bool) -> Vec<Span> {
        runs.into_iter()
            .map(|r| {
                let style = if mark { self.own(r.style) } else { r.style };
                Span::with_decor(r.text, style, r.decor)
            })
            .collect()
    }

    /// 代理写的字上的 `st`：回答还在流式时标成 `Written`。
    pub(super) fn own(&self, st: Style) -> Style {
        if self.written {
            st.with_attr(attr::WRITTEN)
        } else {
            st
        }
    }

    /// 只有一张图的段落是 fg3 的 `Image`、两个空格与 fg3 斜体的替代文字；别的段落是它的行内段。
    pub(super) fn paragraph_runs(&self, content: &[Inline], ink: Style) -> Vec<Run> {
        if let [Inline::Image { alt, .. }] = content {
            return vec![
                Run::new("Image", self.theme.fg3),
                Run::new("  ", Style::DEFAULT),
                Run::new(alt.clone(), self.theme.fg3.with_attr(attr::ITALIC)),
            ];
        }
        self.inline_runs(content, ink)
    }

    pub(super) fn inline_runs(&self, content: &[Inline], base: Style) -> Vec<Run> {
        let mut out = Vec::new();
        self.go(content, base, &Decor::PLAIN, false, &mut out);
        out
    }

    fn go(&self, ins: &[Inline], style: Style, decor: &Decor, linked: bool, out: &mut Vec<Run>) {
        for i in ins {
            match i {
                Inline::Text(v) => out.push(Run::decorated(v.clone(), style, decor)),
                Inline::SoftBreak => out.push(Run::decorated(" ", style, decor)),
                Inline::HardBreak => out.push(Run::decorated("\n", style, decor)),
                Inline::Emph(c) => self.go(c, style.with_attr(attr::ITALIC), decor, linked, out),
                Inline::Strong(c) => self.go(c, style.with_attr(attr::BOLD), decor, linked, out),
                Inline::Strikethrough(c) => {
                    let st = style.merge(self.theme.fg3).with_attr(attr::STRIKE);
                    self.go(c, st, decor, linked, out);
                }
                Inline::Code(v) => {
                    // 链接里的代码随链接（brand 与下划线，反引号也一样）；否则冷灰的代码夹在半色调的反引号里。
                    // 放得下时整段不折行：断在中间会在行末留下一个孤零零的反引号。
                    let whole = style.with_attr(attr::WHOLE);
                    if linked {
                        out.push(Run::decorated(format!("`{v}`"), whole, decor));
                    } else {
                        let tick = Run::decorated("`", whole.merge(self.theme.tick), decor);
                        out.push(tick.clone());
                        out.push(Run::decorated(
                            v.clone(),
                            whole.merge(self.theme.code_ink),
                            decor,
                        ));
                        out.push(tick);
                    }
                }
                Inline::Link { content, dest, .. } => {
                    let d = self.link_decor(dest);
                    self.go(content, style.merge(self.link_style), &d, true, out);
                }
                Inline::Image { alt, .. } => {
                    let st = style.merge(self.theme.fg3.with_attr(attr::ITALIC));
                    out.push(Run::decorated(alt.clone(), st, decor));
                }
                Inline::Autolink { uri, is_email } => {
                    let target = if *is_email {
                        Cow::Owned(format!("mailto:{uri}"))
                    } else {
                        Cow::Borrowed(uri.as_str())
                    };
                    let d = self.link_decor(&target);
                    out.push(Run::decorated(
                        uri.clone(),
                        style.merge(self.link_style),
                        &d,
                    ));
                }
            }
        }
    }

    /// 链接的下划线颜色，以及 `dest` 带 scheme（`https:`、`mailto:`）时它的 OSC 8 目标：相对路径终端打不开。
    /// 目标里丢掉控制字符，回答就不能借它夹带转义序列；也丢掉 [`HIDDEN`]：播放头之后的字还没显示，但它们是地址。
    fn link_decor(&self, dest: &str) -> Decor {
        let target = if has_scheme(dest) {
            dest.chars()
                .filter(|&c| !c.is_control() && c != HIDDEN)
                .collect()
        } else {
            String::new()
        };
        Decor {
            underline: self.link_underline,
            link: target,
        }
    }
}

/// URI 的 scheme：至少两个字符，所以 `C:\path` 不是。Scala 的正则 `(?s)[A-Za-z][A-Za-z0-9+.-]+:.*`（整串匹配）。
fn has_scheme(dest: &str) -> bool {
    let mut cs = dest.chars();
    if !cs.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let mut n = 0usize;
    for c in cs {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-') {
            n += 1;
        } else {
            return c == ':' && n >= 1;
        }
    }
    false
}

/// `runs` 里显示出的部分，与 [`HIDDEN`] 之后还没显示的字的宽度。
pub(super) fn cut_hidden(mut runs: Vec<Run>) -> (Vec<Run>, usize) {
    let Some(k) = runs.iter().position(|r| r.text.contains(HIDDEN)) else {
        return (runs, 0);
    };
    let rest_runs = runs.split_off(k + 1);
    let Some(mut r) = runs.pop() else {
        return (runs, 0);
    };
    let at = r.text.find(HIDDEN).unwrap_or(0);
    let mut hidden = width::string_width(&r.text[at + HIDDEN.len_utf8()..]);
    hidden += rest_runs
        .iter()
        .map(|x| width::string_width(&x.text))
        .sum::<usize>();
    if at > 0 {
        r.text = Cow::Owned(r.text[..at].to_owned());
        runs.push(r);
    }
    (runs, hidden)
}
