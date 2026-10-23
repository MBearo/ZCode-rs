// 改写自 apps/zcode-host/crates/tui/src/ui/interaction_view_question.rs（feat/rs 79b6691c55）：输入换成
// `InteractionRequest`，选项的值就是标签。
//! 提问的排版（docs/specs/rust-tui.md §8.5）：多于一题时第一行是标签页 `← ☐ 题头  ✔ Submit →`（已答
//! `☒`，当前页反色）；问题粗体；选项 `› 1. 标签`（选择列表组件，F26）下面 dim 的说明；最后一行是 `Other` 输入。确认页列出
//! 每题的答案。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::orchestration::InteractionRequest;

use super::{View, bold, dim, frame, input, option};
use crate::ui::i18n::Lang;
use crate::ui::interaction::{Interaction, Question, questions};
use crate::ui::text::{truncate_line, wrap_text};

impl Interaction {
    pub(super) fn question_lines(
        &self,
        request: &InteractionRequest,
        view: &View<'_>,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let list = questions(request);
        let zh = view.lang == Lang::Zh;
        let mut lines = frame("", view);
        lines.pop();
        if list.len() > 1 {
            lines.push(self.tabs(&list, view));
        } else {
            lines.push(Line::default());
        }
        lines.push(Line::default());
        let options = match list.get(self.question) {
            Some(question) => self.question_body(question, &mut lines, view),
            None => self.review(&list, &mut lines, view),
        };
        lines.push(Line::default());
        let text = match (zh, list.len() > 1) {
            (false, true) => {
                "{Enter} to select · {↑/↓} to navigate · {Tab} to switch questions · {Esc} to cancel"
            }
            (false, false) => "{Enter} to select · {↑/↓} to navigate · {Esc} to cancel",
            (true, true) => "{enter} 选择 · {↑/↓} 移动 · {tab} 切换问题 · {esc} 取消",
            (true, false) => "{enter} 选择 · {↑/↓} 移动 · {esc} 取消",
        };
        lines.push(crate::ui::interaction_view::hint(text, view));
        (lines, options)
    }

    /// 标签页：`← ☐ 题头1  ☒ 题头2  ✔ Submit →`，当前页反色。
    fn tabs(&self, list: &[Question], view: &View<'_>) -> Line<'static> {
        let mut spans = vec![Span::styled("  ← ", dim())];
        for (index, question) in list.iter().enumerate() {
            let answered = !self.drafts[index].answers().is_empty();
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
    ) -> Vec<(usize, usize)> {
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
            let mut spans = Vec::new();
            if question.multi {
                let mark = if draft.values.contains(label) {
                    "[✔] "
                } else {
                    "[ ] "
                };
                spans.push(Span::raw(mark));
            }
            spans.push(Span::raw(label.clone()));
            options.push((lines.len(), index));
            lines.push(option(self.cursor == index, index + 1, spans, view));
            if !description.trim().is_empty() {
                lines.extend(wrap_text(
                    description.trim(),
                    dim(),
                    view.width,
                    &[Span::raw("       ")],
                    &[Span::raw("       ")],
                ));
            }
        }
        let other = question.options.len();
        if crate::ui::interaction::question::rows(question) > other {
            let placeholder = match view.lang {
                Lang::En => "Type something.",
                Lang::Zh => "输入其他回答。",
            };
            options.push((lines.len(), other));
            lines.push(option(
                self.cursor == other,
                other + 1,
                input(&draft.notes, placeholder),
                view,
            ));
        }
        options
    }

    /// 确认页：每题一行 `· 问题 → 答案`（绿色），有未答的题时黄色提示，然后提交或取消。
    fn review(
        &self,
        list: &[Question],
        lines: &mut Vec<Line<'static>>,
        view: &View<'_>,
    ) -> Vec<(usize, usize)> {
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
        let green = Style::default().fg(Color::Green);
        let mut missing = false;
        for (question, draft) in list.iter().zip(&self.drafts) {
            let answers = draft.answers();
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
                Style::default().fg(Color::Yellow),
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
        lines.push(option(self.cursor == 0, 1, vec![Span::raw(submit)], view));
        lines.push(option(self.cursor == 1, 2, vec![Span::raw(cancel)], view));
        vec![(start, 0), (start + 1, 1)]
    }
}
