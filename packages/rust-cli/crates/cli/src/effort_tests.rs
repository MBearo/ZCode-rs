use super::*;

fn selection(reasoning: Option<&str>) -> ModelSelection {
    ModelSelection {
        provider: "p".to_owned(),
        model: "m".to_owned(),
        reasoning: reasoning.map(str::to_owned),
        connection: None,
    }
}

fn levels(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// 五个档位去掉首尾空白、不分大小写；`med` 与 `ultracode` 是别名。
#[test]
fn levels_are_trimmed_and_case_insensitive() {
    for level in LEVELS {
        assert_eq!(parse(level), Ok(level));
    }
    assert_eq!(parse(" MAX "), Ok("max"));
    assert_eq!(parse("High"), Ok("high"));
    assert_eq!(parse("med"), Ok("medium"));
    assert_eq!(parse("ultracode"), Ok("xhigh"));
}

/// 不认识的值（含数字与空值）：警告的原文带原样的值，不报错。
#[test]
fn unknown_values_warn_with_the_fixed_text() {
    assert_eq!(
        parse("Turbo"),
        Err("Warning: Unknown --effort value 'Turbo' \u{2014} ignoring it and using the default effort. Valid values: low, medium, high, xhigh, max.".to_owned())
    );
    assert!(parse("5").is_err());
    assert!(parse("").is_err());
    assert!(parse("medium-ish").is_err());
}

/// 显式覆盖（没有模型目录）：原样写进选择，覆盖原来的档位。
#[test]
fn without_a_catalog_the_level_is_written_as_is() {
    let mut chosen = selection(Some("medium"));
    apply(&mut chosen, "max", None);
    assert_eq!(chosen.reasoning.as_deref(), Some("max"));
}

/// 有目录：同名的档位用目录里的写法；xhigh / max 没有时退到 high；仍没有时不改。
#[test]
fn with_a_catalog_only_offered_levels_are_used() {
    let offered = levels(&["Low", "high"]);
    let mut chosen = selection(Some("high"));
    apply(&mut chosen, "low", Some(&offered));
    assert_eq!(chosen.reasoning.as_deref(), Some("Low"));
    let mut chosen = selection(Some("Low"));
    apply(&mut chosen, "max", Some(&offered));
    assert_eq!(chosen.reasoning.as_deref(), Some("high"));
    apply(&mut chosen, "xhigh", Some(&offered));
    assert_eq!(chosen.reasoning.as_deref(), Some("high"));
    // medium 不在目录里：保持缺省档位。
    let mut chosen = selection(Some("Low"));
    apply(&mut chosen, "medium", Some(&offered));
    assert_eq!(chosen.reasoning.as_deref(), Some("Low"));
    // 档位不是这五档的模型（例如 disabled / enabled）：都不改。
    let other = levels(&["disabled", "enabled"]);
    let mut chosen = selection(Some("enabled"));
    apply(&mut chosen, "max", Some(&other));
    assert_eq!(chosen.reasoning.as_deref(), Some("enabled"));
}
