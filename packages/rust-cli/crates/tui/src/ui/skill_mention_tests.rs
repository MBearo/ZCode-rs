//! `$` 提及的规则：开启的词、目录的折叠、排序、插入与发出的写法（照 Puffin `shell/mentions_tests.rs` 的情形）。
use super::*;

fn skill(name: &str, description: &str, from: From) -> MentionSkill {
    MentionSkill {
        name: name.to_owned(),
        description: description.to_owned(),
        path: String::new(),
        from,
    }
}

#[test]
fn the_word_opens_at_the_start_or_after_a_space_and_not_before_a_digit() {
    assert_eq!(token("$", 1), Some((0, String::new())));
    assert_eq!(token("use $re", 7), Some((4, "re".into())));
    assert_eq!(token("use ￥re", "use ￥re".len()), Some((4, "re".into())));
    assert_eq!(token("US$5", 4), None);
    assert_eq!(token("$5", 2), None);
    assert_eq!(token("$a$b", 4), None);
    assert_eq!(token("plain", 5), None);
}

#[test]
fn one_skill_per_name_plugin_first_sorted_by_name() {
    let got = catalog(vec![
        skill("Bench", "mine", From::User),
        skill("bench", "theirs", From::Plugin("q".into())),
        skill("alpha", "", From::Plugin("p".into())),
    ]);
    let names: Vec<(&str, &str)> = got
        .iter()
        .map(|k| (k.name.as_str(), k.description.as_str()))
        .collect();
    assert_eq!(names, [("alpha", ""), ("bench", "theirs")]);
}

#[test]
fn matches_rank_prefix_then_segments_then_contains_then_letters_then_description() {
    let all = catalog(vec![
        skill("dynamic-workflows", "", From::User),
        skill("systematic-debugging", "", From::User),
        skill("release-notes", "Write release notes", From::User),
        skill("triage", "read a crash log", From::User),
    ]);
    let names =
        |q: &str| -> Vec<String> { matches(&all, q).iter().map(|k| k.name.clone()).collect() };
    assert_eq!(names("dw"), ["dynamic-workflows"]);
    assert_eq!(names("dbg"), ["systematic-debugging"]);
    assert_eq!(names("re")[0], "release-notes");
    assert_eq!(names("crash"), ["triage"]);
    assert_eq!(names("").len(), 4);
}

#[test]
fn inserting_replaces_the_whole_word_and_adds_one_space() {
    assert_eq!(
        insert("$re", 3, 0, "release-notes"),
        ((0, 3), "$release-notes ".into(), (0, 15))
    );
    // 光标在词中间：到词尾都换掉；后面已有空白时不加。
    assert_eq!(
        insert("x $rel more", 5, 2, "release-notes"),
        ((2, 6), "$release-notes".into(), (2, 16))
    );
}

#[test]
fn only_skills_with_a_path_become_links() {
    let mut linked = skill("docx", "", From::User);
    linked.path = "/Users/dev/.zcode/skills/docx/SKILL.md".into();
    let kernel = skill("bench", "", From::User);
    let all = [linked, kernel];
    assert_eq!(
        to_wire("use $docx and $bench, not $other", &all),
        "use [$docx](/Users/dev/.zcode/skills/docx/SKILL.md) and $bench, not $other"
    );
    assert_eq!(ranges("$bench $x", &|n| n == "bench"), [(0, 6)]);
}
