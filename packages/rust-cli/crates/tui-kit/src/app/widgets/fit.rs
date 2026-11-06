//! 把字放进格子（docs/specs/rust-cli/tui-kit.md §4.4）：截断、补齐、正文的量度、数与时间的写法。transcript 的各个小部件
//! 都用它们，两套界面一样。
//! 移植自 Puffin 的 shell/consts.rs（`clip`、`pad`、`pad_left`、`fixed`、`tokens`、`thought_time`、
//! `MEASURE_*`）、shell/stats.rs（`count`、`rate`）、shell/tools.rs（`time`）、shell/gather.rs（`width`、`cut`）与
//! shell/tools/text.rs（`cut_text`、`java_space`）的纯函数部分。
use crate::app::element::{Element, measure};
use crate::render::Span;
use crate::render::width::{display_width, string_width};

/// 正文折在 min(width − 5, 96)：内容从第 3 列起，两列边距。
pub const MEASURE_MAX: usize = 99;
pub const MEASURE_MARGIN: usize = 2;

/// 量过的一块：至多 [`MEASURE_MAX`] 格。
pub fn measured(el: Element) -> Element {
    measure(el, MEASURE_MAX, MEASURE_MARGIN)
}

/// `text` 截到 `cols` 格，截断时最后一格是 `…`。
pub fn clip(text: &str, cols: usize) -> String {
    if string_width(text) <= cols {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = string_width(ch.encode_utf8(&mut [0; 4]));
        if w + cw < cols {
            out.push(ch);
            w += cw;
        }
    }
    out.push('…');
    out
}

/// `text` 用空格补到 `cols` 格。
pub fn pad(text: &str, cols: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(cols.saturating_sub(string_width(text)))
    )
}

/// `text` 在 `cols` 格里右对齐。
pub fn pad_left(text: &str, cols: usize) -> String {
    format!(
        "{}{text}",
        " ".repeat(cols.saturating_sub(string_width(text)))
    )
}

/// Java 的 `Character.isWhitespace`：Unicode 的空白，不含不换行的空格（U+00A0、U+2007、U+202F）与 U+0085，另加
/// U+001C–U+001F。
pub fn java_space(c: char) -> bool {
    match c {
        '\u{1c}'..='\u{1f}' => true,
        '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}' => false,
        _ => c.is_whitespace(),
    }
}

/// 宽度 `cols` 里截断的 `text`：去掉末尾空白再加 `…`（只在放不下时用）。
pub fn cut_text(text: &str, cols: usize) -> String {
    let c = clip(text, cols);
    let body = c.strip_suffix('…').unwrap_or(&c);
    format!("{}…", body.trim_end_matches(java_space))
}

/// 段的总宽度（格）。
pub fn width(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}

/// `spans` 放进至多 `room` 格：超出时截断并以 `…` 结尾，截断处之前的墨色不变。`room` 可以是负的（什么都放不下）。
pub fn cut(spans: &[Span], room: i64) -> Vec<Span> {
    if width(spans) as i64 <= room {
        return spans.to_vec();
    }
    let mut out = Vec::new();
    let mut used: i64 = 0;
    for sp in spans {
        let w = string_width(&sp.text) as i64;
        if used + w < room {
            out.push(sp.clone());
            used += w;
        } else {
            // 按格截：宽字符放不下就不放。
            let mut text = String::new();
            for ch in sp.text.chars() {
                let cw = display_width(ch) as i64;
                if used + cw > room - 1 {
                    break;
                }
                text.push(ch);
                used += cw;
            }
            text.push('…');
            out.push(Span::new(text, sp.style));
            break;
        }
    }
    out
}

/// Java 的 `Math.round`：`floor(x + 0.5)`；NaN 是 0。
pub fn round(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

/// `v` 保留 `decimals` 位小数，按 Scala / JVM 的 `%.Nf`：在最短的十进制表示上四舍五入（.5 向上）。
pub fn fixed(v: f64, decimals: usize) -> String {
    let negative = v < 0.0;
    let shortest = format!("{}", v.abs());
    let (int, frac) = shortest.split_once('.').unwrap_or((&shortest, ""));
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes()).map(|b| b - b'0').collect();
    let point = int.len();
    // 补到要的位数之后再多一位，按那一位进位。
    while digits.len() < point + decimals + 1 {
        digits.push(0);
    }
    let up = digits[point + decimals] >= 5;
    digits.truncate(point + decimals);
    if up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let int_len = digits.len() - decimals;
    let mut out = String::new();
    if negative && digits.iter().any(|d| *d != 0) {
        out.push('-');
    }
    out.extend(digits[..int_len].iter().map(|d| char::from(b'0' + d)));
    if decimals > 0 {
        out.push('.');
        out.extend(digits[int_len..].iter().map(|d| char::from(b'0' + d)));
    }
    out
}

/// 压缩行说的 token 数：950、7.9k、52.3k、1.2M。
pub fn tokens(n: f64) -> String {
    let short = |v: f64, unit: &str| {
        let s = fixed(v, 1);
        let s = s.strip_suffix(".0").unwrap_or(&s).to_owned();
        format!("{s}{unit}")
    };
    if n < 1000.0 {
        format!("{}", n as i64)
    } else if n < 1_000_000.0 {
        short(n / 1000.0, "k")
    } else {
        short(n / 1_000_000.0, "M")
    }
}

/// 去掉小数末尾的 0 与小数点（只在有小数点时）。
fn trimmed(s: String) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

/// 一个计数：`950`、`1.2k`、`124k`、`1.84M`（docs/puffin-tui.md §4.2「多少」）。
pub fn count(n: f64) -> String {
    if n < 1000.0 {
        round(n).to_string()
    } else if n < 10000.0 {
        format!("{}k", trimmed(fixed(n / 1000.0, 1)))
    } else if round(n / 1000.0) < 1000 {
        format!("{}k", round(n / 1000.0))
    } else {
        format!("{}M", trimmed(fixed(n / 1e6, 2)))
    }
}

/// 速度：整数。
pub fn rate(r: f64) -> String {
    round(r).to_string()
}

/// 思考用了多久，思考行这样说：整秒向上取，至少 `1s`；过了一分钟 `1m 05s`（§4.2）。
pub fn thought_time(ms: f64) -> String {
    let secs = ((ms / 1000.0).ceil() as i64).max(1);
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// 调用用了多久：10 秒以下一位小数，一分钟以下整秒，之后 `1m 05s`。
pub fn time(ms: f64) -> String {
    if ms < 10000.0 {
        format!("{}s", fixed(ms / 1000.0, 1))
    } else if ms < 60000.0 {
        format!("{}s", round(ms / 1000.0))
    } else {
        let secs = round(ms / 1000.0);
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}
