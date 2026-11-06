use std::time::{Duration, Instant};

use rust_cli_tui_kit::render::{Color, StyledLine};
use rust_cli_tui_kit::theme::rgb::Rgb;
use rust_cli_tui_kit::theme::scheme::ThemeChoice;
use rust_cli_tui_kit::theme::theme::Theme;

use super::press::{self, Press, SECS};
use super::render::{self, Cell};
use super::{Mark, Stage, cached, stage};

fn rich(bg: Rgb, fg: Rgb) -> Theme {
    Theme::resolve(Some(fg), Some(bg), true, &ThemeChoice::default(), None)
}

fn night() -> Theme {
    rich(Rgb::new(0x10, 0x16, 0x1a), Rgb::new(0xe4, 0xdf, 0xd5))
}

fn day() -> Theme {
    rich(Rgb::new(0xf2, 0xf7, 0xf8), Rgb::new(0x22, 0x28, 0x2f))
}

/// 96 × 28、头有 3 行：空白处第 4–22 行，10 行高的 Z，居中。
fn stage96() -> Stage {
    stage(&night(), 96, 4, 24).unwrap()
}

fn painted(mark: &mut Mark, stage: &Stage, now: Instant) -> Vec<StyledLine> {
    let mut lines = vec![StyledLine::default(); stage.top + stage.free + 2];
    mark.paint(&mut lines, stage, now);
    lines
}

/// 一个点在 Z 上的格子：斜杠的中间。
fn middle(stage: &Stage) -> (u16, u16) {
    let x = stage.col + (stage.rows * 2 * 256 / 218) / 2;
    ((x as u16), (stage.row + stage.rows / 2) as u16)
}

/// 只有一次按压、从 0 秒起时 `t` 秒（显著度 `sal`）的格子；`point` 相对 Z 的外框中心。
fn frame(stage: &Stage, point: Option<[f64; 2]>, t: f64, sal: f64) -> Vec<Option<Cell>> {
    let mut slot = None;
    let model = cached(&mut slot, stage.rows);
    let pose = point.and_then(|[x, y]| {
        let press = Press {
            start: 0.0,
            point: [model.cx + x, model.cy + y],
        };
        press::pose(model, &[press], (0.0, SECS), t)
    });
    let dots = render::dots(model, stage.canvas(), pose);
    render::cells(&dots, stage.canvas(), &stage.inks, sal)
}

fn differ(a: &[Option<Cell>], b: &[Option<Cell>]) -> usize {
    a.iter()
        .zip(b)
        .filter(|(x, y)| x.map(|c| c.glyph) != y.map(|c| c.glyph))
        .count()
}

#[test]
fn the_stage_follows_the_free_rows() {
    let s = stage96();
    assert_eq!((s.rows, s.row, s.col), (10, 8, 36));
    assert_eq!((s.top, s.free, s.cols), (4, 19, 96));
    // 再高也不超过 12 行。
    assert_eq!(stage(&night(), 200, 4, 80).unwrap().rows, 12);
    // 不到 6 行不画；窄的终端不裁 Z。
    assert!(stage(&night(), 96, 4, 14).is_none());
    assert_eq!(stage(&night(), 96, 4, 16).unwrap().rows, 6);
    let narrow = stage(&night(), 24, 4, 40).unwrap();
    assert!(
        narrow.rows < 12 && narrow.rows * 2 * 256 / 218 <= 20,
        "{narrow:?}"
    );
    assert!(stage(&night(), 16, 4, 40).is_none());
    // plain 主题不画。
    assert!(stage(&Theme::PLAIN, 96, 4, 24).is_none());
}

/// 静止的 Z：盲文字符，只设前景色，都是静止墨 `mix(bg, fg3, 0.42)`；Z 的上下没有东西。深浅主题都是。
#[test]
fn the_rest_mark_is_braille_in_the_faint_ink() {
    for theme in [night(), day()] {
        let s = stage(&theme, 96, 4, 24).unwrap();
        let lines = painted(&mut Mark::default(), &s, Instant::now());
        let fg3 = match theme.fg3.fg_color() {
            Color::TrueColor(r, g, b) => [r, g, b],
            other => panic!("{other:?}"),
        };
        let bg = theme.bg.unwrap();
        let ink = Color::TrueColor(
            ((f64::from(bg.r) + (f64::from(fg3[0]) - f64::from(bg.r)) * 0.42) + 0.5).floor() as u8,
            ((f64::from(bg.g) + (f64::from(fg3[1]) - f64::from(bg.g)) * 0.42) + 0.5).floor() as u8,
            ((f64::from(bg.b) + (f64::from(fg3[2]) - f64::from(bg.b)) * 0.42) + 0.5).floor() as u8,
        );
        let spans: Vec<_> = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|span| !span.text.trim().is_empty())
            .collect();
        assert!(spans.len() > 10);
        for span in &spans {
            assert!(
                span.text
                    .chars()
                    .all(|c| c == ' ' || ('\u{2801}'..='\u{28ff}').contains(&c)),
                "{:?}",
                span.text
            );
            assert_eq!(span.style.fg_color(), ink);
            assert_eq!(span.style.bg_color(), Color::Default);
        }
        assert!(lines[s.row - 1].spans.is_empty());
        assert!(lines[s.row + s.rows].spans.is_empty());
    }
}

/// 按在哪里（相对中心：设计稿比较用的那一点、斜杠中间、左上与右下的横上），第一帧与最后一帧都与静止的 Z 相同，中间在动。
#[test]
fn a_press_starts_and_ends_on_the_rest_z() {
    // 8、10、12 行高。
    for s in [
        stage(&night(), 96, 4, 20).unwrap(),
        stage96(),
        stage(&night(), 140, 4, 46).unwrap(),
    ] {
        let rest = frame(&s, None, 0.0, 0.0);
        let (half_w, half_h) = (s.rows as f64 * 2.0 * 256.0 / 218.0, s.rows as f64 * 2.0);
        for point in [
            [10.0, -8.0],
            [0.0, 0.0],
            [-0.8 * half_w, -0.85 * half_h],
            [0.8 * half_w, 0.85 * half_h],
        ] {
            let start = frame(&s, Some(point), 0.001, 0.0);
            let end = frame(&s, Some(point), SECS - 0.001, 0.0);
            let middle = frame(&s, Some(point), 0.2, 0.0);
            assert_eq!(differ(&start, &rest), 0, "{point:?} start");
            assert_eq!(differ(&end, &rest), 0, "{point:?} end");
            assert!(differ(&middle, &rest) > 30, "{point:?} moves");
        }
    }
}

#[test]
fn only_a_press_on_the_z_plays() {
    let s = stage96();
    let mut mark = Mark::default();
    let now = Instant::now();
    // 外框的左上角之外、上面的一行、Z 右上方的空处（第一行的左边是横，第二条路径在右边）。
    assert!(!mark.press(&s, 0, 0, now));
    assert!(!mark.press(&s, s.col as u16, (s.row - 1) as u16, now));
    assert!(!mark.press(&s, s.col as u16, (s.row + s.rows / 2) as u16, now));
    assert!(!mark.moving(now));
    let (x, y) = middle(&s);
    assert!(mark.press(&s, x, y, now));
    assert!(mark.moving(now));
    // 被层盖住的行点了不算。
    let covered = Stage {
        shown: usize::from(y),
        ..s
    };
    assert!(!Mark::default().press(&covered, x, y, now));
}

/// 点亮、动起来，播完、停住、落回之后回到完全相同的静止画面并停止重画。
#[test]
fn a_motion_lights_up_and_settles_back_to_the_exact_rest() {
    let s = stage96();
    let mut mark = Mark::default();
    let at = Instant::now();
    let rest = painted(&mut mark, &s, at);
    let (x, y) = middle(&s);
    assert!(mark.press(&s, x, y, at));
    let during = painted(&mut mark, &s, at + Duration::from_millis(200));
    assert_ne!(during, rest);
    let quiet = SECS + 0.3 + 0.9;
    assert!(mark.moving(at + Duration::from_secs_f64(quiet - 0.01)));
    let end = at + Duration::from_secs_f64(quiet);
    assert!(!mark.moving(end));
    assert_eq!(painted(&mut mark, &s, end), rest);
}

/// 动着时再点：新的按压立刻开始，与前一次相加（不排队）；离挡块还远时，姿态正好是两次各自的姿态之和。
#[test]
fn a_press_while_moving_adds_its_spring() {
    let s = stage96();
    let (x, y) = middle(&s);
    let (lx, ly) = (s.col as u16 + 3, s.row as u16);
    let mut mark = Mark::default();
    let at = Instant::now();
    assert!(mark.press(&s, x, y, at));
    let soon = at + Duration::from_millis(200);
    assert!(mark.press(&s, lx, ly, soon));
    assert_eq!(mark.presses.len(), 2);
    assert!((mark.presses[1].start - 0.2).abs() < 1e-9);
    // 第二次按压一按下就改了画面：与只按一次的不同。
    let mut once = Mark::default();
    assert!(once.press(&s, x, y, at));
    let next = soon + Duration::from_millis(60);
    assert_ne!(painted(&mut mark, &s, next), painted(&mut once, &s, next));
    // 姿态相加。
    let model = cached(&mut mark.model, s.rows);
    let t = 0.45;
    let pose = |presses: &[Press]| press::pose(model, presses, (0.0, 0.2 + SECS), t).unwrap();
    let (both, a, b) = (
        pose(&mark.presses),
        pose(&mark.presses[..1]),
        pose(&mark.presses[1..]),
    );
    for k in 0..2 {
        assert!((both.turn[k] - a.turn[k] - b.turn[k]).abs() < 1e-12);
    }
    assert!((both.sink - a.sink - b.sink).abs() < 1e-12);
    // 一直动到第二次按压结束，再停住、落回。
    let quiet = 0.2 + SECS + 0.3 + 0.9;
    assert!(mark.moving(at + Duration::from_secs_f64(quiet - 0.01)));
    assert!(!mark.moving(at + Duration::from_secs_f64(quiet + 0.001)));
    // 结束后再按开始新的一串，前面的都已经不在了。
    let later = at + Duration::from_secs(5);
    assert!(mark.press(&s, x, y, later));
    assert_eq!(mark.presses.len(), 1);
}

/// 一次按压到不了挡块；在同一点连按，倾斜与下沉停在挡块（一次的 1.25 倍）上。
#[test]
fn stacked_presses_stop_at_the_stop() {
    let s = stage96();
    let mut slot = None;
    let model = cached(&mut slot, s.rows);
    let corner = [model.cx - 0.8 * model.cx, model.cy - 0.85 * model.cy];
    let one = Press {
        start: 0.0,
        point: corner,
    };
    let peak = |presses: &[Press]| {
        (1..130)
            .map(|k| press::pose(model, presses, (0.0, 2.0), f64::from(k) / 100.0).unwrap())
            .fold([0.0f64; 3], |m, p| {
                [
                    m[0].max(p.turn[0].abs()),
                    m[1].max(p.turn[1].abs()),
                    m[2].max(p.sink.abs()),
                ]
            })
    };
    let single = peak(&[one]);
    let stops = [0.62 * 1.25, 0.62 * 1.25, 0.16 * model.zh * 1.25];
    for k in 0..3 {
        assert!(single[k] < stops[k] * 0.9, "{k}: {single:?}");
    }
    let many = peak(&[one; 5]);
    for k in 0..2 {
        assert!((many[k] - stops[k]).abs() < 1e-12, "{k}: {many:?}");
    }
}
