//! 单次超时的表（F58）：值与来源一一对应，改值要先改 README §4.1 的表。

use std::time::Duration;

use super::*;

#[test]
fn each_service_has_its_source_value() {
    assert_eq!(MODEL, Duration::from_secs(600));
    assert_eq!(ACCOUNT, Duration::from_secs(15));
    assert_eq!(OAUTH, Duration::from_secs(180));
    assert_eq!(NPM_REGISTRY, Duration::from_secs(5));
}
