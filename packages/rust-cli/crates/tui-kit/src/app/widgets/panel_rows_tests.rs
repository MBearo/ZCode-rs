//! 请求的板、守门的几屏、待办坞、`$` 与 `@` 面板、粘贴的帽与机器的行（docs/specs/rust-cli/tui-kit.md §4.4 K4）：每个从
//! Puffin 搬来的画法一个有代表性的中性输入，铺出来看行。Puffin 的 asks、kept、todo_dock、mentions、at、pastes、machines
//! 的测试经它的映射测同样的规则；路径的缩短与说明的折行的纯用例随画法从 Puffin 搬来。
use std::collections::BTreeMap;

use serde_json::json;

use crate::app::element::{Element, stack};
use crate::app::layout::Layout;
use crate::app::widgets::asks::{self, data::Ask};
use crate::app::widgets::at_panel::rows::{self as at_rows, Ctx};
use crate::app::widgets::at_panel::{AtGroup, AtItem, AtPanel};
use crate::app::widgets::kept_rows::{self, Grants, Trust, YoloFocus};
use crate::app::widgets::machine_rows::{self, Chosen, Machine, MachineRow, Phase};
use crate::app::widgets::modes::Mode;
use crate::app::widgets::paste_caps;
use crate::app::widgets::skill_panel::{self, Panel, Skill, SkillFrom};
use crate::app::widgets::todo_dock::{self, Todos};
use crate::render::Span;
use crate::render::width::string_width;
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

const NOW: f64 = 1_792_080_000_000.0;

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn utc(_: f64) -> i32 {
    0
}

fn lines(rows: Vec<Element>, w: usize) -> Vec<String> {
    Layout::lay(&stack(rows), w)
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

fn text(spans: &[Span]) -> String {
    spans.iter().map(|sp| sp.text.as_ref()).collect()
}

fn command_ask() -> Ask {
    serde_json::from_value(json!({
        "id": "a1",
        "itemId": "t1",
        "toolName": "Bash",
        "name": "Bash",
        "summary": "rm -rf build",
        "kind": "permission",
        "preview": {"kind": "command", "command": "rm -rf build", "reason": "Clean the build"},
        "options": [
            {"id": "o1", "kind": "allowOnce"},
            {"id": "o2", "kind": "deny"}
        ]
    }))
    .unwrap()
}

#[test]
fn a_permission_ask_has_its_question_its_answers_and_its_preview() {
    let t = rich();
    let ask = command_ask();
    assert!(asks::is_permission(&ask) && !asks::is_question(&ask));
    let answers = asks::answers(&t, &ask, "/Users/dev/project", false, 1, None);
    assert_eq!(answers.len(), 2);
    let rows = lines(asks::answer_rows(&t, &answers, 0, 1.0), 100);
    assert!(rows[0].contains('1') && rows[1].contains('2'));
    let head = lines(vec![asks::head_row(&t, &ask, 2)], 100);
    assert!(head[0].contains("2 more waiting"), "{head:?}");
    let preview = lines(asks::preview::permission_preview(&t, &ask, "", 3), 100);
    assert!(preview.iter().any(|l| l.contains("Clean the build")));
    assert_eq!(asks::countdown(9), "auto-continue in 9s · any key to stay");
}

#[test]
fn a_plan_s_answers_name_the_mode_it_runs_in() {
    let t = rich();
    let ask: Ask = serde_json::from_value(json!({
        "id": "p1", "itemId": "", "toolName": "ExitPlanMode", "name": "Plan", "summary": "",
        "kind": "plan", "plan": "1. Do it"
    }))
    .unwrap();
    let answers = asks::answers(&t, &ask, "/", false, 1, Some(Mode::Build));
    let shown = lines(asks::answer_rows(&t, &answers, 0, 1.0), 100).join("\n");
    assert!(
        shown.contains("keep planning") || shown.contains("Keep planning"),
        "{shown}"
    );
}

#[test]
fn the_full_access_confirm_needs_the_tick_before_confirm() {
    let t = rich();
    let got = lines(kept_rows::yolo_rows(&t, false, YoloFocus::Cancel), 100);
    assert!(got[0].contains(kept_rows::YOLO_TITLE));
    assert!(got.iter().any(|l| l.contains("□ I understand the risks")));
    assert!(got.iter().any(|l| l.ends_with("2  Confirm")));
    assert_eq!(YoloFocus::Cancel.step(true), YoloFocus::Confirm);
    assert_eq!(YoloFocus::Acknowledge.step(false), YoloFocus::Confirm);
    assert_eq!(kept_rows::yolo_slot(YoloFocus::Acknowledge)[0].1, "tick");
}

#[test]
fn the_trust_ask_lists_the_project_grants_and_the_two_answers() {
    let t = rich();
    let rules = ["Bash(npm test)".to_owned(), "Read".to_owned()];
    let sources = [".zcode/settings.json".to_owned()];
    let trust = Trust {
        path: "/Users/dev/project",
        backstop: true,
        grants: Some(Grants {
            count: 2,
            rules: &rules,
            sources: &sources,
        }),
    };
    let got = lines(kept_rows::trust_rows(&t, &trust, 0, None), 120);
    assert!(
        got.iter()
            .any(|l| l.contains("pre-approves 2 tool permissions"))
    );
    assert!(got.iter().any(|l| l.contains("Bash(npm test) and Read")));
    assert!(
        got.iter()
            .any(|l| l.ends_with("1  No, continue without these permissions"))
    );
    let armed = lines(kept_rows::trust_rows(&t, &trust, 1, Some('C')), 120);
    assert_eq!(
        armed.last().map(String::as_str),
        Some("   ^C again to exit")
    );
    assert_eq!(
        kept_rows::list(&["a".into(), "b".into(), "c".into()], None),
        "a, b, and c"
    );
}

#[test]
fn the_update_screen_shows_the_versions_and_three_choices() {
    let got = lines(
        kept_rows::update_rows(
            &rich(),
            "0.1.0",
            "0.2.0",
            "zcode update",
            "https://example.com",
            0,
        ),
        100,
    );
    assert!(got[0].contains("Update available") && got[0].contains("0.1.0 → 0.2.0"));
    assert!(got.iter().any(|l| l.contains("runs `zcode update`")));
    assert!(got.iter().any(|l| l.contains("Skip until next version")));
}

#[test]
fn the_todo_dock_says_where_the_agent_is() {
    let t = rich();
    let todos = Todos {
        steps: vec![
            ("completed".to_owned(), "Read the code".to_owned()),
            ("inProgress".to_owned(), "Fix the bug".to_owned()),
            ("pending".to_owned(), "Run the tests".to_owned()),
        ],
        fresh: true,
    };
    let got = lines(vec![todo_dock::dock(&t, Some(&todos), true, false)], 80);
    assert!(got[0].contains("Read the code") && got[0].ends_with("1 of 3 done"));
    assert!(got[1].contains("○ Fix the bug"));
    let one = lines(vec![todo_dock::dock(&t, Some(&todos), true, true)], 80);
    assert_eq!(one.len(), 1);
    assert!(one[0].contains("Fix the bug"));
}

#[test]
fn descriptions_wrap_by_terminal_cells_chinese_takes_two_cells_a_character_and_breaks_anywhere() {
    assert_eq!(
        skill_panel::wrap("Use when encountering any bug, test failure.", 20),
        ["Use when", "encountering any", "bug, test failure."]
    );
    assert_eq!(
        skill_panel::wrap("按团队模板写周报：汇总本周的提交", 12),
        ["按团队模板写", "周报：汇总本", "周的提交"]
    );
}

#[test]
fn the_skill_panel_previews_the_chosen_skill_beside_wide_and_under_narrow() {
    let t = rich();
    let found = [
        Skill {
            name: "docx",
            description: "Write Word documents.",
            from: SkillFrom::Plugin("office"),
        },
        Skill {
            name: "debug",
            description: "Find bugs.",
            from: SkillFrom::User,
        },
    ];
    let p = Panel {
        catalog: Some(2),
        matches: &found,
        choice: 0,
        opened_age: 100,
        shown_age: 100,
    };
    let wide = lines(skill_panel::rows(&t, &p, 100), 100);
    assert!(wide[0].contains("$docx") && wide[0].contains("office plugin"));
    assert!(wide.iter().any(|l| l.contains("Write Word documents.")));
    let narrow = lines(skill_panel::rows(&t, &p, 60), 60);
    assert!(narrow[0].contains("$docx") && narrow[0].contains("office"));
    let none = Panel {
        catalog: None,
        matches: &[],
        ..p
    };
    assert_eq!(
        lines(skill_panel::rows(&t, &none, 100), 100),
        ["   Looking for skills…"]
    );
    assert_eq!(skill_panel::keys(Some(2), 0)[0].1, "send");
}

#[test]
fn a_folder_is_shortened_at_its_slashes_keeping_the_most_whole_parts_the_leading_ones_on_a_tie() {
    assert_eq!(at_rows::clip_path("docs", 17), "docs");
    assert_eq!(
        at_rows::clip_path("packages/tui/tui/src/zcode/tui/shell", 17),
        "…/zcode/tui/shell"
    );
    assert_eq!(
        at_rows::clip_path("packages/ui/src/mentions", 17),
        "…/ui/src/mentions"
    );
    // 一样多的整段：保留开头的段。
    assert_eq!(at_rows::clip_path("ab/cdefg/hi/jk", 8), "ab/…/jk");
    assert!(string_width(&at_rows::clip_path("averyveryverylongfoldername", 10)) <= 10);
}

#[test]
fn the_at_panel_lists_groups_and_previews_the_chosen_file() {
    let t = rich();
    let p = AtPanel {
        groups: vec![AtGroup::of(
            "Files",
            vec![AtItem::Path {
                path: "docs/specs/plan.md".to_owned(),
                folder: false,
            }],
        )],
        under: String::new(),
        q: String::new(),
    };
    let peeks = BTreeMap::new();
    let touch = BTreeMap::new();
    let ctx = Ctx {
        peeks: &peeks,
        touch: &touch,
        choice: 0,
        opened_age: 100,
        files_age: 100,
        shown_age: 100,
        now: NOW,
        offset: &utc,
    };
    let got = lines(at_rows::rows(&t, &p, &ctx, 100), 100);
    // 宽的终端：预览从第 PANE_COL 列起，在组名的那一行旁边。
    assert!(got[0].starts_with("   Files "));
    assert_eq!(got[0].find("plan.md"), Some(at_rows::PANE_COL));
    assert!(got[1].contains("plan.md") && got[1].contains("docs/specs"));
    assert_eq!(at_rows::keys(&p, 0)[1].1, "insert");
    let empty = AtPanel {
        groups: Vec::new(),
        ..p
    };
    assert_eq!(
        lines(at_rows::rows(&t, &empty, &ctx, 100), 100),
        ["   Nothing matches. It goes out as text."]
    );
}

#[test]
fn a_paste_cap_names_its_size_and_the_peek_shows_the_first_lines() {
    let t = rich();
    assert_eq!(paste_caps::size(214, 9000), "214 lines");
    assert_eq!(paste_caps::size(1, 41_200), "41.2k chars");
    assert_eq!(paste_caps::bytes(42_188.8), "41.2 KB");
    assert_eq!(paste_caps::commas(12_345), "12,345");
    assert_eq!(paste_caps::size_cap("214 lines", 80), "Pasted · 214 lines");
    assert_eq!(paste_caps::size_cap("214 lines", 10), "214 lines");
    assert_eq!(
        text(&paste_caps::cap(&t, "Pasted · 3 lines", true, false, 0)),
        " Pasted · 3 lines "
    );
    let peek = lines(paste_caps::text_peek(&t, "one\ntwo\nthree", 80, 100), 80);
    assert_eq!(peek, ["   │ one", "   │ two", "   │ three", "   │ 3 lines"]);
    let row = lines(
        vec![paste_caps::attached_row(
            &t,
            "pasted-text-1.txt",
            "3 lines",
            vec![Span::plain("   ")],
            80,
        )],
        80,
    );
    assert!(row[0].ends_with("attached as pasted-text-1.txt"));
}

fn machines() -> Vec<Machine> {
    vec![
        Machine {
            name: "box".to_owned(),
            ssh: "dev@box.example.com".to_owned(),
            os: "Linux x86_64".to_owned(),
            dir: "~/work".to_owned(),
            executor: Some(("executor 1.2.0".to_owned(), true)),
            row: MachineRow::Ready {
                latency_ms: Some(42),
            },
        },
        Machine {
            name: "lab".to_owned(),
            ssh: "lab".to_owned(),
            os: String::new(),
            dir: machine_rows::REMOTE_HOME.to_owned(),
            executor: None,
            row: MachineRow::Connecting {
                step: "uploading the executor".to_owned(),
                size: Some(" (89 MB)".to_owned()),
                elapsed: "3s".to_owned(),
                steps: vec![
                    ("checked".to_owned(), Phase::Done),
                    ("uploading".to_owned(), Phase::Now),
                    ("handshake".to_owned(), Phase::Later),
                ],
            },
        },
    ]
}

#[test]
fn the_machines_list_has_a_row_a_machine_and_the_chosen_one_s_details() {
    let t = rich();
    let all = machines();
    let got = lines(
        machine_rows::rows(&t, &all, &[], 0, Some(Chosen::Machine(1))),
        120,
    );
    assert!(got[0].contains("Machines") && got[0].contains("1 of 2 connected"));
    assert!(
        got.iter()
            .any(|l| l.contains("● box") && l.ends_with("42ms"))
    );
    assert!(
        got.iter()
            .any(|l| l.contains("✓ checked   ○ uploading   handshake"))
    );
    assert!(got.last().is_some_and(|l| l.contains("+ Add a machine")));
    let dock = lines(machine_rows::dock_row(&t, &all).into_iter().collect(), 120);
    assert!(
        dock[0].contains("lab  uploading the executor (89 MB) · 3s"),
        "{dock:?}"
    );
    assert!(!dock[0].contains("box"));
    assert_eq!(
        machine_rows::where_(&["box", "lab"]),
        Some("@box +1".to_owned())
    );
    assert_eq!(machine_rows::where_(&[]), None);
}
