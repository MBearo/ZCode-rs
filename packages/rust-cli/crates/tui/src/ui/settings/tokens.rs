//! token 数的写法与读法（自动压缩那一行；照抄 Puffin `shell/auto_compact.rs` 的 `grouped`、`tokens_exact`、`parse_tokens`，
//! 两套界面不能互相依赖）。
/// `123,456`：千位用逗号分开。
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `400k`、`1M`、`1.5M`，否则 `123,456`：自动压缩那一行精确地写一个 token 数。
pub fn exact(n: u64) -> String {
    if n >= 1_000_000 && n.is_multiple_of(1_000_000) {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000_000 && n.is_multiple_of(100_000) {
        format!("{}.{}M", n / 1_000_000, n % 1_000_000 / 100_000)
    } else if n.is_multiple_of(1000) {
        format!("{}k", n / 1000)
    } else {
        grouped(n)
    }
}

/// `400000`、`400,000`、`400k`、`1.5M`（不分大小写，可以有空格）→ 一个正整数的 token 数，至多 `i32::MAX`（与 Scala 的
/// `Int` 相同），按十进制精确地四舍五入。
pub fn parse(text: &str) -> Option<u64> {
    let s: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',' && *c != '_')
        .collect::<String>()
        .to_lowercase();
    let (number, mult) = match s.strip_suffix('m') {
        Some(n) => (n, 1_000_000u128),
        None => match s.strip_suffix('k') {
            Some(n) => (n, 1000u128),
            None => (s.as_str(), 1u128),
        },
    };
    let (int, frac) = match number.split_once('.') {
        Some((i, f)) if !f.is_empty() => (i, f),
        Some(_) => return None,
        None => (number, ""),
    };
    if int.is_empty()
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // 小数位多于 30 位的部分不影响结果（上限与四舍五入只看前面的位）。
    let frac = &frac[..frac.len().min(30)];
    let scale = 10u128.checked_pow(u32::try_from(frac.len()).ok()?)?;
    let digits = int.trim_start_matches('0');
    let whole: u128 = if digits.is_empty() {
        0
    } else {
        digits.parse().ok()?
    };
    let part: u128 = if frac.is_empty() {
        0
    } else {
        frac.parse().ok()?
    };
    let num = whole
        .checked_mul(scale)?
        .checked_add(part)?
        .checked_mul(mult)?;
    // num / scale 是精确值：要在 [1, i32::MAX] 里。
    let max = u128::try_from(i32::MAX).ok()?;
    if num < scale || num > max.checked_mul(scale)? {
        return None;
    }
    let rounded = (num * 2 + scale) / (scale * 2);
    u64::try_from(rounded).ok()
}
