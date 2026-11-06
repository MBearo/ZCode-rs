//! 任务行可点的地方（tui-tasks.md §2.2「点击」）：每个部分的列与种类、记号与 `^T` 同 ^T、短写与截断之后、名字不能点、
//! ^T 不起作用时没有。
use std::time::Instant;

use rust_cli_tui_kit::render::width::string_width;

use super::tests::{app, entry, set, shell, todos};
use super::*;

const CARD: Target = Target::Tasks(None);
const SUBAGENTS: Target = Target::Tasks(Some(TaskKind::Subagents));
const COMMANDS: Target = Target::Tasks(Some(TaskKind::Commands));
const TODOS: Target = Target::Tasks(Some(TaskKind::Todos));

fn agents_and_a_shell(lang: Lang) -> App {
    let mut app = app(lang);
    set(
        &mut app,
        vec![
            entry("a", true, SubagentStatus::Running),
            entry("b", true, SubagentStatus::Running),
        ],
    );
    app.snapshots.background = vec![shell("x", "npm run dev")];
    app
}

/// 一行里从第 `col` 格起、`width` 格宽的字。
fn cells(text: &str, col: usize, width: usize) -> String {
    let mut at = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = string_width(&c.to_string());
        if at >= col && at + w <= col + width {
            out.push(c);
        }
        at += w;
    }
    out
}

/// 一行与它可点的地方，每一处写成（那一处的字，目标）。
fn laid(app: &App, width: usize, name: Option<Title>) -> (String, Vec<(String, Target)>) {
    let row = app
        .title_row(width, Instant::now(), name, true)
        .expect("a title row");
    let plain = row.line.plain();
    let spots = row
        .clicks
        .into_iter()
        .map(|(col, w, target)| (cells(&plain, col, w), target))
        .collect();
    (plain, spots)
}

fn spot(text: &str, target: Target) -> (String, Target) {
    (text.to_owned(), target)
}

/// 每个部分落在它的种类；记号与 `↓/^T all tasks` 同 ^T；` · ` 与空格不算。
#[test]
fn every_part_opens_its_own_kind() {
    let mut app = agents_and_a_shell(Lang::En);
    todos(&mut app, 1, 3);
    let (plain, spots) = laid(&app, 120, None);
    assert_eq!(
        plain,
        " ○ 2 subagents running · 1 shell running · 1 of 3 todos done   ↓/^T all tasks"
    );
    assert_eq!(
        spots,
        vec![
            spot("○", CARD),
            spot("2 subagents running", SUBAGENTS),
            spot("1 shell running", COMMANDS),
            spot("1 of 3 todos done", TODOS),
            spot("↓/^T all tasks", CARD),
        ]
    );
}

/// 等你回答、完成、出错、停了这些整段一种墨的部分也是子代理；中文按格数算列。
#[test]
fn inked_parts_are_subagents_and_wide_text_counts_by_cells() {
    let mut app = app(Lang::Zh);
    let mut waiting = entry("a", true, SubagentStatus::Running);
    waiting.waiting = true;
    set(&mut app, vec![waiting]);
    app.snapshots.background = vec![shell("x", "npm run dev")];
    let (_, spots) = laid(&app, 120, None);
    assert_eq!(
        spots,
        vec![
            spot("●", CARD),
            spot("1 个子代理等你回答", SUBAGENTS),
            spot("1 shell 在跑", COMMANDS),
            spot("↓/^T 全部任务", CARD),
        ]
    );
}

/// 窄的时候按画出来的样子：短写的部分照样是它的种类；截了一半的部分带着 `…` 仍是它的种类，截没了的没有地方可点。
#[test]
fn short_and_clipped_parts_keep_their_kind() {
    let app = agents_and_a_shell(Lang::En);
    let (plain, spots) = laid(&app, 38, None);
    assert_eq!(plain, " ○ 2 subagents · 1 shell   ^T");
    assert_eq!(
        spots,
        vec![
            spot("○", CARD),
            spot("2 subagents", SUBAGENTS),
            spot("1 shell", COMMANDS),
            spot("^T", CARD),
        ]
    );
    let (plain, spots) = laid(&app, 20, None);
    assert!(plain.ends_with("…   ^T"), "{plain}");
    assert_eq!(spots.len(), 3, "{spots:?}");
    assert_eq!(spots[0], spot("○", CARD));
    assert_eq!(spots[1].1, SUBAGENTS);
    assert!(spots[1].0.ends_with('…'), "{spots:?}");
    assert_eq!(spots[2], spot("^T", CARD));
}

/// 右边的名字不能点；子代理的描述把计数挤走时只剩记号与 `^T`。
#[test]
fn the_name_is_not_a_spot() {
    let app = agents_and_a_shell(Lang::En);
    let fg3 = app.ink.theme.fg3;
    let title = |stays| Title {
        text: "Fix the footer spacing".to_owned(),
        style: fg3,
        stays,
    };
    let (plain, spots) = laid(&app, 100, Some(title(false)));
    assert!(plain.ends_with("Fix the footer spacing"), "{plain}");
    assert_eq!(spots.last(), Some(&spot("↓/^T all tasks", CARD)));
    let (_, spots) = laid(&app, 30, Some(title(true)));
    assert_eq!(spots, vec![spot("○", CARD), spot("^T", CARD)]);
}

/// ^T 不起作用时（草稿不空）`^T` 是 quiet 墨，这一行没有可点的地方。
#[test]
fn nothing_is_clickable_while_the_key_does_not_work() {
    let mut app = agents_and_a_shell(Lang::En);
    app.composer.area.set_text("draft");
    let row = app.title_row(80, Instant::now(), None, true).unwrap();
    assert!(row.line.plain().contains("↓/^T all tasks"));
    assert!(row.clicks.is_empty(), "{:?}", row.clicks);
}
