//! Z.ai 业务 token 的响应解析（platform.md §5.4）。

use base64::Engine;
use serde_json::json;

use super::*;

fn jwt(exp: i64) -> String {
    let payload =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({"exp": exp}).to_string());
    format!("header.{payload}.signature")
}

#[test]
fn the_earliest_deadline_wins() {
    let started = 1_000_000;
    // JWT 10 分钟后过期，expires_in 1 小时：取 JWT。
    let token = jwt(1_000 + 600);
    let reply =
        Reply::Json(json!({"code": 0, "data": {"access_token": token, "expires_in": 3600}}));
    let value = parse(&reply, started, started).unwrap();
    assert_eq!(value.expires_at, Some(1_600_000));
    assert_eq!(value.refresh_at, 1_600_000 - REFRESH_LEAD_MS);
    // 没有有效期：300 秒。
    let reply = Reply::Json(json!({"data": {"accessToken": " opaque "}}));
    let value = parse(&reply, started, started).unwrap();
    assert_eq!(value.token.expose(), "opaque");
    assert_eq!(value.expires_at, None);
    assert_eq!(value.refresh_at, started + UNKNOWN_TTL_MS - REFRESH_LEAD_MS);
}

#[test]
fn failures_and_expired_tokens_are_rejected() {
    let started = 1_000_000;
    for reply in [
        json!({"code": 401, "data": {"access_token": "t"}}),
        json!({"code": "0", "data": {}}),
        json!({"data": {"access_token": 5}}),
        json!({"data": {"access_token": jwt(-5)}}),
        json!({"data": {"access_token": "t", "expires_in": 0}}),
    ] {
        assert!(
            parse(&Reply::Json(reply.clone()), started, started).is_none(),
            "{reply}"
        );
    }
    assert!(parse(&Reply::NotFound, started, started).is_none());
    assert!(
        parse(
            &Reply::Json(json!({"code": "200", "data": {"access_token": "t"}})),
            started,
            started
        )
        .is_some()
    );
}
