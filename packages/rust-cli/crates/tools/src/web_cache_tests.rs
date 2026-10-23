use super::*;

fn page(content: &str) -> Page {
    Page {
        code: 200,
        content: content.to_owned(),
        content_type: "text/html".to_owned(),
    }
}

/// 15 分钟过期按注入的时间算（F7：以前按 tokio 的 `Instant`）；取到时不刷新存入时间（LRU 只按存入算 TTL）。
#[test]
fn pages_expire_fifteen_minutes_after_they_were_stored() {
    let mut cache = PageCache::default();
    let at = 1_760_000_000_000;
    cache.put("https://example.com/a", page("a"), at);
    assert_eq!(
        cache.get("https://example.com/a", at + TTL_MS - 1),
        Some(page("a"))
    );
    assert_eq!(cache.get("https://example.com/a", at + TTL_MS), None);
    // 过期的项被拿掉了。
    assert_eq!(cache.get("https://example.com/a", at), None);
}

/// 时钟回拨时不当作过期（saturating）。
#[test]
fn a_clock_that_goes_back_keeps_the_page() {
    let mut cache = PageCache::default();
    cache.put("u", page("x"), 2_000);
    assert_eq!(cache.get("u", 1_000), Some(page("x")));
}

/// 超过总量时先丢最久没用的。
#[test]
fn the_least_recently_used_page_goes_first() {
    let mut cache = PageCache::default();
    let half = "x".repeat(MAX_BYTES / 2);
    cache.put("a", page(&half), 0);
    cache.put("b", page(&half), 0);
    assert!(cache.get("a", 1).is_some());
    cache.put("c", page("c"), 2);
    assert!(cache.get("b", 3).is_none());
    assert!(cache.get("a", 3).is_some());
    assert!(cache.get("c", 3).is_some());
}
