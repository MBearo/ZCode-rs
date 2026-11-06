//! `/diff` 的排版与文字（docs/specs/rust-cli/tui-manage.md §4.2）：英文照原文，中文另配；diff 行用界面
//! 同一套排版（`cells::patch`）。
//!
//! 画法（tui-kit.md §3.8，工作线 TK 的 C1）：[`lines`] 产出 kit 的 [`StyledLine`]，墨色经 `app.ink`
//! （dim → `fg3`，+N → `ok`，−N → `err`）。
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::app::App;
use super::cells::Ctx;
use super::cells::patch::{diff, edits_rows, unified_rows};
use super::diff_view::{Current, DiffFile, DiffView};
use super::i18n::Lang;
use super::ink::Ink;
use super::layout;
use super::pick;

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

fn counts(added: u64, removed: u64, ink: &Ink) -> Vec<Span> {
    let mut spans = Vec::new();
    if added > 0 {
        spans.push(Span::new(format!(" +{added}"), ink.success()));
    }
    if removed > 0 {
        spans.push(Span::new(format!(" -{removed}"), ink.destructive()));
    }
    spans
}

/// 来源的一排（当前改动与各回合）：选中的是 [`pick::chosen`]，其余 fg3，之间 line 色的 ` · `，两头能换时 fg3 的
/// `◀` / `▶`。
fn sources(view: &DiffView, turns: usize, zh: bool, ink: &Ink) -> Element {
    let quiet = ink.theme.fg3;
    let mut spans = Vec::new();
    if view.source > 0 {
        spans.push(Span::new("◀ ", quiet));
    }
    for index in 0..=turns {
        let label = if index == 0 {
            if zh { "当前" } else { "Current" }.to_owned()
        } else {
            format!("T{index}")
        };
        if index > 0 {
            spans.push(Span::new(" · ", ink.theme.line));
        }
        let style = if index == view.source {
            pick::chosen(ink)
        } else {
            quiet
        };
        spans.push(Span::new(label, style));
    }
    if view.source < turns {
        spans.push(Span::new(" ▶", quiet));
    }
    pick::row(spans)
}

/// 页面的行（替换输入区）；`height` 是可用高度。
///
/// 第二级的样子（tui-kit.md §4.3「`/diff` 视图」，自拟，工作线 TK 的 S3）：整页是输入框的板上的一层（[`pick::layer`]：
/// rich 是 panel 底与半块上下边，plain 是 line 色的上下线），板下面一行是按键提示（第 3 列起，Puffin 状态栏的位置）。
/// 板里：头一行（[`pick::head`]：fg2 粗体的标题，三格后 fg3 的说明）、来源的一排、fg3 的统计；文件列表照 Puffin 的
/// 选择器（没有指针与底色条，选中的路径 brand、其余 fg2，说明 fg3，窗口外的是 fg3 的 `↑ n more` / `↓ n more`）；
/// 文件详情是 `cells::patch` 的 diff 行（新增、删除的行在 `add_bg` / `del_bg` 上）。
pub fn lines(
    app: &App,
    view: &DiffView,
    width: usize,
    height: usize,
    now_ms: u64,
) -> (Vec<StyledLine>, Vec<Span>) {
    let lang = app.lang;
    let zh = lang == Lang::Zh;
    let ink = &app.ink;
    let dim = ink.dim();
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
    let mut rows = vec![pick::head(&title, subtitle.as_deref(), ink)];
    if !turns.is_empty() {
        rows.push(sources(view, turns.len(), zh, ink));
    }
    // 文件与统计。
    let Listing {
        files,
        stats,
        empty,
    } = listing(lang, turn, &view.current);
    if let Some((count, added, removed)) = stats.filter(|(count, _, _)| *count > 0) {
        let mut spans = vec![Span::new(
            if zh {
                format!("{} 有改动", files_word(lang, count))
            } else {
                format!("{} changed", files_word(lang, count))
            },
            dim,
        )];
        spans.extend(counts(added, removed, ink));
        rows.push(pick::row(spans));
    }
    // 按键提示（`{键}` 模板，按键提示组件画，tui.md §7.4）在草稿行的槽里（tui-kit.md §4.0「板」）；以前在板下面一行。
    let keys = |text: &str| super::key_hint::spans(text, ink);
    let done = |rows: Vec<Element>, hint: Vec<Span>| (pick::layer(rows, ink, width), hint);
    if let Some(empty) = empty {
        rows.push(pick::quiet_row(empty, ink));
        return done(rows, keys(if zh { "{esc} 关闭" } else { "{esc} to close" }));
    }
    // 板的上下边、草稿行各占一行。
    let room = height
        .saturating_sub(rows.len() + 3)
        .max(layout::MIN_LIST_ROWS);
    if let Some(scroll) = view.detail {
        let Some(file) = files.get(view.selected) else {
            return done(rows, Vec::new());
        };
        let mut head = vec![Span::new(file.path.clone(), ink.theme.fg2)];
        head.extend(counts(file.added, file.removed, ink));
        rows.push(pick::row(head));
        let body: Vec<StyledLine> = match (turn, &view.current) {
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
                    pick::lay(
                        &[pick::quiet_row(first, ink), pick::quiet_row(second, ink)],
                        width,
                    )
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
        rows.push(Element::Raw(
            body.into_iter()
                .skip(start)
                .take(room.saturating_sub(1))
                .collect(),
        ));
        return done(
            rows,
            keys(if zh {
                "{↑↓} 滚动 · {esc} 返回"
            } else {
                "{↑↓} to scroll · {esc} to go back"
            }),
        );
    }
    let selected = view.selected.min(files.len().saturating_sub(1));
    let (from, shown) = pick::window(selected, files.len(), room);
    if from > 0 {
        rows.push(pick::more_above(from, 0, ink));
    }
    for (index, file) in files.iter().enumerate().skip(from).take(shown) {
        let focused = index == selected;
        // Puffin 选择器的行（tui-kit.md §4.2）：没有指针与底色条，选中的路径 brand。
        let mut spans = vec![Span::new(file.path.clone(), pick::name_style(ink, focused))];
        match file.note {
            Some(note) => spans.push(Span::new(
                format!(" {note}"),
                pick::desc_style(ink, focused),
            )),
            None => spans.extend(counts(file.added, file.removed, ink)),
        }
        rows.push(pick::row(spans));
    }
    let below = files.len().saturating_sub(from + shown);
    if below > 0 {
        rows.push(pick::more_below(below, 0, ink));
    }
    let switch = if turns.is_empty() {
        ""
    } else if zh {
        "{←/→} 切换 · "
    } else {
        "{←/→} to switch · "
    };
    done(
        rows,
        keys(&if zh {
            format!("{switch}{{↑↓}} 选择 · {{⏎}} 查看 · {{esc}} 关闭")
        } else {
            format!("{switch}{{↑↓}} to select · {{⏎}} to view · {{esc}} to close")
        }),
    )
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
) -> Vec<StyledLine> {
    let label = crate::ui::app::label(app.view.workspace().root.as_str(), app.home.as_deref());
    let ctx = Ctx {
        ink: &app.ink,
        lang: app.lang,
        width,
        expanded: true,
        elapsed: std::time::Duration::ZERO,
        now_ms,
        workspace: app.view.workspace().root.as_str(),
        workspace_label: &label,
        version: &app.version,
        mcp_machines: &app.mcp_machines,
        machines: &app.machines,
        skills: app.composer.mentions.catalog.as_deref().unwrap_or(&[]),
        markdown: None,
    };
    diff(rows, &ctx, usize::MAX).0
}
