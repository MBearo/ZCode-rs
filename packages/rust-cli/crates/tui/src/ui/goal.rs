//! `/goal` 的面板与对话里的 `goal_status` 行（docs/specs/rust-cli/goal.md §6）。
//! 文字有中英文；颜色用终端的 ANSI
//! 色（成功绿、失败红、其余 dim），深浅主题都可读。
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::goal::{GoalStatus, GoalView};

use super::cells::{BULLET, Ctx, bold, dim, red};
use super::i18n::Lang;
use super::text::{truncate_line, wrap_text};

/// 时长只写最高的一个单位。
pub fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// token 数：一千以上写成 `1.2k`。
pub fn tokens(count: u64) -> String {
    if count >= 1000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else {
        count.to_string()
    }
}

fn turns(lang: Lang, count: u32) -> String {
    match lang {
        Lang::Zh => format!("{count} 轮"),
        Lang::En if count == 1 => "1 turn".to_owned(),
        Lang::En => format!("{count} turns"),
    }
}

fn token_count(lang: Lang, count: u64) -> String {
    match lang {
        Lang::Zh => format!("{} token", tokens(count)),
        Lang::En => format!("{} tokens", tokens(count)),
    }
}

/// 达成 / 不可能时括号里的统计：时长 · 轮数 · token。
fn summary(status: &GoalStatus, lang: Lang) -> Vec<String> {
    let mut parts = Vec::new();
    parts.extend(status.duration_ms.map(duration));
    parts.extend(status.iterations.map(|n| turns(lang, n)));
    parts.extend(status.tokens.map(|n| token_count(lang, n)));
    parts
}

/// 对话里的 `goal_status`（设定 / 清除的标记不显示，调用方已过滤）。
pub fn status_lines(status: &GoalStatus, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let zh = ctx.lang == Lang::Zh;
    let (label, style, dot) = if status.failed {
        let label = if zh {
            "目标无法达成"
        } else {
            "Goal could not be achieved"
        };
        (label, red(), red())
    } else if status.met {
        let label = if zh {
            "目标已达成"
        } else {
            "Goal achieved"
        };
        let green = Style::default().fg(Color::Green);
        (label, Style::default(), green)
    } else {
        let label = if zh {
            "目标尚未达成……继续"
        } else {
            "Goal not yet met\u{2026} continuing"
        };
        (label, dim(), dim())
    };
    let mut spans = vec![
        Span::styled(format!("{BULLET} "), dot),
        Span::styled(label.to_owned(), style),
    ];
    let parts = if status.met || status.failed {
        summary(status, ctx.lang)
    } else {
        Vec::new()
    };
    if !parts.is_empty() {
        let joined = parts.join(" \u{b7} ");
        let text = if zh {
            format!("（{joined}）")
        } else {
            format!(" ({joined})")
        };
        spans.push(Span::styled(text, dim()));
    }
    let mut lines = vec![truncate_line(Line::from(spans), ctx.width)];
    if let (true, Some(reason)) = (status.failed, &status.reason) {
        lines.extend(wrap_text(
            reason,
            dim(),
            ctx.width,
            &[Span::raw("  ")],
            &[Span::raw("  ")],
        ));
    }
    lines
}

/// `/goal` 面板（只读，打开时的快照，goal.md D6）。
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    pub title: String,
    pub success: bool,
    pub subtitle: Option<String>,
    /// （标签，内容）；没有标签时只写内容（dim）。
    pub rows: Vec<(Option<String>, String)>,
    pub hint: String,
}

pub fn panel(view: &GoalView, lang: Lang, now_ms: u64) -> Panel {
    let zh = lang == Lang::Zh;
    let goal_label = || Some(if zh { "目标" } else { "Goal" }.to_owned());
    if let Some(active) = &view.active {
        let mut subtitle = vec![if zh {
            format!(
                "已运行 {}",
                duration(now_ms.saturating_sub(active.set_at_ms))
            )
        } else {
            format!(
                "running {}",
                duration(now_ms.saturating_sub(active.set_at_ms))
            )
        }];
        if active.iterations > 0 {
            subtitle.push(turns(lang, active.iterations));
        }
        subtitle.push(token_count(lang, active.tokens));
        let mut rows = vec![(goal_label(), active.condition.clone())];
        if let Some(reason) = &active.last_reason {
            let first = reason.trim().split('\n').next().unwrap_or_default();
            rows.push((
                Some(if zh { "上次检查" } else { "Last check" }.to_owned()),
                first.to_owned(),
            ));
        }
        return Panel {
            title: if zh {
                "◉ 目标进行中"
            } else {
                "◉ Goal active"
            }
            .to_owned(),
            success: false,
            subtitle: Some(subtitle.join(" \u{b7} ")),
            rows,
            hint: if zh {
                "/goal clear 提前结束"
            } else {
                "/goal clear to stop early"
            }
            .to_owned(),
        };
    }
    if let Some(achieved) = &view.achieved {
        let parts = summary(achieved, lang);
        return Panel {
            title: if zh {
                "✓ 目标已达成"
            } else {
                "✓ Goal achieved"
            }
            .to_owned(),
            success: true,
            subtitle: (!parts.is_empty()).then(|| parts.join(" \u{b7} ")),
            rows: vec![(goal_label(), achieved.condition.clone())],
            hint: if zh {
                "/goal <condition> 设定新的目标"
            } else {
                "/goal <condition> to set another"
            }
            .to_owned(),
        };
    }
    Panel {
        title: if zh { "目标" } else { "Goal" }.to_owned(),
        success: false,
        subtitle: None,
        rows: vec![(
            None,
            if zh {
                "没有设定目标"
            } else {
                "No goal set"
            }
            .to_owned(),
        )],
        hint: if zh {
            "/goal <condition> 设定一个目标"
        } else {
            "/goal <condition> to set one"
        }
        .to_owned(),
    }
}

/// 面板的行：第一行是标题（独立页面把它放到页面的标题行）。
pub fn panel_lines(panel: &Panel, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let title = if panel.success {
        Style::default().fg(Color::Green)
    } else {
        bold()
    };
    let mut lines = vec![Line::from(Span::styled(panel.title.clone(), title))];
    if let Some(subtitle) = &panel.subtitle {
        lines.push(Line::from(Span::styled(format!("  {subtitle}"), dim())));
    }
    lines.push(Line::default());
    for (label, value) in &panel.rows {
        let lead = match label {
            Some(label) => vec![Span::raw("  "), Span::styled(format!("{label}: "), dim())],
            None => vec![Span::raw("  ")],
        };
        let style = if label.is_some() {
            Style::default()
        } else {
            dim()
        };
        lines.extend(wrap_text(
            value,
            style,
            ctx.width,
            &lead,
            &[Span::raw("    ")],
        ));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(format!("  {}", panel.hint), dim())));
    lines
}

#[cfg(test)]
#[path = "goal_tests.rs"]
mod tests;
