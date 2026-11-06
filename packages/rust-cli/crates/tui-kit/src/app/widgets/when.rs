//! 说时间的话（docs/puffin-tui.md §4.2「会话列表」「提及（@）」「回退」；docs/specs/rust-cli/tui-kit.md §4.4）：列表一列的年龄
//! （`now`、`12m`、`5h`、`3d`、`2w`、`Sep 3`），上次用是多久以前，会话开始的时候，`@` 面板的日子。纯的。
//!
//! 本地时间不读系统：调用方给 `offset`（毫秒时刻 → 本地相对 UTC 的分钟数），日期按公历换算。
//! 移植自 Puffin 的 shell/resume.rs（`Local`、`local`、`calendar_days`、`age`、`last_active`、`started`、
//! `short_date`）与 shell/at/rows.rs（`hh_mm`、`day_age`、`day_age_long`）。
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

const DAY_MS: f64 = 86_400_000.0;

/// 年龄一列的宽度。
pub const AGE_WIDTH: usize = 6;
/// 年龄、一个空格、记号与一个空格：标题从这里开始。
pub const TITLE_COL: usize = AGE_WIDTH + 3;

/// 本地时间的一刻：年、月（0 起）、日、星期（0 是周日）、时、分；`midnight` 是那天本地零点的 epoch 毫秒。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Local {
    pub year: i64,
    pub month: usize,
    pub day: u32,
    pub weekday: usize,
    pub hour: u32,
    pub minute: u32,
    pub midnight: f64,
}

/// 从 1970-01-01 起的天数的公历日期（Howard Hinnant 的 civil_from_days）。
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `ms` 在本地（相对 UTC 偏 `offset(ms)` 分钟）的时刻。
pub fn local(ms: f64, offset: &dyn Fn(f64) -> i32) -> Local {
    let shift = f64::from(offset(ms)) * 60_000.0;
    let at = ms + shift;
    let days = (at / DAY_MS).floor() as i64;
    let in_day = at - days as f64 * DAY_MS;
    let (year, month, day) = civil(days);
    let minutes = (in_day / 60_000.0).floor() as u32;
    Local {
        year,
        month: (month - 1) as usize,
        day,
        weekday: (days + 4).rem_euclid(7) as usize,
        hour: minutes / 60,
        minute: minutes % 60,
        midnight: days as f64 * DAY_MS - shift,
    }
}

/// `then` 与 `now` 之间的本地零点数。
pub fn calendar_days(now: f64, then: f64, offset: &dyn Fn(f64) -> i32) -> i64 {
    ((local(now, offset).midnight - local(then, offset).midnight) / DAY_MS).round() as i64
}

/// 一行显示的年龄：`now`、`12m`、`5h`，然后是日历日 `3d`、周 `2w`，再是 `Sep 3`。
pub fn age(now: f64, then: f64, offset: &dyn Fn(f64) -> i32) -> String {
    let minutes = (now - then) / 60_000.0;
    if minutes < 1.0 {
        "now".to_owned()
    } else if minutes < 60.0 {
        format!("{}m", minutes as i64)
    } else if minutes < 1440.0 {
        format!("{}h", (minutes / 60.0) as i64)
    } else {
        let days = calendar_days(now, then, offset).max(1);
        if days < 7 {
            format!("{days}d")
        } else if days < 35 {
            format!("{}w", days / 7)
        } else {
            let d = local(then, offset);
            format!("{} {}", MONTHS[d.month], d.day)
        }
    }
}

/// 会话上次用是多久以前，输入框上方的那一行这样说。
pub fn last_active(now: f64, then: f64, offset: &dyn Fn(f64) -> i32) -> String {
    let minutes = (now - then) / 60_000.0;
    let n = |k: i64, unit: &str| format!("{k} {unit}{} ago", if k == 1 { "" } else { "s" });
    if minutes < 1.0 {
        "just now".to_owned()
    } else if minutes < 60.0 {
        n(minutes as i64, "minute")
    } else if minutes < 1440.0 {
        n((minutes / 60.0) as i64, "hour")
    } else {
        let days = calendar_days(now, then, offset).max(1);
        if days == 1 {
            "yesterday".to_owned()
        } else {
            n(days, "day")
        }
    }
}

/// `Wed 14 Oct, 21:10`：会话开始的时候，在它历史上面的头里。
pub fn started(ms: f64, offset: &dyn Fn(f64) -> i32) -> String {
    let d = local(ms, offset);
    format!(
        "{} {} {}, {:02}:{:02}",
        DAYS[d.weekday], d.day, MONTHS[d.month], d.hour, d.minute
    )
}

/// `14 Oct`，在细节行里。
pub fn short_date(ms: f64, offset: &dyn Fn(f64) -> i32) -> String {
    let d = local(ms, offset);
    format!("{} {}", d.day, MONTHS[d.month])
}

/// `18:24`。
pub fn hh_mm(ms: f64, offset: &dyn Fn(f64) -> i32) -> String {
    let t = local(ms, offset);
    format!("{:02}:{:02}", t.hour, t.minute)
}

/// 一行里对话最后的活动：`today`、`yesterday`、`3 days`、`2 months`。
pub fn day_age(now: f64, then: f64, offset: &dyn Fn(f64) -> i32) -> String {
    let days = calendar_days(now, then, offset).max(0);
    match days {
        0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        d if d < 60 => format!("{d} days"),
        d => format!("{} months", d / 30),
    }
}

/// 预览里的：`today, 18:24`、`yesterday, 09:10`、`3 days ago`。
pub fn day_age_long(now: f64, then: f64, offset: &dyn Fn(f64) -> i32) -> String {
    match calendar_days(now, then, offset).max(0) {
        0 => format!("today, {}", hh_mm(then, offset)),
        1 => format!("yesterday, {}", hh_mm(then, offset)),
        d => format!("{d} days ago"),
    }
}
