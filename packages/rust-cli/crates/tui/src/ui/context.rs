//! `/context` 页面的方格图与类别说明（docs/specs/rust-cli/context-usage.md §5.2；L30 起
//! 是独立页面，tui-panels.md §4）。数据是内核给出的 [`ContextUsage`]；这里只排版。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::context_usage::{
    CategoryKind, ContextUsage, UsageCategory, format_tokens,
};

use super::cells::{self, Ctx};
use super::i18n::Lang;

const FREE: &str = "Free space";

/// 类别的颜色：按主题名映射到终端的基本色（深浅主题下都由终端配色决定）。
fn color(name: &str, ctx: &Ctx<'_>) -> Color {
    match name {
        "System prompt" => Color::Gray,
        "MCP tools" => Color::Cyan,
        "MCP server instructions" => Color::Green,
        "Custom agents" => Color::Blue,
        "Memory files" => ctx.palette.accent,
        "Skills" => Color::Yellow,
        "Messages" => Color::Magenta,
        _ => Color::DarkGray,
    }
}

/// JS `((tokens / window) * 100).toFixed(1)`。
fn percent(tokens: u64, window: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = if window == 0 {
        0.0
    } else {
        tokens as f64 / window as f64 * 100.0
    };
    format!("{:.1}", (value * 10.0).round() / 10.0)
}

/// 一格：类别、颜色与满的程度。
#[derive(Clone)]
struct Square {
    category: String,
    fullness: f64,
}

/// 方格图：窗口 ≥ 1M 时 20 × 10，否则 10 × 10；终端窄于 80 列时 5 × 5。
fn grid(usage: &ContextUsage, width: usize) -> (Vec<Vec<Square>>, usize) {
    let narrow = width < 80;
    let large = usage.raw_max_tokens >= 1_000_000;
    let columns = if narrow {
        5
    } else if large {
        20
    } else {
        10
    };
    let rows = if large {
        10
    } else if narrow {
        5
    } else {
        10
    };
    let total = columns * rows;
    #[allow(clippy::cast_precision_loss)]
    let share = |tokens: u64| tokens as f64 / usage.raw_max_tokens.max(1) as f64 * total as f64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let squares_of = |category: &UsageCategory| {
        let squares = share(category.tokens).round() as usize;
        if category.kind == CategoryKind::Free {
            squares
        } else {
            squares.max(1)
        }
    };
    let shown: Vec<&UsageCategory> = usage
        .categories
        .iter()
        .filter(|category| category.kind != CategoryKind::Deferred)
        .collect();
    let fill = |category: &UsageCategory, out: &mut Vec<Square>| {
        let exact = share(category.tokens);
        #[allow(clippy::cast_precision_loss)]
        let whole = exact.floor() as usize;
        for index in 0..squares_of(category) {
            #[allow(clippy::cast_precision_loss)]
            let partial = exact - whole as f64;
            let fullness = if index == whole && partial > 0.0 {
                partial
            } else {
                1.0
            };
            if out.len() < total {
                out.push(Square {
                    category: category.name.clone(),
                    fullness,
                });
            }
        }
    };
    let mut squares = Vec::new();
    for category in shown
        .iter()
        .filter(|c| matches!(c.kind, CategoryKind::Used))
    {
        fill(category, &mut squares);
    }
    let buffer = shown.iter().find(|c| c.kind == CategoryKind::Buffer);
    let reserved = buffer.map_or(0, |buffer| squares_of(buffer));
    while squares.len() < total.saturating_sub(reserved) {
        squares.push(Square {
            category: FREE.to_owned(),
            fullness: 1.0,
        });
    }
    if let Some(buffer) = buffer {
        fill(buffer, &mut squares);
    }
    (
        squares.chunks(columns).map(<[Square]>::to_vec).collect(),
        columns,
    )
}

fn square_span(square: &Square, usage: &ContextUsage, ctx: &Ctx<'_>) -> Span<'static> {
    let buffer = usage
        .categories
        .iter()
        .any(|c| c.kind == CategoryKind::Buffer && c.name == square.category);
    if square.category == FREE {
        return Span::styled("\u{26F6} ", cells::dim());
    }
    let symbol = if buffer {
        "\u{26DD} "
    } else if square.fullness >= 0.7 {
        "\u{26C1} "
    } else {
        "\u{26C0} "
    };
    Span::styled(symbol, Style::default().fg(color(&square.category, ctx)))
}

/// 右边的说明。
fn legend(usage: &ContextUsage, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let dim = cells::dim();
    let window = usage.raw_max_tokens;
    let (tokens, heading) = match ctx.lang {
        Lang::En => ("tokens", "Estimated usage by category"),
        Lang::Zh => ("tokens", "按类别估算"),
    };
    let mut lines = vec![
        Line::from(Span::styled(usage.model.clone(), dim)),
        Line::from(Span::styled(
            format!(
                "{}/{} {tokens} ({}%)",
                format_tokens(usage.total_tokens),
                format_tokens(window),
                usage.percentage
            ),
            dim,
        )),
        Line::default(),
        Line::from(Span::styled(heading, dim.add_modifier(Modifier::ITALIC))),
    ];
    // 顺序：使用中的类别，然后空闲，最后缓冲。
    let order = [CategoryKind::Used, CategoryKind::Free, CategoryKind::Buffer];
    for kind in order {
        for category in usage
            .categories
            .iter()
            .filter(|category| category.kind == kind && category.tokens > 0)
        {
            let value = format_tokens(category.tokens);
            let share = percent(category.tokens, window);
            lines.push(match kind {
                CategoryKind::Free => Line::from(vec![
                    Span::styled("\u{26F6}", dim),
                    Span::raw(format!(" {FREE}: ")),
                    Span::styled(format!("{value} ({share}%)"), dim),
                ]),
                CategoryKind::Buffer => Line::from(vec![
                    Span::styled("\u{26DD}", Style::default().fg(color(&category.name, ctx))),
                    Span::styled(
                        format!(" {}: {value} {tokens} ({share}%)", category.name),
                        dim,
                    ),
                ]),
                _ => Line::from(vec![
                    Span::styled("\u{26C1}", Style::default().fg(color(&category.name, ctx))),
                    Span::raw(format!(" {}: ", category.name)),
                    Span::styled(format!("{value} {tokens} ({share}%)"), dim),
                ]),
            });
        }
    }
    lines
}

/// 页面标题（`/context` 独立页面，tui-panels.md §4.2）。
pub fn title(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Context Usage",
        Lang::Zh => "上下文用量",
    }
}

/// 超限说明、方格图与说明并排（放不下时说明在下面）。明细与建议见 `context_page.rs`。
pub fn chart(usage: &ContextUsage, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if let Some(over) = usage.over_limit() {
        out.push(Line::from(Span::styled(
            over,
            Style::default().fg(Color::Red),
        )));
    }
    let (rows, columns) = grid(usage, ctx.width);
    let legend = legend(usage, ctx);
    let grid_width = columns * 2 + 1;
    let side_by_side = ctx.width >= grid_width + 2 + 40;
    let mut grid_lines: Vec<Line<'static>> = rows
        .iter()
        .map(|row| {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(row.iter().map(|square| square_span(square, usage, ctx)));
            Line::from(spans)
        })
        .collect();
    if side_by_side {
        let height = grid_lines.len().max(legend.len());
        grid_lines.resize(height, Line::default());
        for (index, mut line) in grid_lines.into_iter().enumerate() {
            let used = line.width();
            line.spans
                .push(Span::raw(" ".repeat(grid_width.saturating_sub(used) + 2)));
            if let Some(right) = legend.get(index) {
                line.spans.extend(right.spans.clone());
            }
            out.push(line);
        }
    } else {
        out.extend(grid_lines);
        out.push(Line::default());
        out.extend(legend);
    }
    out
}
