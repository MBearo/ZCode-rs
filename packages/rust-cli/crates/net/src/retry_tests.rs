use super::*;

#[test]
fn backoff_doubles_with_jitter_up_to_the_cap() {
    let policy = RetryPolicy::default();
    let ms = |attempt, random| policy.delay(attempt, None, random).map(|d| d.as_millis());
    assert_eq!(ms(1, 0.0), Some(500));
    assert_eq!(ms(1, 0.999_999), Some(625));
    assert_eq!(ms(2, 0.5), Some(1125));
    assert_eq!(ms(6, 0.0), Some(16_000));
    // 上限 32 秒在抖动之前生效：32 s × 1.25 = 40 s，仍小于 60 秒的等待上限。
    assert_eq!(ms(7, 0.0), Some(32_000));
    assert_eq!(ms(30, 1.0), Some(40_000));
    assert_eq!(ms(u32::MAX, 0.0), Some(32_000));
}

#[test]
fn retry_after_wins_when_longer_and_too_long_gives_up() {
    let policy = RetryPolicy::default();
    let secs = |s| Some(Duration::from_secs(s));
    assert_eq!(policy.delay(1, secs(3), 0.0), secs(3));
    // 服务端要求的等待比退避短时，取退避。
    assert_eq!(
        policy.delay(4, secs(1), 0.0),
        Some(Duration::from_millis(4000))
    );
    assert_eq!(policy.delay(1, secs(60), 0.0), secs(60));
    assert_eq!(policy.delay(1, secs(61), 0.0), None);
}

#[test]
fn retry_after_header_forms_and_precedence() {
    let now = httpdate::parse_http_date("Wed, 21 Oct 2015 07:27:00 GMT").unwrap();
    let parse = |pairs: &[(&str, &str)]| {
        retry_after(&ResponseHeaders::from_pairs(pairs.iter().copied()), now).map(|d| d.as_millis())
    };
    assert_eq!(
        parse(&[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")]),
        Some(60_000)
    );
    assert_eq!(
        parse(&[("retry-after", "Wed, 21 Oct 2015 07:00:00 GMT")]),
        Some(0)
    );
    assert_eq!(parse(&[("retry-after", " 2 ")]), Some(2000));
    assert_eq!(parse(&[("retry-after", "1.5")]), Some(1500));
    assert_eq!(parse(&[("retry-after", "-3")]), Some(0));
    assert_eq!(parse(&[("retry-after", "soon")]), None);
    assert_eq!(
        parse(&[("retry-after", "9"), ("retry-after-ms", "12.5")]),
        Some(13)
    );
    assert_eq!(parse(&[]), None);
}

#[test]
fn should_retry_header() {
    let header =
        |value: &str| should_retry(&ResponseHeaders::from_pairs([("x-should-retry", value)]));
    assert_eq!(header("true"), Some(true));
    assert_eq!(header(" false "), Some(false));
    assert_eq!(header("maybe"), None);
    assert_eq!(should_retry(&ResponseHeaders::default()), None);
}

#[test]
fn jitter_stays_in_range_and_varies() {
    let samples: Vec<f64> = (0..64).map(|_| jitter()).collect();
    assert!(samples.iter().all(|value| (0.0..1.0).contains(value)));
    assert!(samples.windows(2).any(|pair| pair[0] != pair[1]));
}
