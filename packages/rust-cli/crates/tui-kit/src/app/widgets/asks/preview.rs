//! 请求在它的行下面的预览（docs/puffin-tui.md §4.2「预览」；docs/specs/rust-cli/tui-kit.md §4.4）：子代理的请求说谁在问，命令
//! 与它的说明、改动的行、新文件的全文、地址与它想要的、输入的字段，都在 `│` 的轨上；提问的一页是问题与选中的回答的预览；
//! 这个会话里没有行的请求自己的一行。纯的。
//! 移植自 Puffin 的 shell/asks/words.rs 的「行下面」一节。
use crate::app::element::{Element, Wrap, br, single_line, wrap_styled, wrap_text};
use crate::markdown::render::INDENT;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::super::fit::measured;
use super::super::tools::Sign;
use super::data::{Ask, AskBody, AskPreview};
use super::{QuestionUi, pages_of};

fn lines(text: &str) -> Vec<String> {
    text.replace('\r', "")
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// `   │ `：预览的轨，子代理的请求再往里 `indent` 列。
fn rail(theme: &Theme, indent: usize) -> Vec<Span> {
    vec![
        Span::plain(format!("{INDENT}{}", " ".repeat(indent))),
        Span::new("│", theme.line),
        Span::plain(" "),
    ]
}

fn rail_line(theme: &Theme, indent: usize, text: &str, st: Style, wrap: Wrap) -> Element {
    if text.is_empty() {
        return single_line(
            vec![
                Span::plain(format!("{INDENT}{}", " ".repeat(indent))),
                Span::new("│", theme.line),
            ],
            false,
        );
    }
    measured(wrap_styled(
        rail(theme, indent),
        rail(theme, indent),
        vec![Span::new(text.replace('\t', "    "), st)],
        wrap,
    ))
}

fn rail_spans(theme: &Theme, indent: usize, spans: Vec<Span>) -> Element {
    let mut all = rail(theme, indent);
    all.extend(spans);
    measured(single_line(all, false))
}

/// 权限请求要做的事，在它的行下面（docs/puffin-tui.md §4.2「预览」）。`row_summary` 是行已经说了的。
pub fn permission_preview(
    theme: &Theme,
    ask: &Ask,
    row_summary: &str,
    indent: usize,
) -> Vec<Element> {
    let who = ask.origin.as_ref().map(|o| format!("{} asks", o.who));
    permission_preview_said(theme, ask, row_summary, indent, who.as_deref())
}

/// 同 [`permission_preview`]，子代理的请求那一句（「`谁` asks」）由调用方给（旧界面的中文界面与旧会话的询问用）。
pub fn permission_preview_said(
    theme: &Theme,
    ask: &Ask,
    row_summary: &str,
    indent: usize,
    who: Option<&str>,
) -> Vec<Element> {
    let mut out: Vec<Element> = who
        .iter()
        .map(|who| rail_line(theme, indent, who, theme.fg3, Wrap::Word))
        .collect();
    let AskBody::Permission { preview, .. } = &ask.body else {
        return out;
    };
    match preview {
        AskPreview::Command { command, reason } => {
            if !reason.is_empty() && reason != row_summary {
                out.push(rail_line(theme, indent, reason, theme.fg2, Wrap::Word));
            }
            if !command.is_empty() && command != row_summary {
                for (i, l) in lines(command).iter().enumerate() {
                    let lead = if i == 0 { "$ " } else { "  " };
                    out.push(rail_line(
                        theme,
                        indent,
                        &format!("{lead}{l}"),
                        Style::DEFAULT,
                        Wrap::Word,
                    ));
                }
            }
        }
        AskPreview::Diff { lines: ds, .. } => {
            for d in ds {
                out.push(match d.sign {
                    Sign::Remove => rail_line(
                        theme,
                        indent,
                        &format!("− {}", d.text),
                        Style::fg(theme.err),
                        Wrap::Char,
                    ),
                    Sign::Add => rail_line(
                        theme,
                        indent,
                        &format!("+ {}", d.text),
                        Style::fg(theme.ok),
                        Wrap::Char,
                    ),
                    Sign::Context => rail_line(theme, indent, &d.text, theme.fg3, Wrap::Char),
                });
            }
        }
        AskPreview::File { text, .. } => {
            out.extend(
                lines(text)
                    .iter()
                    .map(|l| rail_line(theme, indent, l, theme.fg2, Wrap::Char)),
            );
        }
        AskPreview::Fetch { url, prompt } => {
            out.push(rail_line(theme, indent, url, Style::DEFAULT, Wrap::Char));
            if !prompt.is_empty() {
                out.push(rail_line(theme, indent, prompt, theme.fg3, Wrap::Word));
            }
        }
        AskPreview::Fields { fields } => {
            for f in fields {
                let first = lines(&f.value).into_iter().next().unwrap_or_default();
                out.push(rail_spans(
                    theme,
                    indent,
                    vec![
                        Span::new(f.key.clone(), theme.fg3),
                        Span::plain("  "),
                        Span::new(first, theme.fg2),
                    ],
                ));
            }
        }
    }
    out
}

/// 提问的一页，在它的行下面：问题，与选中的回答的预览。
pub fn question_preview(
    theme: &Theme,
    ask: &Ask,
    ui: &QuestionUi<'_>,
    typed: bool,
    indent: usize,
) -> Vec<Element> {
    let Some(p) = pages_of(ask).get(ui.page) else {
        return Vec::new();
    };
    let lead = format!("{INDENT}{}", " ".repeat(indent));
    let q = measured(wrap_text(
        lead.clone(),
        lead,
        p.question.clone(),
        Wrap::Word,
    ));
    let mut out = vec![q];
    if !p.multi
        && !typed
        && let Some(t) = p
            .options
            .get(ui.choice)
            .map(|o| &o.preview)
            .filter(|t| !t.is_empty())
    {
        out.extend(
            lines(t)
                .iter()
                .map(|l| rail_line(theme, indent, l, theme.fg2, Wrap::Char)),
        );
    }
    out
}

/// 在这个会话里没有行的请求（子代理的、MCP 的提问）自己的一行：记号、名字与摘要，往里 `indent` 列。
pub fn own_row(theme: &Theme, ask: &Ask, mark: Style, indent: usize) -> Element {
    let mut spans = vec![
        Span::plain(format!(" {}", " ".repeat(indent))),
        Span::new("●", mark),
        Span::plain(" "),
        Span::new(ask.name.clone(), theme.fg2.with_attr(attr::BOLD)),
    ];
    if !ask.summary.is_empty() {
        spans.push(Span::plain("  "));
        spans.push(Span::new(ask.summary.clone(), theme.fg3));
    }
    single_line(spans, false)
}

/// 计划的预览前的空行（`askPreview` 的计划分支用）。
pub fn plan_gap() -> Element {
    br()
}
