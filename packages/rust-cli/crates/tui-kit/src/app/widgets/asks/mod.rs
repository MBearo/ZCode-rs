//! 请求、提问与计划的板上的行（docs/puffin-tui.md §4.2「请求」「提问」「计划」；docs/specs/rust-cli/tui-kit.md §4.4）：板头的
//! 问题与右边还有几个在等（`2 more waiting`），回答（序号、名字、说明；选中的是 brand，打开之后点亮），提问的页名与这一页的
//! 回答（推荐的、多选的方块），计划的两个回答。纯的；入参是 [`data`] 的请求与中性的 [`QuestionUi`]，壳持有状态与按键。
//! 行下面的预览在 [`preview`]。
//! 移植自 Puffin 的 shell/asks/words.rs（Asks.scala 第 1–20、68–245、307–441 行，去掉工作流的一节）。
use std::collections::BTreeMap;

use crate::app::element::{Element, first_fit, single_line, spread};
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fit::{measured, pad};
pub use super::lists::{chosen_style, fade_in, row};
use super::modes::{self, Mode};

pub mod data;
pub mod preview;

use data::{Ask, AskBody, AskOption, AskOptionKind, AskPreview, QuestionOption, QuestionPage};

/// 提问此刻的页、选中的回答、逐页选中的回答与你自己的话（壳的请求状态的那一部分）。
#[derive(Clone, Copy, Debug)]
pub struct QuestionUi<'a> {
    pub page: usize,
    pub choice: usize,
    pub picks: &'a BTreeMap<usize, Vec<usize>>,
    pub own: &'a BTreeMap<usize, String>,
}

/// 请求打开之后多少 tick 内回答不能选：0.4 s。
pub const ARM_TICKS: i64 = 12;
/// Deny 带的字最多这么多（V4 MAX_PERMISSION_FEEDBACK_CHARS），按码点。
pub const MAX_WORDS: usize = 4096;
/// 一次允许同一工具全部开着的请求的合成回答。
pub const ALLOW_ALL: &str = "allowAll";

/// 一个回答的种类：计划的两个、合成的「全部允许」，或请求自己的选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerKind {
    Approve,
    Keep,
    AllowAll,
    Option(AskOptionKind),
}

impl AnswerKind {
    /// 日志里的名字（Asks.scala 的 `Answer.kind` 字符串）。合成的「全部允许」记作 `allowOnce`（ShellApp.scala 第 1697 行）。
    pub fn name(self) -> &'static str {
        match self {
            AnswerKind::Approve => "approve",
            AnswerKind::Keep => "keep",
            AnswerKind::AllowAll | AnswerKind::Option(AskOptionKind::AllowOnce) => "allowOnce",
            AnswerKind::Option(AskOptionKind::AllowAlways) => "allowAlways",
            AnswerKind::Option(AskOptionKind::AllowSession) => "allowSession",
            AnswerKind::Option(AskOptionKind::FullAccess) => "fullAccess",
            AnswerKind::Option(AskOptionKind::Deny) => "deny",
            AnswerKind::Option(AskOptionKind::Refine) => "refine",
        }
    }
}

/// 板上显示的一个回答：`id` 交回 agent。
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub id: String,
    pub kind: AnswerKind,
    pub label: String,
    pub desc: Vec<(String, Style)>,
}

fn indent() -> Span {
    Span::plain(INDENT)
}

/// 路径的最后一段。
pub fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

fn relative<'a>(path: &'a str, cwd: &str) -> &'a str {
    path.strip_prefix(cwd)
        .and_then(|rest| rest.strip_prefix('/').or_else(|| rest.strip_prefix('\\')))
        .unwrap_or(path)
}

/// 去掉 `scheme://`。
fn strip_scheme(url: &str) -> &str {
    let scheme_len = url
        .char_indices()
        .take_while(|(i, c)| {
            if *i == 0 {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')
            }
        })
        .count();
    if scheme_len > 0 && url[scheme_len..].starts_with("://") {
        &url[scheme_len + 3..]
    } else {
        url
    }
}

/// 权限请求的选项；别的请求没有。
pub fn options_of(ask: &Ask) -> &[AskOption] {
    match &ask.body {
        AskBody::Permission { options, .. } => options,
        _ => &[],
    }
}

/// 提问的页；别的请求没有。
pub fn pages_of(ask: &Ask) -> &[QuestionPage] {
    match &ask.body {
        AskBody::Question { pages, .. } => pages,
        _ => &[],
    }
}

pub fn is_question(ask: &Ask) -> bool {
    matches!(ask.body, AskBody::Question { .. })
}

pub fn is_plan(ask: &Ask) -> bool {
    matches!(ask.body, AskBody::Plan { .. })
}

pub fn is_permission(ask: &Ask) -> bool {
    matches!(ask.body, AskBody::Permission { .. })
}

fn preview_path(ask: &Ask) -> &str {
    match &ask.body {
        AskBody::Permission {
            preview: AskPreview::Diff { path, .. } | AskPreview::File { path, .. },
            ..
        } => path,
        _ => "",
    }
}

// ---- 板问的 ----

/// 板头上的问题（docs/puffin-tui.md §4.2「板」）。
pub fn head(ask: &Ask) -> String {
    if is_plan(ask) {
        return "Start on this plan?".to_owned();
    }
    let path = preview_path(ask);
    match ask.tool_name.as_str() {
        "Bash" => "Run this command?".to_owned(),
        "Edit" => format!("Change {}?", basename(path)),
        "Write" => format!("Write {}?", basename(path)),
        "ApplyPatch" => "Apply this patch?".to_owned(),
        "WebFetch" => "Fetch this page?".to_owned(),
        "WebSearch" => "Search the web?".to_owned(),
        t => match t.strip_prefix("mcp__").map(|rest| rest.split_once("__")) {
            Some(Some((server, tool))) => format!("Use {server} · {tool}?"),
            _ => format!("Use {t}?"),
        },
    }
}

/// agent 没给规则的命令：Autonomous 只在能毁掉工作的命令前停下，只给一次与拒绝。
pub fn risky(ask: &Ask) -> bool {
    is_permission(ask)
        && ask.tool_name == "Bash"
        && !options_of(ask).iter().any(|o| {
            matches!(
                o.kind,
                AskOptionKind::AllowAlways | AskOptionKind::AllowSession
            )
        })
}

/// 「总是」的回答保存的规则，板上的说法。
pub fn rule_text(tool_name: &str, rules: &[String], cwd: &str) -> String {
    let each: Vec<String> = match tool_name {
        "Bash" => rules
            .iter()
            .map(|r| match r.strip_suffix(":*") {
                Some(head) => format!("{head} …"),
                None => r.clone(),
            })
            .collect(),
        "Edit" | "ApplyPatch" => rules
            .iter()
            .map(|r| format!("edits to {}", basename(r)))
            .collect(),
        "Write" => rules
            .iter()
            .map(|r| format!("writes to {}", relative(r, cwd)))
            .collect(),
        "WebFetch" => rules.iter().map(|r| strip_scheme(r).to_owned()).collect(),
        _ => rules.to_vec(),
    };
    each.join(", ")
}

/// 回答，按 agent 的次序（docs/puffin-tui.md §4.2「回答的说法」）。`typed`：输入框里有字。`all_count`：全部允许会回答几个
/// 请求（从 2 起显示）。`mode` 是批准计划之后跑在的模式。
pub fn answers(
    theme: &Theme,
    ask: &Ask,
    cwd: &str,
    typed: bool,
    all_count: usize,
    mode: Option<Mode>,
) -> Vec<Answer> {
    let fg3 = |t: &str| vec![(t.to_owned(), theme.fg3)];
    let answer = |id: &str, kind: AnswerKind, label: &str, desc: Vec<(String, Style)>| Answer {
        id: id.to_owned(),
        kind,
        label: label.to_owned(),
        desc,
    };
    if is_plan(ask) {
        let m = mode.unwrap_or(Mode::Build);
        return vec![
            answer(
                "approve",
                AnswerKind::Approve,
                "Approve",
                vec![
                    ("runs in ".to_owned(), theme.fg3),
                    (modes::label(m).to_owned(), modes::style(theme, m)),
                    ("  ".to_owned(), Style::DEFAULT),
                    ("⇧⇥".to_owned(), theme.fg2),
                    (" changes it".to_owned(), theme.fg3),
                ],
            ),
            answer(
                "keep",
                AnswerKind::Keep,
                "Keep planning",
                if typed {
                    vec![("with what you typed".to_owned(), theme.fg2)]
                } else {
                    fg3("type what to change")
                },
            ),
        ];
    }
    let mut out = Vec::new();
    for o in options_of(ask) {
        let kind = AnswerKind::Option(o.kind);
        match o.kind {
            AskOptionKind::AllowOnce => {
                out.push(answer(&o.id, kind, "Allow once", Vec::new()));
                if all_count >= 2 {
                    out.push(answer(
                        ALLOW_ALL,
                        AnswerKind::AllowAll,
                        &format!("Allow all {all_count}"),
                        fg3("once each; the amber rows above are the others"),
                    ));
                }
            }
            AskOptionKind::AllowAlways => {
                let rule = rule_text(&ask.tool_name, &o.rules, cwd);
                let mut desc = if rule.is_empty() {
                    Vec::new()
                } else {
                    vec![(rule, theme.fg2), ("  ".to_owned(), Style::DEFAULT)]
                };
                desc.extend(fg3("in this project"));
                out.push(answer(&o.id, kind, "Always allow", desc));
            }
            AskOptionKind::AllowSession => {
                out.push(answer(&o.id, kind, "Always allow", fg3("in this session")));
            }
            AskOptionKind::FullAccess => {
                out.push(answer(
                    &o.id,
                    kind,
                    "Full access",
                    fg3("this session stops asking"),
                ));
            }
            AskOptionKind::Deny => {
                let desc = if typed {
                    vec![("and send what you typed".to_owned(), theme.fg2)]
                } else {
                    fg3("or type what to do instead")
                };
                out.push(answer(&o.id, kind, "Deny", desc));
            }
            // Refine 只属于工作流的请求（spec §1）。
            AskOptionKind::Refine => {}
        }
    }
    out
}

/// 打字选中的回答：Deny，计划是 Keep planning。
pub fn typed_choice(ask: &Ask, answers: &[Answer]) -> usize {
    let wanted = if is_plan(ask) {
        AnswerKind::Keep
    } else {
        AnswerKind::Option(AskOptionKind::Deny)
    };
    answers.iter().position(|a| a.kind == wanted).unwrap_or(0)
}

/// 回答的行：序号、名字、说明；选中的是 brand。`armed` 在点亮时从 0 走到 1。
pub fn answer_rows(theme: &Theme, answers: &[Answer], choice: usize, armed: f64) -> Vec<Element> {
    let lw = answers
        .iter()
        .map(|a| string_width(&a.label))
        .max()
        .unwrap_or(0)
        + 3;
    let arm = |st: Style| fade_in(theme, st, armed);
    answers
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let chosen = i == choice;
            let num = Span::new(
                format!("{}  ", i + 1),
                arm(if chosen {
                    Style::fg(theme.brand)
                } else {
                    theme.fg3
                }),
            );
            let label = Span::new(
                pad(&a.label, lw),
                arm(if chosen {
                    chosen_style(theme)
                } else {
                    theme.fg2
                }),
            );
            let mut spans = vec![num, label];
            spans.extend(a.desc.iter().map(|(t, st)| {
                let st = if chosen && *st == theme.fg3 {
                    theme.fg2
                } else {
                    *st
                };
                Span::new(t.clone(), arm(st))
            }));
            row(spans)
        })
        .collect()
}

/// 板头：问题，Autonomous 停下的命令标 `risky`，右边是还有几个请求在等。
pub fn head_row(theme: &Theme, ask: &Ask, more: usize) -> Element {
    let more = (more > 0).then(|| more_waiting(more));
    head_row_said(
        theme,
        &head(ask),
        risky(ask).then_some("risky"),
        more.as_deref(),
    )
}

/// 右边的「还有几个在等」。
pub fn more_waiting(more: usize) -> String {
    format!("{more} more waiting")
}

/// 同 [`head_row`]，字由调用方给（旧界面的中文界面用）：问题、`risky` 的字、右边的字。
pub fn head_row_said(
    theme: &Theme,
    head: &str,
    risky: Option<&str>,
    waiting: Option<&str>,
) -> Element {
    let mut left = vec![indent(), Span::plain(head.to_owned())];
    if let Some(word) = risky {
        left.push(Span::new(format!("  {word}"), Style::fg(theme.err)));
    }
    waiting_row(theme, left, waiting)
}

fn waiting_row(theme: &Theme, left: Vec<Span>, waiting: Option<&str>) -> Element {
    match waiting {
        None => measured(single_line(left, false)),
        Some(words) => {
            let right = vec![Span::new(words.to_owned(), theme.fg3)];
            measured(first_fit([
                spread(left.clone(), right),
                single_line(left, false),
            ]))
        }
    }
}

// ---- 提问 ----

/// 页的名字，然后是这一页的回答（docs/puffin-tui.md §4.2「提问」）。
pub fn question_rows(
    theme: &Theme,
    ask: &Ask,
    ui: &QuestionUi<'_>,
    typed: bool,
    armed: f64,
    more: usize,
) -> Vec<Element> {
    let more = (more > 0).then(|| more_waiting(more));
    question_rows_said(theme, ask, ui, typed, armed, more.as_deref(), "recommended")
}

/// 同 [`question_rows`]，右边的字与「推荐」的字由调用方给（旧界面的中文界面用）。
pub fn question_rows_said(
    theme: &Theme,
    ask: &Ask,
    ui: &QuestionUi<'_>,
    typed: bool,
    armed: f64,
    waiting: Option<&str>,
    recommended: &str,
) -> Vec<Element> {
    let recommended = format!(" {recommended}");
    let mark = string_width(&recommended);
    let pages = pages_of(ask);
    let mut head_spans = vec![indent()];
    for (i, p) in pages.iter().enumerate() {
        let answered = ui.picks.get(&i).is_some_and(|v| !v.is_empty())
            || ui.own.get(&i).is_some_and(|s| !s.is_empty());
        if i > 0 {
            head_spans.push(Span::new("  ·  ", theme.line));
        }
        let st = if i == ui.page {
            chosen_style(theme)
        } else if answered {
            theme.fg2
        } else {
            theme.fg3
        };
        head_spans.push(Span::new(p.header.clone(), st));
        if answered && i != ui.page {
            head_spans.push(Span::new(" ✓", Style::fg(theme.ok)));
        }
    }
    let page = pages.get(ui.page);
    let options = page.map_or(&[][..], |p| &p.options[..]);
    let multi = page.is_some_and(|p| p.multi);
    let width = |o: &QuestionOption| string_width(&o.label) + if o.recommended { mark } else { 0 };
    let lw = options.iter().map(width).max().unwrap_or(0) + 3;
    let arm = |st: Style| fade_in(theme, st, armed);
    let ticked_page = ui.picks.get(&ui.page);
    let mut rows = vec![waiting_row(theme, head_spans, waiting)];
    for (i, o) in options.iter().enumerate() {
        let chosen = i == ui.choice && !typed;
        let ticked = ticked_page.is_some_and(|v| v.contains(&i));
        let lead = if multi {
            Span::new(
                if ticked { "■ " } else { "□ " },
                arm(if ticked {
                    Style::fg(theme.brand)
                } else {
                    theme.fg3
                }),
            )
        } else {
            Span::new(
                format!("{}  ", i + 1),
                arm(if chosen {
                    Style::fg(theme.brand)
                } else {
                    theme.fg3
                }),
            )
        };
        let mut spans = vec![
            lead,
            Span::new(
                o.label.clone(),
                arm(if chosen {
                    chosen_style(theme)
                } else {
                    theme.fg2
                }),
            ),
        ];
        if o.recommended {
            spans.push(Span::new(recommended.clone(), arm(theme.fg3)));
        }
        spans.push(Span::plain(" ".repeat(lw.saturating_sub(width(o)).max(2))));
        spans.push(Span::new(
            o.description.clone(),
            arm(if chosen { theme.fg2 } else { theme.fg3 }),
        ));
        rows.push(row(spans));
    }
    rows
}

/// 提问的回答，一页一个：选中的回答的值，然后是你自己的话。
pub fn question_pages(ask: &Ask, ui: &QuestionUi<'_>) -> Vec<Vec<String>> {
    pages_of(ask)
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut values: Vec<String> = ui
                .picks
                .get(&i)
                .into_iter()
                .flatten()
                .filter_map(|k| p.options.get(*k).map(|o| o.value.clone()))
                .collect();
            if let Some(own) = ui.own.get(&i).filter(|s| !s.is_empty()) {
                values.push(own.clone());
            }
            values
        })
        .collect()
}

/// 提问的空闲超时在槽里的倒计时（docs/specs/rust-cli/tools-more.md §1.1）：还剩 `seconds` 秒自动继续。
pub fn countdown(seconds: u64) -> String {
    format!("auto-continue in {seconds}s · any key to stay")
}
