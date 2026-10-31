//! 清单行与预算（ext.md §2.3）。

use rust_cli_base::models::GLM_5_3;
use std::sync::Arc;

use super::*;
use crate::skills::SkillSource;
use crate::skills::load::parse;

fn skill(name: &str, raw: &str) -> Arc<Skill> {
    Arc::new(parse(raw, name.to_owned(), "/s", SkillSource::Project))
}

fn lines(entries: &[ListingEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.line.as_str()).collect()
}

#[test]
fn k1_lines() {
    let skills = vec![
        skill(
            "greet",
            "---\ndescription: Greet someone by name\nwhen_to_use: When the user asks for a greeting\n---\nB\n",
        ),
        skill("plain", "# Plain skill\n\nNo frontmatter here.\n"),
    ];
    let found = entries(&skills, &skills, budget(Some(1_000_000), 4));
    assert_eq!(
        lines(&found),
        [
            "- greet: Greet someone by name - When the user asks for a greeting",
            "- plain: Plain skill"
        ]
    );
    assert_eq!(found[0].name, "greet");
}

#[test]
fn display_names_and_long_descriptions() {
    let skills = vec![
        skill("greet", "---\nname: Greeter\ndescription: Hi\n---\n"),
        skill("same", "---\nname: same\ndescription: Same\n---\n"),
        skill(
            "spaced",
            "---\nname: Two words\ndescription: Bad name\n---\n",
        ),
        skill(
            "long",
            &format!("---\ndescription: {}\n---\n", "d".repeat(2000)),
        ),
    ];
    let found = entries(&skills, &skills, budget(None, 4));
    assert_eq!(found[0].line, "- greet (Greeter): Hi");
    assert_eq!(found[1].line, "- same: Same");
    assert_eq!(found[2].line, "- spaced: Bad name");
    assert_eq!(found[3].line, format!("- long: {}…", "d".repeat(1535)));
}

/// 显示名的校验（修复线 F58）：长度按 UTF-16 码元，字符是字母、组合标记、数字与 `_:.-`
/// （`/^[\p{L}\p{M}\p{N}_:.-]+$/u`）。以前按码点计数、用 Rust 的 `is_alphanumeric`：组合标记（U+0301）算非法、Ⓐ
/// 算合法，200 个 𝒜（400 个码元）也算合法。
#[test]
fn display_names_follow_the_unicode_rule() {
    let skills = vec![
        skill("accent", "---\nname: Cafe\u{301}\ndescription: A\n---\n"),
        skill("circled", "---\nname: \u{24b6}\ndescription: B\n---\n"),
        skill(
            "wide",
            &format!(
                "---\nname: {}\ndescription: C\n---\n",
                "\u{1d49c}".repeat(200)
            ),
        ),
    ];
    let found = entries(&skills, &skills, budget(None, 4));
    assert_eq!(found[0].line, "- accent (Cafe\u{301}): A");
    assert_eq!(found[1].line, "- circled: B");
    assert_eq!(found[2].line, "- wide: C");
}

#[test]
fn over_budget_keeps_names_and_restores_descriptions_in_order() {
    let skills: Vec<_> = (0..3)
        .map(|index| {
            skill(
                &format!("s{index}"),
                &format!("---\ndescription: {}\n---\n", "x".repeat(40)),
            )
        })
        .collect();
    // 预算 = 1000 × 4 × 1% = 40 字符：只放得下名字（3 × 4 + 2 = 14），描述一条也放不回。
    let tight = entries(&skills, &skills, budget(Some(1_000), 4));
    assert_eq!(lines(&tight), ["- s0", "- s1", "- s2"]);
    // 预算 80：名字 14，剩 66，第一条描述（42 字符）放回，第二条放不下。
    let some = entries(&skills, &skills, budget(Some(2_000), 4));
    assert_eq!(
        lines(&some),
        [format!("- s0: {}", "x".repeat(40)).as_str(), "- s1", "- s2"]
    );
}

/// F10（context-usage.md D5）：预算按模型的每 token 字符数：不在旧模型清单里的模型是 3，窗口 1000000 时
/// 30000 个字符；旧模型与没有模型时是 4。
#[test]
fn the_budget_follows_the_models_chars_per_token() {
    let current = rust_cli_prompts::models::chars_per_token(GLM_5_3);
    assert_eq!(budget(Some(1_000_000), current), 30_000);
    assert_eq!(budget(Some(1_000_000), 4), 40_000);
    assert_eq!(
        budget(None, rust_cli_prompts::models::chars_per_token("")),
        8_000
    );
}
