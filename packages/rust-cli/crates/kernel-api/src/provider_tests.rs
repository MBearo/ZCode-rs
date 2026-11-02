//! 端点的查询与请求选项（工作线 F95，docs/specs/rust-cli/model.md §9）。

use serde_json::json;

use super::*;
use crate::model::RequestOptions;

/// 交给补丁的上限：没有时不写出，有时原样往返。
#[test]
fn the_provider_output_limit_is_optional_on_the_wire() {
    assert_eq!(
        serde_json::to_value(RequestOptions::default()).unwrap(),
        json!({})
    );
    let options = RequestOptions {
        provider_max_output_tokens: Some(4096),
        ..RequestOptions::default()
    };
    let value = serde_json::to_value(&options).unwrap();
    assert_eq!(value, json!({"provider_max_output_tokens": 4096}));
    assert_eq!(
        serde_json::from_value::<RequestOptions>(value).unwrap(),
        options
    );
}

/// 第一次取的查询：只有选择。
#[test]
fn a_new_query_has_only_the_selection() {
    let selection = ModelSelection::new("p".to_owned(), "m".to_owned());
    let query = EndpointQuery::new(&selection);
    assert_eq!(query.selection, &selection);
    assert!(query.rejected.is_none() && query.output_limit.is_none() && query.session.is_none());
}
