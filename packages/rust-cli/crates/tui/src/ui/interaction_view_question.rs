// 改写自 apps/zcode-host/crates/tui/src/ui/interaction_view_question.rs（feat/rs 79b6691c55）：输入换成
// `InteractionRequest`，选项的值就是标签。
//! 提问的排版（docs/specs/rust-tui.md §8.5；选项的画法见 docs/specs/rust-cli/tui.md §7.6，工作线 F74）：多于一题时
//! 第一行是标签页 `← ☐ 题头  ✔ Submit →`（已答 `☒`，当前页反色）；问题粗体；选项 `❯ 1. 标签` 下面 dim 的说明；最后
//! 一行是 `Other` 输入。确认页列出每题的答案。
//!
//! 选项不用选择列表组件的底色条（F74 以前用）：底色条盖在“其他”行的输入上，看不到光标。焦点行只有强调色的 `❯ `
//! 与名字，序号 dim（行首与输入行的画法在 `interaction_view.rs`，工作线 F76 起权限询问与计划审批共用）；多选勾上的
//! `[✔]` 与单选的已选项是成功色。焦点在“其他”行时 [`Interaction::cursor`] 给出终端光标的位置。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::orchestration::InteractionRequest;

use super::{At, Rows, View, bold, checkbox, dim, frame, input, input_row, prefix};
use crate::ui::i18n::Lang;
use crate::ui::interaction::{Draft, Interaction, Question, questions};
use crate::ui::text::{truncate_line, wrap_text};

/// 名字的样式：单选的已选项是成功色，焦点行是强调色，其余不上色。
fn name_style(focused: bool, chosen: bool, view: &View<'_>) -> Style {
    if chosen {
        view.palette.success()
    } else if focused {
        Style::default().fg(view.palette.accent)
    } else {
        Style::default()
    }
}

/// 已选项后面的 ` ✔`。
fn tick(view: &View<'_>) -> Span<'static> {
    Span::styled(" ✔", view.palette.success())
}

/// 一行选项（放不下时截断）。
fn choice(
    focused: bool,
    number: usize,
    label: Vec<Span<'static>>,
    view: &View<'_>,
) -> Line<'static> {
    let mut spans = prefix(focused, number, view);
    spans.extend(label);
    truncate_line(Line::from(spans), view.width)
}

/// 一道题的答案：预览题只有选中的值（它的输入是备注，工作线 F72），其余题是选中的值加 `Other`。
fn answers_of(question: &Question, draft: &crate::ui::interaction::Draft) -> Vec<String> {
    if question.preview_mode() {
        draft.values.clone()
    } else {
        draft.answers()
    }
}

impl Interaction {
    /// 排好的提问：行、选项所在的行与终端光标。
    pub(super) fn question_layout(
        &self,
        request: &InteractionRequest,
        view: &View<'_>,
    ) -> (Vec<Line<'static>>, Rows, At) {
        let list = questions(request);
        let zh = view.lang == Lang::Zh;
        let mut lines = frame("", view);
        lines.pop();
        // 单选且有预览的题（工作线 F72，tools-more.md §1.1）：只有一题时也有标签行。
        let preview = list.get(self.question).is_some_and(Question::preview_mode);
        match list.as_slice() {
            [_, _, ..] => lines.push(self.tabs(&list, view)),
            [only] if preview => lines.push(self.single_tab(only)),
            _ => lines.push(Line::default()),
        }
        lines.push(Line::default());
        // 预览题不放终端光标：备注一行在预览框下面，写备注时光标的位置没有做（tools-more.md §1.2）。
        let (options, cursor) = match list.get(self.question) {
            Some(question) if preview => (self.preview_body(question, &mut lines, view), None),
            Some(question) => self.question_body(question, &mut lines, view),
            None => (self.review(&list, &mut lines, view), None),
        };
        lines.push(Line::default());
        let text = match (zh, list.len() > 1, preview) {
            (false, true, true) => {
                "{Enter} to select · {↑/↓} to navigate · {n} to add notes · {Tab} to switch questions · {Esc} to cancel"
            }
            (false, false, true) => {
                "{Enter} to select · {↑/↓} to navigate · {n} to add notes · {Esc} to cancel"
            }
            (false, true, false) => {
                "{Enter} to select · {↑/↓} to navigate · {Tab} to switch questions · {Esc} to cancel"
            }
            (false, false, false) => "{Enter} to select · {↑/↓} to navigate · {Esc} to cancel",
            (true, true, true) => {
                "{enter} 选择 · {↑/↓} 移动 · {n} 写备注 · {tab} 切换问题 · {esc} 取消"
            }
            (true, false, true) => "{enter} 选择 · {↑/↓} 移动 · {n} 写备注 · {esc} 取消",
            (true, true, false) => "{enter} 选择 · {↑/↓} 移动 · {tab} 切换问题 · {esc} 取消",
            (true, false, false) => "{enter} 选择 · {↑/↓} 移动 · {esc} 取消",
        };
        lines.push(crate::ui::interaction_view::hint(text, view));
        // 空闲倒计时（tools-more.md §1.1）：右对齐的一行 dim 文字。
        if let Some(seconds) = self.countdown {
            let text = match view.lang {
                Lang::En => format!("auto-continue in {seconds}s \u{b7} any key to stay"),
                Lang::Zh => format!("{seconds} 秒后自动继续 \u{b7} 按任意键留下"),
            };
            let pad = view
                .width
                .saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()));
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(text, dim()),
            ]));
        }
        (lines, options, cursor)
    }

    /// 标签页：`← ☐ 题头1  ☒ 题头2  ✔ Submit →`，当前页反色。
    fn tabs(&self, list: &[Question], view: &View<'_>) -> Line<'static> {
        let mut spans = vec![Span::styled("  ← ", dim())];
        for (index, question) in list.iter().enumerate() {
            let answered = !answers_of(question, &self.drafts[index]).is_empty();
            let mark = if answered { "☒" } else { "☐" };
            let header = if question.header.is_empty() {
                format!("Q{}", index + 1)
            } else {
                question.header.clone()
            };
            let style = if index == self.question {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            spans.push(Span::styled(format!("{mark} {header}"), style));
            spans.push(Span::raw("  "));
        }
        let submit = match view.lang {
            Lang::En => "✔ Submit",
            Lang::Zh => "✔ 提交",
        };
        let style = if self.question == list.len() {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(submit, style));
        spans.push(Span::styled(" →", dim()));
        truncate_line(Line::from(spans), view.width)
    }

    fn question_body(
        &self,
        question: &Question,
        lines: &mut Vec<Line<'static>>,
        view: &View<'_>,
    ) -> (Rows, At) {
        let draft = &self.drafts[self.question];
        if !question.text.trim().is_empty() {
            lines.extend(wrap_text(
                question.text.trim(),
                bold(),
                view.width,
                &[Span::raw("  ")],
                &[Span::raw("  ")],
            ));
            lines.push(Line::default());
        }
        let mut options = Vec::new();
        for (index, (label, description)) in question.options.iter().enumerate() {
            let focused = self.cursor == index;
            let picked = draft.values.contains(label);
            // 单选回到答过的题：已选项是成功色、后面 ` ✔`；多选看方框。
            let chosen = !question.multi && picked;
            let mut spans = Vec::new();
            if question.multi {
                spans.extend(checkbox(picked, view));
            }
            spans.push(Span::styled(
                label.clone(),
                name_style(focused, chosen, view),
            ));
            if chosen {
                spans.push(tick(view));
            }
            options.push((lines.len(), index));
            lines.push(choice(focused, index + 1, spans, view));
            if !description.trim().is_empty() {
                // 单选的说明随名字上色；多选的说明不上色。都 dim。
                let style = if question.multi {
                    dim()
                } else {
                    name_style(focused, chosen, view).add_modifier(Modifier::DIM)
                };
                lines.extend(wrap_text(
                    description.trim(),
                    style,
                    view.width,
                    &[Span::raw("       ")],
                    &[Span::raw("       ")],
                ));
            }
        }
        let other = question.options.len();
        let mut cursor = None;
        if crate::ui::interaction::question::rows(question) > other {
            options.push((lines.len(), other));
            cursor = self.other_row(question, draft, lines, view);
        }
        (options, cursor)
    }

    /// “其他”行：输入按宽度折行，续行与文字对齐。多选时前面有方框（文字不为空就勾上，与回答的规则相同）；
    /// 单选的回答是这段文字且焦点不在这一行时，后面 ` ✔`。焦点在这一行时返回终端光标：文字末尾，空的时候在占位
    /// 文字的第一个字上；放不下时停在这一行的最后一格。
    fn other_row(
        &self,
        question: &Question,
        draft: &Draft,
        lines: &mut Vec<Line<'static>>,
        view: &View<'_>,
    ) -> At {
        let other = question.options.len();
        let focused = self.cursor == other;
        let typed = !draft.notes.trim().is_empty();
        let mut first = prefix(focused, other + 1, view);
        if question.multi {
            first.extend(checkbox(typed, view));
        }
        let placeholder = match view.lang {
            Lang::En => "Type something.",
            Lang::Zh => "输入其他回答。",
        };
        let mut content = input(&draft.notes, placeholder);
        if !question.multi && typed && draft.values.is_empty() && !focused {
            content.push(tick(view));
        }
        let (rows, at) = input_row(first, content, draft.notes.is_empty(), lines.len(), view);
        lines.extend(rows);
        focused.then_some(at)
    }

    /// 确认页：每题一行 `· 问题 → 答案`（绿色），有未答的题时黄色提示，然后提交或取消。
    fn review(&self, list: &[Question], lines: &mut Vec<Line<'static>>, view: &View<'_>) -> Rows {
        let zh = view.lang == Lang::Zh;
        lines.push(Line::from(Span::styled(
            format!(
                "  {}",
                if zh {
                    "检查你的回答"
                } else {
                    "Review your answers"
                }
            ),
            bold(),
        )));
        lines.push(Line::default());
        let green = view.palette.success();
        let mut missing = false;
        for (question, draft) in list.iter().zip(&self.drafts) {
            let answers = answers_of(question, draft);
            if answers.is_empty() {
                missing = true;
                continue;
            }
            lines.push(truncate_line(
                Line::from(vec![
                    Span::styled("   · ", dim()),
                    Span::raw(format!("{} ", question.text.trim())),
                    Span::styled(format!("→ {}", answers.join(", ")), green),
                ]),
                view.width,
            ));
        }
        if missing {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    if zh {
                        "还有问题没有回答"
                    } else {
                        "You have not answered all questions"
                    }
                ),
                Style::default().fg(ratatui::style::Color::Yellow),
            )));
        }
        lines.push(Line::default());
        lines.push(Line::from(format!(
            "  {}",
            if zh {
                "要提交回答吗？"
            } else {
                "Ready to submit your answers?"
            }
        )));
        let start = lines.len();
        let (submit, cancel) = if zh {
            ("提交回答", "取消")
        } else {
            ("Submit answers", "Cancel")
        };
        for (index, text) in [submit, cancel].into_iter().enumerate() {
            let focused = self.cursor == index;
            let label = vec![Span::styled(text, name_style(focused, false, view))];
            lines.push(choice(focused, index + 1, label, view));
        }
        vec![(start, 0), (start + 1, 1)]
    }
}

#[cfg(test)]
#[path = "interaction_view_question_tests.rs"]
mod tests;
