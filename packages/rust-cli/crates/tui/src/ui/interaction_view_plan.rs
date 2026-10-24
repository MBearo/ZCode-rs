// 改写自 apps/zcode-host/crates/tui/src/ui/interaction_view_plan.rs（feat/rs 79b6691c55）：排版按
// ExitPlanMode 的界面，选项按 Node 的计划审批（修复线 F20，docs/specs/rust-cli/tui.md §5.2）。
//! 计划审批的排版：`Ready to code?`，虚线之间是计划（最多 12 行），然后 1. 批准（退出 plan，权限模式不变）；
//! 2. 继续计划（这一行直接是输入）。
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{At, PREVIEW, Rows, View, dashed, dim, frame, input, input_row, named, option, prefix};
use crate::ui::i18n::Lang;
use crate::ui::interaction::Interaction;
use crate::ui::markdown;

impl Interaction {
    pub(super) fn plan_lines(&self, plan: &str, view: &View<'_>) -> (Vec<Line<'static>>, Rows, At) {
        let zh = view.lang == Lang::Zh;
        let mut lines = frame(
            if zh {
                "可以开始写代码了吗？"
            } else {
                "Ready to code?"
            },
            view,
        );
        lines.push(Line::default());
        lines.push(Line::from(format!(
            "  {}",
            if zh {
                "ZCode 的计划："
            } else {
                "Here is ZCode's plan:"
            }
        )));
        lines.push(dashed(view.width));
        let mut body = markdown::render(
            plan,
            view.width,
            vec![Span::raw("    ")],
            vec![Span::raw("    ")],
            Style::default(),
            view.palette,
        );
        if body.len() > PREVIEW {
            let hidden = body.len() - PREVIEW;
            body.truncate(PREVIEW);
            body.push(Line::from(Span::styled(
                format!("    … +{hidden} lines"),
                dim(),
            )));
        }
        lines.extend(body);
        lines.push(dashed(view.width));
        lines.push(Line::default());
        lines.push(Line::from(format!(
            "  {}",
            if zh {
                "要继续吗？"
            } else {
                "Would you like to proceed?"
            }
        )));
        // 修复线 F20：plan 是独立开关，批准只关掉它（两种批准各换一个权限模式的做法不再提供）。
        let labels = if zh {
            ["是，退出计划模式", "否，继续计划"]
        } else {
            ["Yes, exit plan mode", "No, keep planning"]
        };
        let last = labels.len() - 1;
        let mut options = Vec::new();
        let mut cursor = None;
        for (index, label) in labels.into_iter().enumerate() {
            let focused = self.cursor == index;
            options.push((lines.len(), index));
            // 焦点在“继续计划”时这一行就是输入，显示光标（工作线 F76，tui.md §7.7）。
            if index == last && focused {
                let placeholder = if zh {
                    "告诉 ZCode 要改什么"
                } else {
                    "Tell ZCode what to change"
                };
                let first = prefix(true, index + 1, view);
                let content = input(&self.text, placeholder);
                let (rows, at) = input_row(first, content, self.text.is_empty(), lines.len(), view);
                lines.extend(rows);
                cursor = Some(at);
            } else {
                lines.push(option(
                    focused,
                    index + 1,
                    vec![named(label, focused, view)],
                    view,
                ));
            }
        }
        lines.push(Line::default());
        let text = if zh {
            "{enter} 选择 · {esc} 继续计划"
        } else {
            "{Enter} to select · {Esc} to keep planning"
        };
        lines.push(crate::ui::interaction_view::hint(text, view));
        (lines, options, cursor)
    }
}
