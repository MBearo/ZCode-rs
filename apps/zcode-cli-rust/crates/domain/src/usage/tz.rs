//! Node `resolveTzOffsetMs`: a time zone's UTC offset at one instant, as `Intl`
//! resolves it (IANA names in any case, `±HH:MM` offsets), else 0.
use chrono::{Offset, TimeZone};
use chrono_tz::Tz;

fn named(name: &str) -> Option<Tz> {
    name.parse::<Tz>().ok().or_else(|| {
        chrono_tz::TZ_VARIANTS
            .iter()
            .copied()
            .find(|tz| tz.name().eq_ignore_ascii_case(name))
    })
}

/// `±HH`, `±HHMM` or `±HH:MM` in seconds.
fn fixed(name: &str) -> Option<i64> {
    let (sign, rest) = match name.as_bytes().first()? {
        b'+' => (1, &name[1..]),
        b'-' => (-1, &name[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    let valid = digits.chars().all(|c| c.is_ascii_digit())
        && matches!(digits.len(), 2 | 4)
        && (rest.len() == digits.len() || rest.len() == 5 && rest.as_bytes()[2] == b':');
    if !valid {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = match &digits[2..] {
        "" => 0,
        text => text.parse().ok()?,
    };
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 3600 + minutes * 60))
}

/// The offset in milliseconds at `at_ms`; unknown zones are 0 like Node's fallback.
pub fn offset_ms(time_zone: &str, at_ms: i64) -> i64 {
    if let Some(seconds) = fixed(time_zone) {
        return seconds * 1000;
    }
    let Some(tz) = named(time_zone) else {
        return 0;
    };
    let Some(at) = chrono::DateTime::from_timestamp_millis(at_ms) else {
        return 0;
    };
    let seconds = tz
        .offset_from_utc_datetime(&at.naive_utc())
        .fix()
        .local_minus_utc();
    i64::from(seconds) * 1000
}
