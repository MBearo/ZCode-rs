//! HTTP 传输的请求构造（ext.md §3.2）。

use super::*;

/// 缺陷 R2-13：配置给的 header（常是 Authorization）原来按普通值交给 net；现在是秘密值，Debug 里看不到。
#[test]
fn configured_headers_are_secret() {
    let headers = BTreeMap::from([("authorization".to_owned(), "Bearer t0k-secret".to_owned())]);
    let shown = format!(
        "{:?}",
        with_headers(HttpRequest::get("http://h/sse"), &headers)
    );
    assert!(shown.contains("authorization"), "{shown}");
    assert!(!shown.contains("t0k-secret"), "{shown}");
}
