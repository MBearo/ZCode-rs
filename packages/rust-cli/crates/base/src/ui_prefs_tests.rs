use super::*;

/// 只认 Node 的三个值（从 platform 的 `cli_config_tests.rs` 移来）。
#[test]
fn only_the_three_node_locales_parse() {
    for value in ["auto", "en-US", "zh-CN"] {
        assert_eq!(UiLocale::parse(value).unwrap().as_str(), value);
    }
    for value in ["zh", "zh_CN", "EN-us", ""] {
        assert_eq!(UiLocale::parse(value), None, "{value}");
    }
}

#[test]
fn only_the_three_node_themes_parse() {
    for value in ["auto", "dark", "light"] {
        assert_eq!(UiTheme::parse(value).unwrap().as_str(), value);
    }
    assert_eq!(UiTheme::parse("Dark"), None);
}
