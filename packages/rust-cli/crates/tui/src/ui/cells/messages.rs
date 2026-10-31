// 改写自 apps/zcode-host/crates/tui/src/ui/cells/messages.rs（feat/rs 79b6691c55）：输入换成视图行的字段。
//! 消息类的块：用户消息、回复、推理、时间线标记、错误与启动头。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::NoticeLevel;

use super::{BULLET, Ctx, bold, dim, red, words};
use crate::ui::i18n::Lang;
use crate::ui::markdown;
use crate::ui::text::{truncate_line, wrap_text};
use crate::view::Marker;

/// 用户消息：首行 `❯ `（dim），续行缩进 2 列；整块用户消息底色，右侧留 1 列。
pub fn user(text: &str, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let block = ctx.palette.prompt_block();
    let first = vec![Span::styled("❯ ", dim())];
    let rest = vec![Span::raw("  ")];
    // 右侧留 1 列，底色不贴边。
    let width = ctx.width.saturating_sub(1);
    wrap_text(text, Style::default(), width, &first, &rest)
        .into_iter()
        .map(|line| line.style(block))
        .collect()
}

/// 回复显示的文字：流式中最后一行没写完（没有换行）时先不显示这一行，免得半个词、半个 markdown 标记闪来闪去。
pub(crate) fn visible(text: &str, streaming: bool) -> &str {
    if streaming {
        text.rfind('\n').map_or("", |end| &text[..end])
    } else {
        text
    }
}

/// 回复：首行 `⏺ `，其余缩进 2 列；markdown，只排 [`visible`] 的部分。
pub fn agent(text: &str, streaming: bool, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let text = visible(text, streaming);
    if text.trim().is_empty() {
        return Vec::new();
    }
    let first = vec![Span::raw(format!("{BULLET} "))];
    let rest = vec![Span::raw("  ")];
    // 流式中按块缓存排版，只重排变了的块（修复线 F6，审查 R5-5）：以前每帧整篇重排，长回复平方增长。
    match ctx.markdown.filter(|_| streaming) {
        Some(memo) => memo.render(
            text,
            ctx.width,
            (first, rest),
            Style::default(),
            ctx.palette,
        ),
        None => markdown::render(text, ctx.width, first, rest, Style::default(), ctx.palette),
    }
}

/// 推理的详细形式：`∴ `（dim）后接全文（dim、斜体），续行缩进 2 列。普通视图里推理并进工作段。
pub fn reasoning(text: &str, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    markdown::render(
        text.trim(),
        ctx.width,
        vec![Span::styled("∴ ", dim())],
        vec![Span::raw("  ")],
        dim().add_modifier(Modifier::ITALIC),
        ctx.palette,
    )
}

/// 时间线标记：压缩 `✻ …`（dim）。
pub fn marker(marker: Marker, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let text = match marker {
        Marker::Compacting => words::compacting(ctx.lang),
        Marker::Compacted => words::compacted(ctx.lang),
    };
    vec![truncate_line(
        Line::from(Span::styled(format!("✻ {text}"), dim())),
        ctx.width,
    )]
}

/// 定时任务的触发行：时间线标记 `✻ …`（dim，tools-more.md §18.3）。
pub fn fire(fire: &crate::view::Fire, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let wakeup = fire.kind == rust_cli_kernel_api::cron::CronFireKind::Loop;
    let fold = fire
        .fold
        .as_ref()
        .map(|fold| (fold.streak, fold.since.as_str()));
    let text = words::fire(ctx.lang, &fire.at, wakeup, fold);
    vec![truncate_line(
        Line::from(Span::styled(format!("✻ {text}"), dim())),
        ctx.width,
    )]
}

/// 系统提示：`⏺ ` 加原文，多行时续行缩进；warning 黄色、info dim（L18，tui.md §4.2）。
pub fn notice(level: NoticeLevel, text: &str, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let style = match level {
        NoticeLevel::Warning => Style::default().fg(Color::Yellow),
        NoticeLevel::Info => dim(),
    };
    wrap_text(
        text,
        style,
        ctx.width,
        &[Span::styled(format!("{BULLET} "), style)],
        &[Span::raw("  ")],
    )
}

/// 跨会话的一行（cross-session.md §2.6）：同伴消息是强调色的 `@ 名字 ›`，折叠时
/// 后面接正文的第一行，展开时正文缩进 2 列；回执是警告色的说明（中英文）。
pub fn peer(row: &crate::view::PeerRow, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    use crate::ui::tokens::{ACCENT_DARK, ACCENT_LIGHT, ACCENT_NAMED};
    match row {
        crate::view::PeerRow::Message { name, body } => {
            let label = ctx
                .palette
                .status_line(ACCENT_DARK, ACCENT_LIGHT, ACCENT_NAMED);
            let head = vec![Span::styled(format!("@ {name} › "), label)];
            if ctx.expanded {
                let mut lines = vec![Line::from(vec![Span::styled(format!("@ {name} ›"), label)])];
                lines.extend(wrap_text(
                    body,
                    Style::default(),
                    ctx.width,
                    &[Span::raw("  ")],
                    &[Span::raw("  ")],
                ));
                lines
            } else {
                let first = body
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default();
                let more = if body.trim().lines().count() > 1 {
                    " …"
                } else {
                    ""
                };
                vec![truncate_line(
                    Line::from([head, vec![Span::raw(format!("{first}{more}"))]].concat()),
                    ctx.width,
                )]
            }
        }
        crate::view::PeerRow::Receipt { status } => notice(
            NoticeLevel::Warning,
            crate::ui::i18n_peers::receipt(ctx.lang, status),
            ctx,
        ),
    }
}

/// `⏺ 信息`（红色）。
/// 紧凑形式下错误文字的上限。
const ERROR_LIMIT: usize = 1000;

/// 错误行（docs/specs/rust-cli/tui-select.md §1）：`⏺ ` 后面是错误文字，按 `宽 − 10` 折行，续行缩进
/// 2 列；紧凑形式超过 1000 个字符时只显示前 1000 个字符加 `…`，下面一行 dim 的 `(ctrl+o to expand)`（中文另配）；详细
/// 形式是全文。
///
/// 缺陷修复（F25）：原来整条错误截到一行宽（`truncate`），Bedrock 这类长的 400 只显示到 `https…`，后半句与请求 ID
/// 看不到也复制不到。
pub fn error_lines(message: &str, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let message = words::turn_error(ctx.lang, message.trim());
    let text = message.as_str();
    let cut = !ctx.expanded && text.chars().count() > ERROR_LIMIT;
    let shown = if cut {
        let mut shown: String = text.chars().take(ERROR_LIMIT).collect();
        shown.push('…');
        shown
    } else {
        text.to_owned()
    };
    let first = vec![Span::styled(format!("{BULLET} "), red())];
    let rest = vec![Span::raw("  ")];
    // 符号占 2 列，文字块宽 `列数 − 10`（至少 10 列）。
    const RESERVED: usize = 8;
    const MIN_WIDTH: usize = 12;
    let width = ctx.width.saturating_sub(RESERVED).max(MIN_WIDTH);
    let mut lines = wrap_text(&shown, red(), width, &first, &rest);
    if cut {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(super::agent::expand_hint(ctx.lang), dim()),
        ]));
    }
    lines
}

/// 启动头：`ZCode v版本`；下一行 `模型 · 目录`（目录太长时从左侧省略）。模型名与目录用 `model` / `path` 的颜色
/// （状态栏的配色，tui.md §7.4，修复线 F26；原来整行 dim）。权限模式为 bypassPermissions 时再加一行
/// `permissions: Full access`（红色）。
pub fn header(mode: PermissionMode, model: &str, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let directory = ctx.workspace_label;
    let lead = if model.is_empty() {
        String::new()
    } else {
        format!("{model} · ")
    };
    let room = ctx
        .width
        .saturating_sub(unicode_width::UnicodeWidthStr::width(lead.as_str()));
    let directory = if unicode_width::UnicodeWidthStr::width(directory) > room && room > 1 {
        let tail: String = directory
            .chars()
            .rev()
            .scan(0, |used, c| {
                *used += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                (*used < room).then_some(c)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        directory.to_owned()
    };
    let mut lines = vec![
        truncate_line(
            Line::from(vec![
                Span::styled("ZCode", bold()),
                Span::styled(format!(" v{}", ctx.version), dim()),
            ]),
            ctx.width,
        ),
        truncate_line(
            Line::from(if model.is_empty() {
                vec![Span::styled(directory, ctx.palette.path())]
            } else {
                vec![
                    Span::styled(model.to_owned(), ctx.palette.model()),
                    super::super::key_hint::separator(ctx.palette),
                    Span::styled(directory, ctx.palette.path()),
                ]
            }),
            ctx.width,
        ),
    ];
    if mode == PermissionMode::BypassPermissions {
        let label = match ctx.lang {
            Lang::En => "permissions: ",
            Lang::Zh => "权限：",
        };
        lines.push(Line::from(vec![
            Span::styled(label, dim()),
            Span::styled(words::full_access(ctx.lang), red()),
        ]));
    }
    lines
}
