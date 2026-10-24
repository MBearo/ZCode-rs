// 改写自 apps/zcode-host/crates/tui/src/ui/footer.rs（feat/rs 79b6691c55）：权限模式换成内核的模式；
// 去掉子代理、后台 shell、任务清单与回退浏览的提示；上下文提示来自 `ContextNotice` 事件。
//! 输入框下面的一行（docs/specs/rust-tui.md §8.4）：footer 与 `?` 快捷键帮助。footer 左侧是权限模式
//! 与一条提示，右侧是模型与剩余上下文。提示经按键提示组件画（按键亮色粗体、说明灰色），模型名用 `model`
//! 的颜色（docs/specs/rust-cli/tui.md §7.4，修复线 F26，用户决定；原来都是 dim）。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::permission::PermissionMode;
use unicode_width::UnicodeWidthStr;

use super::app::App;
use super::i18n::{self, Lang};
use super::mouse::Target;
use super::text::truncate_line;

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// 提示模板：`{键}` 标出按键名（模板里的词序中英文都能放），经按键提示组件画（tui.md §7.4）。
pub fn hint(template: &str, palette: &super::palette::Palette) -> Vec<Span<'static>> {
    super::key_hint::spans(template, palette)
}

/// 权限模式名称的颜色（tui.md §7，工作线 F76，用户决定 2026-10-23“照 App 的配色”）：变更前确认是普通文字色，
/// 自主模式是 App 的 icon-blue，完全访问是 App 的 warning（以前分别是 dim、黄、红）；`/mode` 选择器也用它。
pub fn mode_style(mode: PermissionMode, palette: &super::palette::Palette) -> Style {
    match mode {
        PermissionMode::Default => Style::default(),
        PermissionMode::Guarded => palette.icon_blue(),
        PermissionMode::BypassPermissions => palette.warning(),
        PermissionMode::AcceptEdits => Style::default().fg(Color::Magenta),
        PermissionMode::Plan => Style::default().fg(Color::Cyan),
        PermissionMode::Auto | PermissionMode::DontAsk => Style::default().fg(Color::Yellow),
    }
}

/// 权限模式的指示：符号、名称与颜色（docs/specs/rust-cli/tui.md §7）。
pub fn mode_indicator(
    lang: Lang,
    mode: PermissionMode,
    palette: &super::palette::Palette,
) -> (String, Style) {
    let label = i18n::mode_label(lang, mode);
    // 变更前确认与计划模式是暂停图标，其余是 `⏵⏵`。
    let glyph = match mode {
        PermissionMode::Default | PermissionMode::Plan => "⏸",
        _ => "⏵⏵",
    };
    (format!("{glyph} {label}"), mode_style(mode, palette))
}

/// plan 开着时在权限模式前面加 `⏸ Plan mode · `（修复线 F20，tui.md §7）：plan 是独立开关，shift+tab 改的是
/// 后面的权限模式。
fn mode_spans(app: &App) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if app.view.plan() {
        let (label, style) = mode_indicator(app.lang, PermissionMode::Plan, &app.palette);
        spans.push(Span::styled(label, style));
        spans.push(super::key_hint::separator(&app.palette));
    }
    let (label, style) = mode_indicator(app.lang, app.view.mode(), &app.palette);
    spans.push(Span::styled(label, style));
    // 用户决定（2026-10-20）：不显示 `(shift+tab to cycle)` / `（shift+tab 切换）`（tui.md §7 登记）。
    spans
}

/// footer 左侧放不下时省到哪一步：先省 `? for shortcuts`，再省 `/tasks to see subagents`（修复线 F30，
/// tui-panels.md §1.4）；右侧的统计在这之后才省（F27 的顺序不变）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Left {
    Full,
    NoShortcuts,
    Compact,
}

/// 左侧：搜索历史、选区、子代理列表、回退浏览时各有专用提示；其余时候是权限模式，后面用 ` · ` 接
/// 按状态给的一条提示。按 `left` 省掉 `? for shortcuts` 与 `/tasks` 的提示。
fn footer_left(app: &App, left: Left) -> Vec<Span<'static>> {
    let compact = left >= Left::NoShortcuts;
    let strings = app.strings;
    if let Some((query, _)) = &app.composer.search {
        let missing = !query.is_empty() && app.composer.search_match().is_none();
        let label = match (app.lang, missing) {
            (Lang::En, false) => "search prompts: ",
            (Lang::En, true) => "no matching prompt: ",
            (Lang::Zh, false) => "搜索输入：",
            (Lang::Zh, true) => "没有匹配的输入：",
        };
        return vec![Span::styled(label, dim()), Span::raw(query.clone())];
    }
    let palette = &app.palette;
    // 快捷键面板打开时（面板在输入框上方，tui.md §7.4）。
    if matches!(app.overlay, Some(super::app::Overlay::Shortcuts)) {
        return hint(
            match app.lang {
                Lang::En => "{esc} to close",
                Lang::Zh => "{esc} 关闭",
            },
            palette,
        );
    }
    // 鼠标选区不给提示（F25，2026-10-19 用户决定：松开即复制，不显示原来的 `ctrl+c copy · enter copy & follow ·
    // esc clear`）。
    if app.composer.area.selection().is_some() {
        return hint(strings.composer_selection_hint, palette);
    }
    // 子代理列表获得焦点（tui-panels.md §1.3）。
    if let Some(text) = app.agents_focus_hint(std::time::Instant::now()) {
        return hint(text, palette);
    }
    // 临时提示（连按的提示、粘贴、ctrl+r 等）到时之前替换整个左侧；按键提示经组件画，其余按原文。
    if let Some(notice) = app.live_notice() {
        return if notice.keyed {
            hint(&notice.text, palette)
        } else {
            vec![Span::styled(notice.text.clone(), palette.secondary())]
        };
    }
    // 后台任务的 pill 与任务清单的提示（tui-display.md §2.2、§2.3）。
    let pill = super::manage::pill(&app.background, app.lang);
    let pill_focused = app.focus == Some(super::agents::Focus::Pill);
    let tip = if pill_focused {
        hint(super::i18n_panels::pill_hint(app.lang), palette)
    } else if app.scroll > 0 {
        hint(strings.esc_latest, palette)
    } else if app.running() {
        hint(strings.interrupt_hint, palette)
    } else if pill.is_some() {
        hint(
            match app.lang {
                Lang::En => "{↓} to manage",
                Lang::Zh => "{↓} 管理",
            },
            palette,
        )
    } else if let Some(text) = super::tasks::toggle_hint(app) {
        hint(text, palette)
    } else if app.composer.is_empty() {
        // `← for projects · ? for shortcuts`（F29，原来是 `← for agents`）；放不下时先省掉 `? for shortcuts`。
        hint(super::i18n_projects::empty_hint(app.lang, compact), palette)
    } else {
        Vec::new()
    };
    let mut spans = mode_spans(app);
    if let Some(pill) = pill {
        // 选中时反色（tui-panels.md §3.1）。
        let style = if pill_focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            dim()
        };
        spans.push(super::key_hint::separator(palette));
        spans.push(Span::styled(pill, style));
    }
    // 有隐藏的已完成后台代理时指向 `/tasks`（修复线 F30，tui-panels.md §1.4）；放不下时在
    // `? for shortcuts` 之后、右侧的统计之前省掉。
    if left < Left::Compact && app.hidden_agents(std::time::Instant::now()) {
        spans.push(super::key_hint::separator(palette));
        spans.extend(hint(super::i18n_panels::tasks_hint(app.lang), palette));
    }
    if !tip.is_empty() {
        spans.push(super::key_hint::separator(palette));
        spans.extend(tip);
    }
    spans
}

/// 右侧“上下文快满”的提示（compact.md §2.1）：自动压缩关闭时是红色的 `Context low …`，其余 dim。
fn context_span(app: &App) -> Option<Span<'static>> {
    let notice = app.view.context_notice();
    let text = i18n::context_notice(app.lang, notice)?;
    // 颜色用命名的记号（tui.md §7.4）：快满时红色，其余是次要文字（原来是 dim）。
    let style = app
        .palette
        .context_notice(matches!(notice, ContextNotice::Low { .. }));
    Some(Span::styled(text, style))
}

/// footer：左侧见 [`footer_left`]；右侧 `模型 档位 · N tok/s · ctx N% · cache N% · <上下文提示>`（请求统计见
/// [`super::footer_stats`]，tui-display.md §2.6）。放不下时依次省掉 `? for shortcuts`、cache、TPS、ctx、上下文提示、
/// 模型，最后截断左侧。返回行与可点击的提示：（起始列，宽度，目标）。
pub fn footer(app: &App, width: usize) -> (Line<'static>, Vec<(usize, usize, Target)>) {
    let context = context_span(app);
    // 模型名用 `model` 的颜色（Codex 状态栏的 Model，tui.md §7.4）。
    let model: Vec<Span<'static>> = app
        .model_label()
        .into_iter()
        .map(|m| Span::styled(m, app.palette.model()))
        .collect();
    // F27：输出速度、上下文占用与缓存命中率（以前速度一直是空的，tui.md §9）。
    let stats = super::footer_stats::items(app);
    let joined = |parts: &[Option<&Span<'static>>]| {
        let mut spans = model.clone();
        for part in parts.iter().flatten() {
            if !spans.is_empty() {
                spans.push(super::key_hint::separator(&app.palette));
            }
            spans.push((*part).clone());
        }
        spans
    };
    let (tps, ctx, cache) = (stats.tps.as_ref(), stats.ctx.as_ref(), stats.cache.as_ref());
    let full = joined(&[tps, ctx, cache, context.as_ref()]);
    let without_cache = joined(&[tps, ctx, context.as_ref()]);
    let without_tps = joined(&[ctx, context.as_ref()]);
    let without_ctx = joined(&[context.as_ref()]);
    let width_of = |spans: &[Span<'static>]| spans.iter().map(|s| s.content.width()).sum::<usize>();
    // 只有完整的右侧才配完整的左侧：`? for shortcuts` 省掉之后不再回来（F27 之前右侧省到只剩模型名时，较短的右侧
    // 让完整的左侧又放得下，`? for shortcuts` 会重新出现）。`/tasks` 的提示（F30）排在它之后、统计之前。
    let candidates = [
        (Left::Full, &full),
        (Left::NoShortcuts, &full),
        (Left::Compact, &full),
        (Left::Compact, &without_cache),
        (Left::Compact, &without_tps),
        (Left::Compact, &without_ctx),
        (Left::Compact, &model),
    ];
    let chosen = candidates.iter().find_map(|(level, right)| {
        let mut left = vec![Span::raw("  ")];
        left.extend(footer_left(app, *level));
        let (l, r) = (width_of(&left), width_of(right));
        (r > 0 && l + r + 3 <= width).then(|| (left, (*right).clone()))
    });
    let spans = match chosen {
        Some((mut left, right)) => {
            let used = width_of(&left) + width_of(&right);
            left.push(Span::raw(" ".repeat(width - used - 2)));
            left.extend(right);
            left
        }
        None => {
            let mut left = vec![Span::raw("  ")];
            let full_left = footer_left(app, Left::Full);
            let fits = width_of(&full_left) + 2 <= width;
            left.extend(if fits {
                full_left
            } else {
                footer_left(app, Left::Compact)
            });
            left
        }
    };
    let line = truncate_line(Line::from(spans), width);
    let plain = super::text::plain(&line);
    let mut targets = Vec::new();
    let agents = super::i18n_projects::for_projects(app.lang);
    let pill = super::manage::pill(&app.background, app.lang).unwrap_or_default();
    for (text, target) in [
        (app.strings.for_shortcuts, Target::Shortcuts),
        (agents, Target::Agents),
        (pill.as_str(), Target::Pill),
    ]
    .into_iter()
    .filter(|(text, _)| !text.is_empty())
    .map(|(template, target)| (super::key_hint::plain(template), target))
    {
        if let Some(at) = plain.find(&text) {
            targets.push((plain[..at].width(), text.width(), target));
        }
    }
    (line, targets)
}

/// 搜索历史时光标所在的列（footer 里查询的末尾）。
pub fn search_cursor(app: &App) -> Option<u16> {
    let (query, _) = app.composer.search.as_ref()?;
    let label = match app.lang {
        Lang::En => "search prompts: ",
        Lang::Zh => "搜索输入：",
    };
    Some((2 + label.width() + query.width()) as u16)
}

/// `?` 快捷键面板（tui.md §7.4，修复线 F26，用户决定；排版参照 Codex `shortcut_overlay.rs` / `shortcut_help.rs`）：
/// 画在输入框上面。粗体标题、空一行，三组（Compose / Session / Transcript）并排，组间 4 列；放不下时两组一排，
/// 再放不下一组一排。每行按键（`key` 样式，补到本组最长的按键 + 2 列）加说明（`secondary`）。原来是输入框下面
/// 三栏 dim 的文字。
pub fn help(app: &App, width: usize) -> Vec<Line<'static>> {
    let zh = app.lang == Lang::Zh;
    let paste = super::shortcuts::image_paste_chord();
    let pick = |en: &'static str, zh_text: &'static str| if zh { zh_text } else { en };
    // 说明写短，80 列放得下三组（tui.md §7.4 的表）。
    let groups: [(&str, Vec<(&str, &str)>); 3] = [
        (
            pick("Compose", "输入"),
            vec![
                ("!", pick("shell mode", "shell 模式")),
                ("/", pick("commands", "命令")),
                ("@", pick("file paths", "文件路径")),
                ("shift+enter", pick("newline", "换行")),
                ("ctrl+_", pick("undo", "撤销")),
                (paste, pick("paste image", "粘贴图片")),
                ("ctrl+s", pick("stash", "暂存输入")),
                ("ctrl+g", pick("editor", "外部编辑器")),
                ("esc esc", pick("clear", "清空输入")),
            ],
        ),
        (
            pick("Session", "会话"),
            vec![
                ("enter", pick("send", "发送")),
                ("ctrl+enter", pick("send now", "立即发送")),
                ("ctrl+x enter", pick("queue", "排队发送")),
                ("esc", pick("interrupt", "中断")),
                ("shift+tab", pick("modes", "切换模式")),
                ("meta+p", pick("model", "切换模型")),
            ],
        ),
        (
            pick("Transcript", "记录"),
            vec![
                ("ctrl+o", pick("details", "查看详细")),
                ("ctrl+r", pick("history", "搜索历史")),
                ("ctrl+t", pick("tasks", "任务清单")),
                ("pgup/pgdn", pick("scroll", "滚动")),
                ("←", pick("agents", "代理")),
                ("↓", pick("background", "后台任务")),
            ],
        ),
    ];
    let palette = &app.palette;
    let bold = Style::default().add_modifier(Modifier::BOLD);
    // 一组的行：组名，然后每行按键补齐、说明。
    let column = |(title, rows): &(&str, Vec<(&str, &str)>)| -> Vec<Line<'static>> {
        let keys = rows.iter().map(|(key, _)| key.width()).max().unwrap_or(0) + 2;
        let mut lines = vec![Line::from(Span::styled((*title).to_owned(), bold))];
        lines.extend(rows.iter().map(|(key, text)| {
            Line::from(vec![
                Span::styled((*key).to_owned(), palette.key()),
                Span::raw(" ".repeat(keys - key.width())),
                Span::styled((*text).to_owned(), palette.secondary()),
            ])
        }));
        lines
    };
    let columns: Vec<Vec<Line<'static>>> = groups.iter().map(column).collect();
    let column_width = |lines: &[Line<'static>]| lines.iter().map(Line::width).max().unwrap_or(0);
    let widths: Vec<usize> = columns.iter().map(|c| column_width(c)).collect();
    const GAP: usize = 4;
    // 一排能放几组：三组、两组、一组。
    let per_row = [3usize, 2]
        .into_iter()
        .find(|n| {
            widths
                .chunks(*n)
                .all(|chunk| 2 + chunk.iter().sum::<usize>() + GAP * (chunk.len() - 1) <= width)
        })
        .unwrap_or(1);
    let mut out = vec![
        Line::from(vec![
            Span::raw("  "),
            Span::styled(pick("Keyboard shortcuts", "快捷键").to_owned(), bold),
        ]),
        Line::default(),
    ];
    for (row, chunk) in columns.chunks(per_row).enumerate() {
        if row > 0 {
            out.push(Line::default());
        }
        let chunk_widths = &widths[row * per_row..row * per_row + chunk.len()];
        let height = chunk.iter().map(Vec::len).max().unwrap_or(0);
        for index in 0..height {
            let mut spans = vec![Span::raw("  ")];
            for (column, lines) in chunk.iter().enumerate() {
                let line = lines.get(index).cloned().unwrap_or_default();
                let used = line.width();
                spans.extend(line.spans);
                if column + 1 < chunk.len() {
                    spans.push(Span::raw(" ".repeat(chunk_widths[column] - used + GAP)));
                }
            }
            out.push(truncate_line(Line::from(spans), width));
        }
    }
    out
}

#[cfg(test)]
#[path = "footer_tests.rs"]
mod tests;
