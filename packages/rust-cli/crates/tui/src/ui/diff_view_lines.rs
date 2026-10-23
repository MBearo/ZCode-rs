//! `/diff` 的排版与文字（docs/specs/rust-cli/tui-manage.md §4.2）：英文照原文，中文另配；diff 行用界面
//! 同一套排版（`cells::patch`）。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::app::App;
use super::cells::Ctx;
use super::cells::patch::{diff_lines, edits_rows, unified_rows};
use super::diff_view::{Current, DiffFile, DiffView};
use super::i18n::Lang;
use super::text::truncate_line;

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn files_word(lang: Lang, n: usize) -> String {
    match lang {
        Lang::En if n == 1 => "1 file".to_owned(),
        Lang::En => format!("{n} files"),
        Lang::Zh => format!("{n} 个文件"),
    }
}

/// 列表或详情里一个文件（当前改动的，或某个回合的）。
struct Shown {
    path: String,
    added: u64,
    removed: u64,
    note: Option<&'static str>,
}

fn shown(lang: Lang, file: &DiffFile) -> Shown {
    let note = match (lang, file.untracked, file.binary) {
        (Lang::En, true, _) => Some("untracked"),
        (Lang::Zh, true, _) => Some("未跟踪"),
        (Lang::En, false, true) => Some("Binary file"),
        (Lang::Zh, false, true) => Some("二进制文件"),
        (_, false, false) => None,
    };
    Shown {
        path: file.path.clone(),
        added: file.added,
        removed: file.removed,
        note,
    }
}

fn counts(added: u64, removed: u64) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if added > 0 {
        spans.push(Span::styled(
            format!(" +{added}"),
            Style::default().fg(Color::Green),
        ));
    }
    if removed > 0 {
        spans.push(Span::styled(
            format!(" -{removed}"),
            Style::default().fg(Color::Red),
        ));
    }
    spans
}

/// 页面的行（替换输入区）；`height` 是可用高度。
pub fn lines(
    app: &App,
    view: &DiffView,
    width: usize,
    height: usize,
    now_ms: u64,
) -> Vec<Line<'static>> {
    let lang = app.lang;
    let zh = lang == Lang::Zh;
    let accent = app.palette.accent;
    let turns = app.view.turn_changes();
    let turn = view
        .source
        .checked_sub(1)
        .and_then(|index| turns.get(index));
    let (title, subtitle) = match turn {
        Some(changes) => {
            let preview = changes.prompt.lines().next().unwrap_or_default().trim();
            (
                if zh {
                    format!("第 {} 轮", view.source)
                } else {
                    format!("Turn {}", view.source)
                },
                (!preview.is_empty()).then(|| format!("\"{preview}\"")),
            )
        }
        None => (
            if zh {
                "未提交的改动"
            } else {
                "Uncommitted changes"
            }
            .to_owned(),
            Some("(git diff HEAD)".to_owned()),
        ),
    };
    let mut header = vec![Span::styled(
        format!("  {title}"),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if let Some(subtitle) = subtitle {
        header.push(Span::styled(format!(" {subtitle}"), dim()));
    }
    let mut lines = vec![
        Line::from(Span::styled("─".repeat(width), Style::default().fg(accent))),
        truncate_line(Line::from(header), width),
    ];
    if !turns.is_empty() {
        let mut spans = vec![Span::raw("  ")];
        if view.source > 0 {
            spans.push(Span::styled("◀ ", dim()));
        }
        for index in 0..=turns.len() {
            let label = if index == 0 {
                if zh { "当前" } else { "Current" }.to_owned()
            } else {
                format!("T{index}")
            };
            if index > 0 {
                spans.push(Span::styled(" · ", dim()));
            }
            spans.push(if index == view.source {
                Span::styled(label, Style::default().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(label, dim())
            });
        }
        if view.source < turns.len() {
            spans.push(Span::styled(" ▶", dim()));
        }
        lines.push(truncate_line(Line::from(spans), width));
    }
    // 文件与统计。
    let Listing {
        files,
        stats,
        empty,
    } = listing(lang, turn, &view.current);
    if let Some((count, added, removed)) = stats.filter(|(count, _, _)| *count > 0) {
        let mut spans = vec![Span::styled(
            if zh {
                format!("  {} 有改动", files_word(lang, count))
            } else {
                format!("  {} changed", files_word(lang, count))
            },
            dim(),
        )];
        spans.extend(counts(added, removed));
        lines.push(truncate_line(Line::from(spans), width));
    }
    lines.push(Line::default());
    let footer = |text: &str| Line::from(Span::styled(format!("  {text}"), dim()));
    // 按键提示（`{键}` 模板，按键提示组件画，tui.md §7.4）。
    let keys = |text: &str| super::key_hint::line(2, text, &app.palette);
    if let Some(empty) = empty {
        lines.push(footer(&empty));
        lines.push(Line::default());
        lines.push(keys(if zh { "{esc} 关闭" } else { "{Esc} to close" }));
        return lines;
    }
    let room = height.saturating_sub(lines.len() + 2).max(3);
    if let Some(scroll) = view.detail {
        let Some(file) = files.get(view.selected) else {
            return lines;
        };
        let mut head = vec![Span::raw(format!("  {}", file.path))];
        head.extend(counts(file.added, file.removed));
        lines.push(truncate_line(Line::from(head), width));
        let body: Vec<Line<'static>> = match (turn, &view.current) {
            (None, Current::Loaded { files: all, .. }) => {
                let found = &all[view.selected];
                if found.untracked {
                    let [first, second] = if zh {
                        ["新文件，还没有暂存。", "用 git add 暂存后才能看到行数。"]
                    } else {
                        [
                            "New file not yet staged.",
                            "Stage it with git add to see line counts.",
                        ]
                    };
                    vec![footer(first), footer(second)]
                } else {
                    rendered(app, &unified_rows(&found.body), width, now_ms)
                }
            }
            _ => {
                let edits = app.view.turn_edits(view.source - 1, &file.path);
                rendered(app, &edits_rows(&edits), width, now_ms)
            }
        };
        let start = scroll.min(body.len().saturating_sub(1));
        lines.extend(body.into_iter().skip(start).take(room.saturating_sub(1)));
        lines.push(Line::default());
        lines.push(keys(if zh {
            "{↑/↓} 滚动 · {esc} 返回"
        } else {
            "{↑/↓} to scroll · {Esc} to back"
        }));
        return lines;
    }
    let start = view.selected.saturating_sub(room.saturating_sub(1));
    for (index, file) in files.iter().enumerate().skip(start).take(room) {
        let focused = index == view.selected;
        // 选择列表组件（`› ` 加底色条，tui.md §7.4，修复线 F26）。
        let mut spans = vec![Span::raw("  "), super::pick::pointer(focused)];
        spans.push(Span::raw(file.path.clone()));
        match file.note {
            Some(note) => spans.push(Span::styled(format!(" {note}"), dim())),
            None => spans.extend(counts(file.added, file.removed)),
        }
        lines.push(super::pick::paint(
            truncate_line(Line::from(spans), width),
            focused,
            &app.palette,
            width,
        ));
    }
    lines.push(Line::default());
    let switch = if turns.is_empty() {
        ""
    } else if zh {
        "{←/→} 切换 · "
    } else {
        "{←/→} to switch · "
    };
    lines.push(keys(&if zh {
        format!("{switch}{{↑/↓}} 选择 · {{enter}} 查看 · {{esc}} 关闭")
    } else {
        format!("{switch}{{↑/↓}} to select · {{Enter}} to view · {{Esc}} to close")
    }));
    lines
}

/// 这个来源的文件、统计与空状态的说明。
struct Listing {
    files: Vec<Shown>,
    stats: Option<(usize, u64, u64)>,
    empty: Option<String>,
}

fn listing(lang: Lang, turn: Option<&crate::view::TurnChanges>, current: &Current) -> Listing {
    let zh = lang == Lang::Zh;
    let (files, stats, empty) = match (turn, current) {
            (Some(changes), _) => {
                let files: Vec<Shown> = changes
                    .files
                    .iter()
                    .map(|file| Shown {
                        path: file.path.clone(),
                        added: file.additions,
                        removed: file.deletions,
                        note: None,
                    })
                    .collect();
                let added = files.iter().map(|f| f.added).sum();
                let removed = files.iter().map(|f| f.removed).sum();
                let stats = Some((files.len(), added, removed));
                let empty = files.is_empty().then(|| {
                    if zh { "这一轮没有文件改动" } else { "No file changes in this turn" }.to_owned()
                });
                (files, stats, empty)
            }
            (None, Current::Loading) => (Vec::new(), None, Some(if zh { "读取 diff…" } else { "Loading diff…" }.to_owned())),
            (None, Current::NotRepository) => (
                Vec::new(),
                None,
                Some(if zh {
                    "diff 页面显示 git 改动，当前目录不在 git 仓库里"
                } else {
                    "The diff panel shows git changes — the current directory isn't in a git repository"
                }
                .to_owned()),
            ),
            (None, Current::Failed(reason)) => (
                Vec::new(),
                None,
                Some(if zh {
                    format!("无法读取 diff（{reason}）")
                } else {
                    format!("Diff unavailable ({reason})")
                }),
            ),
            (None, Current::Loaded { files, stats }) => {
                let shown: Vec<Shown> = files.iter().map(|file| self::shown(lang, file)).collect();
                let empty = (stats.0 == 0).then(|| if zh { "还没有改动" } else { "No changes yet" }.to_owned());
                let empty = empty.or_else(|| {
                    shown.is_empty().then(|| {
                        if zh { "文件太多，无法显示细节" } else { "Too many files to display details" }.to_owned()
                    })
                });
                (shown, Some(*stats), empty)
            }
        };
    Listing {
        files,
        stats,
        empty,
    }
}

fn rendered(
    app: &App,
    rows: &[super::cells::patch::PatchRow],
    width: usize,
    now_ms: u64,
) -> Vec<Line<'static>> {
    let label = crate::ui::app::label(app.view.workspace().root.as_str(), app.home.as_deref());
    let ctx = Ctx {
        palette: &app.palette,
        lang: app.lang,
        strings: app.strings,
        width,
        expanded: true,
        elapsed: std::time::Duration::ZERO,
        now_ms,
        workspace: app.view.workspace().root.as_str(),
        workspace_label: &label,
        version: &app.version,
        markdown: None,
    };
    diff_lines(rows, &ctx, usize::MAX).0
}
