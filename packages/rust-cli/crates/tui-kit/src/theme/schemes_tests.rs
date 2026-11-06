//! 内置主题（docs/puffin-tui.md §4.2「主题」）：每个都有两侧，只用它有的墨，画在哪里都在底线之上。
//! 移植自 packages/tui/tui/test/src/zcode/tui/SchemesSuite.scala（feat/tui a2bf878b34）。
use super::rgb::Rgb;
use super::schemes;
use super::theme::Theme;
use crate::render::{Color, Style};

fn rgb_of(c: Color) -> Rgb {
    match c {
        Color::TrueColor(r, g, b) => Rgb::new(r, g, b),
        other => panic!("not truecolour: {other:?}"),
    }
}

fn fg_of(st: Style) -> Rgb {
    rgb_of(st.fg_color())
}

fn hex(s: &str) -> Rgb {
    Rgb::parse_hex(s).unwrap()
}

// 与设计稿相同的两个示例终端：地是终端的时候，主题的墨画在它们上面。
fn dark_terminal() -> (Rgb, Rgb) {
    (hex("#d6d5d0"), hex("#1b1c1f"))
}
fn light_terminal() -> (Rgb, Rgb) {
    (hex("#2a2c30"), hex("#fbfaf7"))
}

#[test]
fn the_list_is_terminal_first_then_the_named_themes_in_the_specs_order() {
    assert_eq!(
        schemes::ids(),
        [
            "terminal",
            "puffin",
            "solarized",
            "gruvbox",
            "nord",
            "dracula",
            "catppuccin",
            "tokyonight",
            "rosepine",
            "one",
            "github",
            "everforest",
            "kanagawa",
            "ayu",
            "flexoki",
            "modus",
            "selenized",
            "nightowl",
        ]
    );
    // id 表与主题表的次序一致。
    let from_all: Vec<&str> = schemes::all().iter().map(|s| s.id).collect();
    assert_eq!(&schemes::ids()[1..], from_all.as_slice());
    let mut distinct = schemes::ids().to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), schemes::ids().len());
    assert!(
        schemes::ids().iter().all(|id| id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())),
        "{:?}",
        schemes::ids()
    );
    assert_eq!(schemes::name("terminal"), "Terminal");
    assert_eq!(schemes::name("rosepine"), "Rosé Pine");
}

#[test]
fn a_theme_is_found_by_its_id_or_its_name_ignoring_case() {
    assert_eq!(schemes::find("gruvbox"), Some("gruvbox"));
    assert_eq!(schemes::find("Tokyo Night"), Some("tokyonight"));
    assert_eq!(schemes::find("  ATOM ONE "), Some("one"));
    assert_eq!(schemes::find("terminal"), Some("terminal"));
    assert_eq!(schemes::find("tokyo"), None);
    assert_eq!(schemes::find(""), None);
}

#[test]
fn each_side_is_the_side_it_says() {
    for s in schemes::all() {
        let (l, d) = (s.light.palette, s.dark.palette);
        assert!(l.bg.lightness() >= 0.5, "{} light bg {}", s.id, l.bg.hex());
        assert!(d.bg.lightness() < 0.5, "{} dark bg {}", s.id, d.bg.hex());
        assert!(l.fg.lightness() < l.bg.lightness(), "{}", s.id);
        assert!(d.fg.lightness() > d.bg.lightness(), "{}", s.id);
    }
}

#[test]
fn every_ink_a_role_names_exists_on_both_sides() {
    for s in schemes::all() {
        for side in [s.light, s.dark] {
            for name in s.roles.all().into_iter().chain(s.syntax.all()) {
                assert!(
                    side.palette.has_ink(name),
                    "{} {}: no ink {name}",
                    s.id,
                    side.name
                );
            }
        }
    }
}

#[test]
fn nords_light_side_is_snow_storm_with_polar_night_text_and_the_same_inks() {
    let nord = schemes::by_id("nord").unwrap();
    let l = nord.light.palette;
    assert_eq!(
        (l.bg.hex(), l.surface.hex(), l.fg.hex(), l.muted.hex()),
        (
            "#eceff4".to_owned(),
            "#d8dee9".to_owned(),
            "#2e3440".to_owned(),
            "#4c566a".to_owned()
        )
    );
    assert_eq!(l.inks, nord.dark.palette.inks);
    assert_eq!(nord.sides(), "light · dark");
}

#[test]
fn the_sides_as_the_list_shows_them() {
    let sides = |id: &str| schemes::by_id(id).unwrap().sides();
    assert_eq!(sides("catppuccin"), "Latte · Mocha");
    assert_eq!(sides("nightowl"), "Light Owl · Night Owl");
    assert_eq!(sides("dracula"), "Alucard · Dracula");
    assert_eq!(sides("puffin"), "Day · Night");
}

/// `t` 的每个 token 在 bg 与 panel 上都达到它的底线；灰最多与字本身一样清楚（它向字抬）。
fn readable(t: &Theme, what: &str) -> Vec<String> {
    let grounds = [t.bg.unwrap(), rgb_of(t.panel.unwrap())];
    let fg = t.fg.unwrap();
    let mut out = Vec::new();
    let mut check = |name: &str, c: Rgb, floor: f64, grey: bool| {
        for g in grounds {
            let need = if grey {
                floor.min(fg.contrast(g))
            } else {
                floor
            };
            if c.contrast(g) < need - 1e-9 {
                out.push(format!(
                    "{what} {name} {} on {}: {:.2} < {need:.2}",
                    c.hex(),
                    g.hex(),
                    c.contrast(g)
                ));
            }
        }
    };
    check("fg2", fg_of(t.fg2), Theme::FLOOR_TEXT, true);
    check("brand", rgb_of(t.brand), Theme::FLOOR_TEXT, false);
    check("fg3", fg_of(t.fg3), Theme::FLOOR_INK, true);
    let inks = [
        ("code", t.code),
        ("ok", t.ok),
        ("err", t.err),
        ("skill", t.skill),
        ("file", t.file),
        ("work", t.work),
        ("agent", t.agent),
    ];
    for (n, c) in inks {
        check(n, rgb_of(c), Theme::FLOOR_INK, false);
    }
    let y = t.syntax;
    let syntax = [
        ("keyword", y.keyword),
        ("string", y.string),
        ("number", y.number),
        ("function", y.function),
        ("type", y.ty),
        ("constant", y.constant),
        ("key", y.key),
    ];
    for (n, c) in syntax {
        check(n, rgb_of(c), Theme::FLOOR_INK, false);
    }
    out
}

#[test]
fn every_side_of_every_theme_reads_above_the_floor_on_its_own_ground_and_on_a_terminals() {
    let mut failed = Vec::new();
    for s in schemes::all() {
        for dark in [false, true] {
            let name = s.side(dark).name;
            failed.extend(readable(
                &Theme::scheme(s, dark, None),
                &format!("{} {name}", s.id),
            ));
            let terminal = if dark {
                dark_terminal()
            } else {
                light_terminal()
            };
            failed.extend(readable(
                &Theme::scheme(s, dark, Some(terminal)),
                &format!("{} {name} on the terminal", s.id),
            ));
        }
    }
    assert_eq!(failed.join("\n"), "");
}
